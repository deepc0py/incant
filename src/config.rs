//! Configuration management for llmcmd.
//!
//! Configuration is loaded from `~/.config/llmcmd/config.toml`.

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
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            modern_tools: true,
            verbose_flags: true,
        }
    }
}

fn default_true() -> bool {
    true
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
    pub fn config_dir() -> Result<PathBuf> {
        dirs::config_dir()
            .map(|p| p.join("llmcmd"))
            .context("Could not determine config directory")
    }

    /// Get the config file path.
    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.toml"))
    }

    /// Get the socket path for daemon communication.
    pub fn socket_path() -> Result<PathBuf> {
        // Prefer XDG_RUNTIME_DIR, fall back to ~/.local/run/llmcmd
        if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            Ok(PathBuf::from(runtime_dir).join("llmcmd.sock"))
        } else {
            dirs::home_dir()
                .map(|p| p.join(".local/run/llmcmd.sock"))
                .context("Could not determine home directory")
        }
    }

    /// Get the PID file path for the daemon.
    pub fn pid_path() -> Result<PathBuf> {
        if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
            Ok(PathBuf::from(runtime_dir).join("llmcmd.pid"))
        } else {
            dirs::home_dir()
                .map(|p| p.join(".local/run/llmcmd.pid"))
                .context("Could not determine home directory")
        }
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
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)
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
            BackendConfig::Ollama { default_profile, .. } => default_profile,
            BackendConfig::Anthropic { default_profile, .. } => default_profile,
            BackendConfig::OpenAI { default_profile, .. } => default_profile,
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

    /// Get the Ollama host URL.
    pub fn ollama_host(&self) -> Option<&str> {
        match &self.backend {
            BackendConfig::Ollama { host, .. } => Some(host),
            _ => None,
        }
    }

    /// Get the API key for cloud backends.
    pub fn api_key(&self) -> Option<&str> {
        match &self.backend {
            BackendConfig::Anthropic { api_key, .. } => api_key.as_deref(),
            BackendConfig::OpenAI { api_key, .. } => api_key.as_deref(),
            _ => None,
        }
    }

    /// Get all profile names.
    pub fn profile_names(&self) -> Vec<&String> {
        self.profiles.keys().collect()
    }

    /// Build the system prompt based on context and preferences.
    pub fn build_system_prompt(&self, context: &crate::protocol::Context) -> String {
        let modern_tools_note = if self.preferences.modern_tools {
            "- Use modern tools when appropriate (ripgrep over grep, fd over find, bat over cat)"
        } else {
            "- Use standard POSIX tools (grep, find, cat)"
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
CWD: {}"#,
            modern_tools_note,
            flags_note,
            context.os,
            distro_info,
            context.shell,
            context.cwd.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert!(matches!(config.backend, BackendConfig::Ollama { .. }));
        assert!(config.preferences.modern_tools);
        assert!(config.profiles.contains_key("default"));
        assert!(config.profiles.contains_key("fast"));
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
