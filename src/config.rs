//! Configuration management for incant.
//!
//! Configuration is loaded from `~/.config/incant/config.toml`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Main configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Backend configuration.
    #[serde(default)]
    pub backend: BackendConfig,
    /// Named profiles for quick model switching.
    #[serde(default)]
    pub profiles: HashMap<String, Profile>,
    /// User preferences.
    #[serde(default)]
    pub preferences: Preferences,
}

impl Default for Config {
    fn default() -> Self {
        let mut profiles = HashMap::new();
        profiles.insert(
            "default".to_string(),
            Profile {
                model: "qwen2.5-coder:7b".to_string(),
                temperature: Some(0.1),
            },
        );
        profiles.insert(
            "fast".to_string(),
            Profile {
                model: "qwen2.5-coder:1.5b".to_string(),
                temperature: Some(0.1),
            },
        );
        profiles.insert(
            "heavy".to_string(),
            Profile {
                model: "qwen2.5-coder:32b".to_string(),
                temperature: Some(0.1),
            },
        );

        Self {
            backend: BackendConfig::default(),
            profiles,
            preferences: Preferences::default(),
        }
    }
}

/// A named profile with model and temperature settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    /// The model to use.
    pub model: String,
    /// Temperature for generation (0.0-1.0).
    #[serde(default)]
    pub temperature: Option<f32>,
}

/// Backend configuration for LLM providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BackendConfig {
    /// Ollama local backend.
    Ollama {
        /// Ollama host URL (default: http://localhost:11434).
        #[serde(default = "default_ollama_host")]
        host: String,
        /// Default profile name (default: "default").
        #[serde(default = "default_profile_name")]
        default_profile: String,
    },
    /// Anthropic Claude API.
    Anthropic {
        /// Default profile name (default: "default").
        #[serde(default = "default_profile_name")]
        default_profile: String,
        /// API key (prefer ANTHROPIC_API_KEY env var).
        #[serde(default)]
        api_key: Option<String>,
    },
    /// OpenAI API.
    OpenAI {
        /// Default profile name (default: "default").
        #[serde(default = "default_profile_name")]
        default_profile: String,
        /// API key (prefer OPENAI_API_KEY env var).
        #[serde(default)]
        api_key: Option<String>,
    },
}

impl Default for BackendConfig {
    fn default() -> Self {
        BackendConfig::Ollama {
            host: default_ollama_host(),
            default_profile: default_profile_name(),
        }
    }
}

fn default_ollama_host() -> String {
    "http://localhost:11434".to_string()
}

fn default_profile_name() -> String {
    "default".to_string()
}

/// User preferences for command generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    /// Prefer modern tools (rg/fd/bat over grep/find/cat).
    #[serde(default = "default_true")]
    pub modern_tools: bool,
    /// Prefer verbose flags (--recursive over -r).
    #[serde(default = "default_true")]
    pub verbose_flags: bool,
    /// Show advisory warnings when a generated command looks destructive.
    #[serde(default = "default_true")]
    pub safety_warnings: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            modern_tools: true,
            verbose_flags: true,
            safety_warnings: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Resolve the config directory from explicit inputs (pure, testable).
///
/// `$XDG_CONFIG_HOME/incant` when set and non-empty, else
/// `<home>/.config/incant`.
fn config_dir_from(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(xdg) = xdg_config_home {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("incant"));
        }
    }
    home.map(|p| p.join(".config/incant"))
        .context("Could not determine home directory")
}

/// Model selection options from CLI.
#[derive(Debug, Clone, Default)]
pub struct ModelSelection {
    /// Explicit model override (highest priority).
    pub model: Option<String>,
    /// Profile name to use.
    pub profile: Option<String>,
    /// Use the "fast" profile alias.
    pub fast: bool,
}

