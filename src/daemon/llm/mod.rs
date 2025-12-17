//! LLM backend implementations.
//!
//! This module provides a unified interface for different LLM backends
//! including Ollama (local) and cloud providers (Anthropic, OpenAI).

pub mod anthropic;
pub mod ollama;
pub mod openai;

use anyhow::Result;

/// Enum-based backend for LLM providers.
/// Using an enum instead of trait objects for better performance and simplicity.
pub enum Backend {
    Ollama(ollama::OllamaBackend),
    Anthropic(anthropic::AnthropicBackend),
    OpenAI(openai::OpenAIBackend),
}

impl Backend {
    /// Generate a command from a query and system prompt.
    /// Optionally override the model and temperature for this request.
    pub async fn generate(
        &self,
        system_prompt: &str,
        user_query: &str,
        model_override: Option<&str>,
        temperature_override: Option<f32>,
    ) -> Result<String> {
        match self {
            Backend::Ollama(b) => {
                b.generate(system_prompt, user_query, model_override, temperature_override)
                    .await
            }
            Backend::Anthropic(b) => {
                b.generate(system_prompt, user_query, model_override, temperature_override)
                    .await
            }
            Backend::OpenAI(b) => {
                b.generate(system_prompt, user_query, model_override, temperature_override)
                    .await
            }
        }
    }

    /// Get the backend name.
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Ollama(_) => "ollama",
            Backend::Anthropic(_) => "anthropic",
            Backend::OpenAI(_) => "openai",
        }
    }

    /// Get the model name.
    pub fn model(&self) -> &str {
        match self {
            Backend::Ollama(b) => &b.model,
            Backend::Anthropic(b) => &b.model,
            Backend::OpenAI(b) => &b.model,
        }
    }

    /// Check if the backend is available/reachable.
    pub async fn health_check(&self) -> Result<()> {
        match self {
            Backend::Ollama(b) => b.health_check().await,
            Backend::Anthropic(b) => b.health_check().await,
            Backend::OpenAI(b) => b.health_check().await,
        }
    }
}

/// Create a backend from configuration.
/// The model is resolved from the default profile in the config.
pub fn create_backend(config: &crate::config::Config) -> Backend {
    let model = config.model_name();

    match &config.backend {
        crate::config::BackendConfig::Ollama { host, .. } => {
            Backend::Ollama(ollama::OllamaBackend::new(model, host.clone()))
        }
        crate::config::BackendConfig::Anthropic { api_key, .. } => {
            Backend::Anthropic(anthropic::AnthropicBackend::new(model, api_key.clone()))
        }
        crate::config::BackendConfig::OpenAI { api_key, .. } => {
            Backend::OpenAI(openai::OpenAIBackend::new(model, api_key.clone()))
        }
    }
}
