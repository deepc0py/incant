//! Anthropic Claude backend implementation.
//!
//! Uses the Anthropic API for command generation with Claude models.

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic backend for Claude API.
pub struct AnthropicBackend {
    pub model: String,
    api_key: Option<String>,
    client: Client,
}

impl AnthropicBackend {
    /// Create a new Anthropic backend.
    pub fn new(model: String, api_key: Option<String>) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            model,
            api_key,
            client,
        }
    }

    /// Get the API key from config or environment.
    fn get_api_key(&self) -> Result<String> {
        self.api_key
            .clone()
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
            .ok_or_else(|| {
                anyhow!(
                    "Anthropic API key not found. Set ANTHROPIC_API_KEY environment variable \
                     or add api_key to config file."
                )
            })
    }

    /// Generate a command from a query and system prompt.
    /// Optionally override the model and temperature for this request.
    pub async fn generate(
        &self,
        system_prompt: &str,
        user_query: &str,
        model_override: Option<&str>,
        temperature_override: Option<f32>,
    ) -> Result<String> {
        let api_key = self.get_api_key()?;

        let model = model_override.unwrap_or(&self.model);
        let temperature = temperature_override.unwrap_or(0.1);

        let request = AnthropicRequest {
            model: model.to_string(),
            max_tokens: 200,
            system: system_prompt.to_string(),
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: user_query.to_string(),
            }],
            temperature,
        };

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .header("x-api-key", &api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to connect to Anthropic API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body: Result<AnthropicError, _> = response.json().await;
            let message = body
                .map(|e| e.error.message)
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!(
                "Anthropic API request failed with status {}: {}",
                status,
                message
            ));
        }

        let anthropic_response: AnthropicResponse = response
            .json()
            .await
            .context("Failed to parse Anthropic response")?;

        let command = anthropic_response
            .content
            .first()
            .map(|c| c.text.trim().to_string())
            .ok_or_else(|| anyhow!("Empty response from Anthropic"))?;

        Ok(command)
    }

    /// Check if the backend is available/reachable.
    pub async fn health_check(&self) -> Result<()> {
        // Just verify we have an API key
        self.get_api_key()?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMessage>,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    #[allow(dead_code)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicContent {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    content_type: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicError {
    error: AnthropicErrorDetail,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetail {
    message: String,
}
