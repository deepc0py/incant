//! Ollama backend implementation.
//!
//! Ollama is a local LLM server that provides fast inference without API costs.

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Ollama backend for local LLM inference.
pub struct OllamaBackend {
    pub model: String,
    host: String,
    client: Client,
}

impl OllamaBackend {
    /// Create a new Ollama backend.
    pub fn new(model: String, host: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            model,
            host,
            client,
        }
    }

    /// Generate a command from a query and system prompt.
    pub async fn generate(&self, system_prompt: &str, user_query: &str) -> Result<String> {
        let url = format!("{}/api/generate", self.host);

        let request = OllamaRequest {
            model: self.model.clone(),
            prompt: user_query.to_string(),
            system: system_prompt.to_string(),
            stream: false,
            options: OllamaOptions {
                temperature: 0.1, // Low temperature for deterministic commands
                num_predict: 200, // Limit output length
            },
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to connect to Ollama")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Ollama request failed with status {}: {}",
                status,
                body
            ));
        }

        let ollama_response: OllamaResponse = response
            .json()
            .await
            .context("Failed to parse Ollama response")?;

        // Clean up the response - remove any markdown/backticks that might slip through
        let command = clean_command(&ollama_response.response);
        Ok(command)
    }

    /// Check if the backend is available/reachable.
    pub async fn health_check(&self) -> Result<()> {
        let url = format!("{}/api/tags", self.host);
        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .context("Failed to connect to Ollama - is it running?")?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("Ollama health check failed: {}", response.status()))
        }
    }
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
    system: String,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f32,
    num_predict: i32,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    response: String,
    #[allow(dead_code)]
    done: bool,
}

/// Clean up the generated command.
fn clean_command(response: &str) -> String {
    let mut command = response.trim().to_string();

    // Remove markdown code blocks if present
    if command.starts_with("```") {
        // Find the end of the first line (language specifier)
        if let Some(first_newline) = command.find('\n') {
            command = command[first_newline + 1..].to_string();
        }
        // Remove trailing ```
        if let Some(end) = command.rfind("```") {
            command = command[..end].to_string();
        }
    }

    // Remove single backticks
    command = command.trim_matches('`').to_string();

    // Remove common preambles
    let preambles = [
        "Here's the command:",
        "Here is the command:",
        "The command is:",
        "Run:",
        "Execute:",
        "Command:",
    ];
    for preamble in preambles {
        if let Some(stripped) = command.strip_prefix(preamble) {
            command = stripped.to_string();
        }
    }

    command.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_command_plain() {
        assert_eq!(clean_command("ls -la"), "ls -la");
    }

    #[test]
    fn test_clean_command_with_backticks() {
        assert_eq!(clean_command("`ls -la`"), "ls -la");
    }

    #[test]
    fn test_clean_command_with_code_block() {
        assert_eq!(clean_command("```bash\nls -la\n```"), "ls -la");
    }

    #[test]
    fn test_clean_command_with_preamble() {
        assert_eq!(clean_command("Here's the command: ls -la"), "ls -la");
    }
}