impl ModelSelection {
    /// Resolve the model to use based on priority:
    /// --model > --profile/--fast > default_profile > hardcoded fallback
    pub fn resolve_model(&self, config: &Config) -> String {
        // Highest priority: explicit --model
        if let Some(model) = &self.model {
            return model.clone();
        }

        // Next: --profile or --fast alias
        let profile_name = if self.fast {
            Some("fast".to_string())
        } else {
            self.profile.clone()
        };

        if let Some(name) = profile_name {
            if let Some(profile) = config.profiles.get(&name) {
                return profile.model.clone();
            }
        }

        // Next: default_profile from backend config
        let default_profile = config.default_profile();
        if let Some(profile) = config.profiles.get(default_profile) {
            return profile.model.clone();
        }

        // Fallback: hardcoded default based on backend type
        config.fallback_model().to_string()
    }

    /// Resolve the temperature to use.
    pub fn resolve_temperature(&self, config: &Config) -> f32 {
        // If explicit model, use default temperature
        if self.model.is_some() {
            return 0.1;
        }

        // Check profile
        let profile_name = if self.fast {
            Some("fast".to_string())
        } else {
            self.profile.clone()
        };

        if let Some(name) = profile_name {
            if let Some(profile) = config.profiles.get(&name) {
                return profile.temperature.unwrap_or(0.1);
            }
        }

        // Default profile
        let default_profile = config.default_profile();
        if let Some(profile) = config.profiles.get(default_profile) {
            return profile.temperature.unwrap_or(0.1);
        }

        0.1
    }
}

impl Config {
    /// Get the config directory path.
    ///
    /// XDG semantics on every Unix platform: `$XDG_CONFIG_HOME/incant`,
    /// falling back to `~/.config/incant`. CLI tools live in `~/.config`
    /// on macOS too — this is what every doc for this project promises
    /// (previously `dirs::config_dir()` silently resolved to
    /// `~/Library/Application Support` on macOS, so documented configs
    /// were ignored; see issue #22).
    pub fn config_dir() -> Result<PathBuf> {
        config_dir_from(std::env::var_os("XDG_CONFIG_HOME"), dirs::home_dir())
    }

