//! OpenAI backend implementation.
//!
//! Uses the OpenAI API for command generation with GPT models.

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";

/// OpenAI backend for GPT API.
pub struct OpenAIBackend {
    pub model: String,
    api_key: Option<String>,
    client: Client,
}

impl OpenAIBackend {
    /// Create a new OpenAI backend.
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
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| {
                anyhow!(
                    "OpenAI API key not found. Set OPENAI_API_KEY environment variable \
                     or add api_key to config file."
                )
            })
    }

    /// Generate a command from a query and system prompt.
    pub async fn generate(&self, system_prompt: &str, user_query: &str) -> Result<String> {
        let api_key = self.get_api_key()?;

        let request = OpenAIRequest {
            model: self.model.clone(),
            messages: vec![
                OpenAIMessage {
                    role: "system".to_string(),
                    content: system_prompt.to_string(),
                },
                OpenAIMessage {
                    role: "user".to_string(),
                    content: user_query.to_string(),
                },
            ],
            max_tokens: 200,
            temperature: 0.1,
        };

        let response = self
            .client
            .post(OPENAI_API_URL)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to connect to OpenAI API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body: Result<OpenAIError, _> = response.json().await;
            let message = body
                .map(|e| e.error.message)
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow!(
                "OpenAI API request failed with status {}: {}",
                status,
                message
            ));
        }

        let openai_response: OpenAIResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI response")?;

        let command = openai_response
            .choices
            .first()
            .map(|c| c.message.content.trim().to_string())
            .ok_or_else(|| anyhow!("Empty response from OpenAI"))?;

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
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    max_tokens: u32,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct OpenAIMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessageResponse,
}

#[derive(Debug, Deserialize)]
struct OpenAIMessageResponse {
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIError {
    error: OpenAIErrorDetail,
}

#[derive(Debug, Deserialize)]
struct OpenAIErrorDetail {
    message: String,
}
