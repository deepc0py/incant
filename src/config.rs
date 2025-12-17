//! Configuration management for llmcmd.
//!
//! Configuration is loaded from `~/.config/llmcmd/config.toml`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main configuration structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Backend configuration.
    #[serde(default)]
    pub backend: BackendConfig,
    /// User preferences.
    #[serde(default)]
    pub preferences: Preferences,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: BackendConfig::default(),
            preferences: Preferences::default(),
        }
    }
}

/// Backend configuration for LLM providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BackendConfig {
    /// Ollama local backend.
    Ollama {
        /// Model name (default: qwen2.5-coder:7b).
        #[serde(default = "default_ollama_model")]
        model: String,
        /// Ollama host URL (default: http://localhost:11434).
        #[serde(default = "default_ollama_host")]
        host: String,
    },
    /// Anthropic Claude API.
    Anthropic {
        /// Model name (default: claude-3-5-haiku-latest).
        #[serde(default = "default_anthropic_model")]
        model: String,
        /// API key (prefer ANTHROPIC_API_KEY env var).
        #[serde(default)]
        api_key: Option<String>,
    },
    /// OpenAI API.
    OpenAI {
        /// Model name (default: gpt-4o-mini).
        #[serde(default = "default_openai_model")]
        model: String,
        /// API key (prefer OPENAI_API_KEY env var).
        #[serde(default)]
        api_key: Option<String>,
    },
}

impl Default for BackendConfig {
    fn default() -> Self {
        BackendConfig::Ollama {
            model: default_ollama_model(),
            host: default_ollama_host(),
        }
    }
}

fn default_ollama_model() -> String {
    "qwen2.5-coder:7b".to_string()
}

fn default_ollama_host() -> String {
    "http://localhost:11434".to_string()
}

fn default_anthropic_model() -> String {
    "claude-3-5-haiku-latest".to_string()
}

fn default_openai_model() -> String {
    "gpt-4o-mini".to_string()
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

    /// Get the model name.
    pub fn model_name(&self) -> &str {
        match &self.backend {
            BackendConfig::Ollama { model, .. } => model,
            BackendConfig::Anthropic { model, .. } => model,
            BackendConfig::OpenAI { model, .. } => model,
        }
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
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml = toml::to_string_pretty(&config).unwrap();
        assert!(toml.contains("ollama"));
    }

    #[test]
    fn test_config_deserialization() {
        let toml = r#"
[backend]
type = "anthropic"
model = "claude-3-5-haiku-latest"

[preferences]
modern_tools = false
verbose_flags = true
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(matches!(config.backend, BackendConfig::Anthropic { .. }));
        assert!(!config.preferences.modern_tools);
    }
}