    /// Get the config file path.
    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    /// Get the per-user runtime directory holding the socket, PID, and
    /// startup-status files: `$XDG_RUNTIME_DIR` (already private per spec)
    /// or `~/.local/run` as the fallback.
    pub fn runtime_dir() -> Result<PathBuf> {
        if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            Ok(PathBuf::from(runtime_dir))
        } else {
            dirs::home_dir()
                .map(|p| p.join(".local/run"))
                .context("Could not determine home directory")
        }
    }

    /// Get the socket path for daemon communication.
    pub fn socket_path() -> Result<PathBuf> {
        Ok(Self::runtime_dir()?.join("incant.sock"))
    }

    /// Get the PID file path for the daemon.
    pub fn pid_path() -> Result<PathBuf> {
        Ok(Self::runtime_dir()?.join("incant.pid"))
    }

    /// Get the startup status file path for daemon startup reporting.
    pub fn startup_status_path() -> Result<PathBuf> {
        Ok(Self::runtime_dir()?.join("incant.startup"))
    }

    /// Load configuration from file, using defaults if not found.
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if path.exists() {
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config file: {}", path.display()))?;
            toml::from_str(&contents)
                .with_context(|| format!("Failed to parse config file: {}", path.display()))
        } else {
            Ok(Self::default())
        }
    }

    /// Save configuration to file.
    ///
    /// The config may contain API keys, so the directory is created 0700
    /// and the file is written 0600.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::config_path()?)
    }

    /// Save configuration to an explicit path (separated for testability).
    fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        let contents = toml::to_string_pretty(self)?;
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("Failed to write config file: {}", path.display()))?;
            // An existing file keeps its old mode; enforce 0600 regardless.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            file.write_all(contents.as_bytes())?;
        }
        #[cfg(not(unix))]
        std::fs::write(path, &contents)
            .with_context(|| format!("Failed to write config file: {}", path.display()))?;
        Ok(())
    }

    /// Get the backend type as a string.
    pub fn backend_type(&self) -> &'static str {
        match &self.backend {
            BackendConfig::Ollama { .. } => "ollama",
            BackendConfig::Anthropic { .. } => "anthropic",
            BackendConfig::OpenAI { .. } => "openai",
        }
    }

    /// Get the default profile name from backend config.
    pub fn default_profile(&self) -> &str {
        match &self.backend {
            BackendConfig::Ollama {
                default_profile, ..
            } => default_profile,
            BackendConfig::Anthropic {
                default_profile, ..
            } => default_profile,
            BackendConfig::OpenAI {
                default_profile, ..
            } => default_profile,
        }
    }

    /// Get the fallback model for this backend type.
    pub fn fallback_model(&self) -> &'static str {
        match &self.backend {
            BackendConfig::Ollama { .. } => "qwen2.5-coder:7b",
            BackendConfig::Anthropic { .. } => "claude-3-5-haiku-latest",
            BackendConfig::OpenAI { .. } => "gpt-4o-mini",
        }
    }

    /// Get the model name (from default profile or fallback).
    pub fn model_name(&self) -> String {
        let default_profile = self.default_profile();
        if let Some(profile) = self.profiles.get(default_profile) {
            profile.model.clone()
        } else {
            self.fallback_model().to_string()
        }
    }

    /// Build the system prompt based on context and preferences.
    pub fn build_system_prompt(&self, context: &crate::protocol::Context) -> String {
        let modern_tools_note = if !self.preferences.modern_tools {
            "- Use standard POSIX tools (grep, find, cat)".to_string()
        } else if context.tools.is_empty() {
            "- Use modern tools when appropriate (ripgrep over grep, fd over find, bat over cat)"
                .to_string()
        } else {
            format!(
                "- Prefer these installed modern tools over classic equivalents: {}",
                context.tools.join(", ")
            )
        };

        let flags_note = if self.preferences.verbose_flags {
            "- Prefer long flags for clarity (--recursive over -r) unless brevity is clearly preferred"
        } else {
            "- Use short flags for brevity (-r over --recursive)"
        };

        let distro_info = context
            .distro
            .as_ref()
            .map(|d| format!("\nDistro: {}", d))
            .unwrap_or_default();

        let mut extra = String::new();
        if !context.projects.is_empty() {
            extra.push_str(&format!("\nProject: {}", context.projects.join(", ")));
        }
        if let Some(git) = &context.git {
            extra.push_str(&format!("\nGit: {}", git));
        }
        if !context.env_flags.is_empty() {
            extra.push_str(&format!("\nEnvironment: {}", context.env_flags.join(", ")));
        }

        format!(
            r#"You are a shell command generator. Your ONLY output is the exact command to run.

Rules:
- Output ONLY the command, nothing else
- No markdown, no backticks, no explanations
- No preamble like "Here's the command:"
- If multiple commands needed, separate with && or ;
- Make reasonable assumptions for ambiguous requests
{}
{}

Context:
OS: {}{}
Shell: {}
CWD: {}{}"#,
            modern_tools_note,
            flags_note,
            context.os,
            distro_info,
            context.shell,
            context.cwd.display(),
            extra
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_prefers_xdg_config_home() {
        let dir = config_dir_from(
            Some("/custom/xdg".into()),
            Some(PathBuf::from("/home/user")),
        )
        .unwrap();
        assert_eq!(dir, PathBuf::from("/custom/xdg/incant"));
    }

    #[test]
    fn config_dir_falls_back_to_dot_config() {
        let dir = config_dir_from(None, Some(PathBuf::from("/home/user"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/user/.config/incant"));
    }

    #[test]
    fn config_dir_ignores_empty_xdg() {
        let dir = config_dir_from(Some("".into()), Some(PathBuf::from("/home/user"))).unwrap();
        assert_eq!(dir, PathBuf::from("/home/user/.config/incant"));
    }

    #[test]
    #[cfg(unix)]
    fn save_writes_config_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cfgdir").join("config.toml");
        Config::default().save_to(&path).unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);

        // Re-saving over a file with loosened permissions re-tightens it.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        Config::default().save_to(&path).unwrap();
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
    }

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(matches!(config.backend, BackendConfig::Ollama { .. }));
        assert!(config.preferences.modern_tools);
        assert!(config.profiles.contains_key("default"));
        assert!(config.profiles.contains_key("fast"));
    }

    fn ctx(projects: Vec<&str>, tools: Vec<&str>, git: Option<&str>) -> crate::protocol::Context {
        crate::protocol::Context {
            cwd: "/work/demo".into(),
            shell: "/bin/zsh".to_string(),
            os: "Darwin 25.3.0".to_string(),
            distro: None,
            projects: projects.into_iter().map(String::from).collect(),
            tools: tools.into_iter().map(String::from).collect(),
            git: git.map(String::from),
            env_flags: Vec::new(),
        }
    }

    #[test]
    fn prompt_renders_enriched_context() {
        let config = Config::default();
        let prompt = config.build_system_prompt(&ctx(
            vec!["rust"],
            vec!["rg", "fd"],
            Some("branch main, dirty"),
        ));
        assert!(prompt.contains("Project: rust"));
        assert!(prompt.contains("Git: branch main, dirty"));
        assert!(prompt.contains("installed modern tools over classic equivalents: rg, fd"));
    }

    #[test]
    fn prompt_omits_absent_context_sections() {
        let config = Config::default();
        let prompt = config.build_system_prompt(&ctx(vec![], vec![], None));
        assert!(!prompt.contains("Project:"));
        assert!(!prompt.contains("Git:"));
        // Without a tool probe result, fall back to generic advice.
        assert!(prompt.contains("Use modern tools when appropriate"));
    }

    #[test]
    fn prompt_respects_posix_preference_over_probe() {
        let mut config = Config::default();
        config.preferences.modern_tools = false;
        let prompt = config.build_system_prompt(&ctx(vec![], vec!["rg"], None));
        assert!(prompt.contains("standard POSIX tools"));
        assert!(!prompt.contains("installed modern tools"));
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml = toml::to_string_pretty(&config).unwrap();
        assert!(toml.contains("ollama"));
        assert!(toml.contains("profiles"));
    }

    #[test]
    fn test_config_deserialization() {
        let toml = r#"
[backend]
type = "anthropic"
default_profile = "default"

[profiles.default]
model = "claude-3-5-haiku-latest"
temperature = 0.1

[preferences]
modern_tools = false
verbose_flags = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(matches!(config.backend, BackendConfig::Anthropic { .. }));
        assert!(!config.preferences.modern_tools);
    }

    #[test]
    fn test_model_selection_explicit_model() {
        let config = Config::default();
        let selection = ModelSelection {
            model: Some("custom-model:latest".to_string()),
            profile: None,
            fast: false,
        };
        assert_eq!(selection.resolve_model(&config), "custom-model:latest");
    }

    #[test]
    fn test_model_selection_fast() {
        let config = Config::default();
        let selection = ModelSelection {
            model: None,
            profile: None,
            fast: true,
        };
        assert_eq!(selection.resolve_model(&config), "qwen2.5-coder:1.5b");
    }

    #[test]
    fn test_model_selection_profile() {
        let config = Config::default();
        let selection = ModelSelection {
            model: None,
            profile: Some("heavy".to_string()),
            fast: false,
        };
        assert_eq!(selection.resolve_model(&config), "qwen2.5-coder:32b");
    }

    #[test]
    fn test_model_selection_default() {
        let config = Config::default();
        let selection = ModelSelection::default();
        assert_eq!(selection.resolve_model(&config), "qwen2.5-coder:7b");
    }
}
