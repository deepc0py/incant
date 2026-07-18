//! IPC protocol definitions for client-daemon communication.
//!
//! The protocol uses JSON over Unix domain sockets for simplicity and debuggability.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Request sent from client to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// The natural language query from the user.
    pub query: String,
    /// System context for better command generation.
    pub context: Context,
    /// Model override (if specified by client).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Temperature override (if specified by client).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Request a short explanation of the generated command.
    #[serde(default)]
    pub explain: bool,
}

/// System context gathered by the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    /// Current working directory.
    pub cwd: PathBuf,
    /// User's shell (from $SHELL).
    pub shell: String,
    /// Operating system info (uname -a output).
    pub os: String,
    /// Linux distribution info (from /etc/os-release).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distro: Option<String>,
    /// Detected project types in cwd (e.g. "rust", "node").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projects: Vec<String>,
    /// Modern CLI tools available on PATH (e.g. "rg", "fd", "jq").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Git state of cwd, e.g. "branch main, dirty". None outside a repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,
    /// Environment flags: "ssh", "tmux", "docker".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_flags: Vec<String>,
}

/// Response sent from daemon to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// The generated command, if successful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Error message, if the request failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Advisory safety assessment of the generated command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<crate::safety::Assessment>,
    /// Short explanation of the command (present when requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}

impl Response {
    /// Create a successful response with a command and its safety assessment.
    pub fn success(command: String, risk: crate::safety::Assessment) -> Self {
        Self {
            command: Some(command),
            error: None,
            risk: Some(risk),
            explanation: None,
        }
    }

    /// Attach an explanation to a successful response.
    pub fn with_explanation(mut self, explanation: String) -> Self {
        self.explanation = Some(explanation);
        self
    }

    /// Create a response carrying informational text (e.g. daemon status)
    /// that is not a generated command, so no risk assessment applies.
    pub fn plain(text: String) -> Self {
        Self {
            command: Some(text),
            error: None,
            risk: None,
            explanation: None,
        }
    }

    /// Create an error response.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            command: None,
            error: Some(message.into()),
            risk: None,
            explanation: None,
        }
    }
}

/// Message type for IPC communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// A query request from the client.
    Query(Box<Request>),
    /// Request daemon status.
    Status,
    /// Shutdown the daemon gracefully.
    Shutdown,
}

/// Framing for messages: length-prefixed JSON.
/// Format: 4 bytes (big-endian u32) length + JSON payload
pub mod framing {
    use anyhow::{anyhow, Result};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Write a length-prefixed message.
    pub async fn write_message<W, T>(writer: &mut W, message: &T) -> Result<()>
    where
        W: AsyncWriteExt + Unpin,
        T: serde::Serialize,
    {
        let json = serde_json::to_vec(message)?;
        let len = json.len() as u32;
        writer.write_all(&len.to_be_bytes()).await?;
        writer.write_all(&json).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Read a length-prefixed message.
    pub async fn read_message<R, T>(reader: &mut R) -> Result<T>
    where
        R: AsyncReadExt + Unpin,
        T: serde::de::DeserializeOwned,
    {
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf) as usize;

        // Sanity check: max 1MB message
        if len > 1_000_000 {
            return Err(anyhow!("Message too large: {} bytes", len));
        }

        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await?;
        let message = serde_json::from_slice(&buf)?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_response_success() {
        let resp = Response::success("ls -la".to_string(), crate::safety::assess("ls -la"));
        assert_eq!(resp.command, Some("ls -la".to_string()));
        assert!(resp.error.is_none());
        assert!(resp.risk.as_ref().is_some_and(|r| r.is_safe()));
    }

    #[test]
    fn test_response_plain_has_no_risk() {
        let resp = Response::plain("Backend: ollama (qwen)".to_string());
        assert!(resp.command.is_some());
        assert!(resp.risk.is_none());
    }

    #[test]
    fn test_response_risk_roundtrips_through_json() {
        let resp = Response::success("rm -rf /".to_string(), crate::safety::assess("rm -rf /"));
        let json = serde_json::to_vec(&resp).unwrap();
        let back: Response = serde_json::from_slice(&json).unwrap();
        let risk = back.risk.expect("risk must survive serialization");
        assert_eq!(risk.level, crate::safety::RiskLevel::Destructive);
    }

    #[test]
    fn test_response_error() {
        let resp = Response::error("Connection failed");
        assert!(resp.command.is_none());
        assert_eq!(resp.error, Some("Connection failed".to_string()));
    }

    #[test]
    fn test_request_serialization() {
        let req = Request {
            query: "list files".to_string(),
            context: Context {
                cwd: "/home/user".into(),
                shell: "/bin/zsh".to_string(),
                os: "Linux 5.15.0".to_string(),
                distro: Some("Ubuntu 22.04".to_string()),
                projects: Vec::new(),
                tools: Vec::new(),
                git: None,
                env_flags: Vec::new(),
            },
            model: None,
            temperature: None,
            explain: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.query, req.query);
    }

    #[test]
    fn test_request_with_model_override() {
        let req = Request {
            query: "list files".to_string(),
            context: Context {
                cwd: "/home/user".into(),
                shell: "/bin/zsh".to_string(),
                os: "Linux 5.15.0".to_string(),
                distro: None,
                projects: Vec::new(),
                tools: Vec::new(),
                git: None,
                env_flags: Vec::new(),
            },
            model: Some("qwen2.5-coder:1.5b".to_string()),
            temperature: Some(0.2),
            explain: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("qwen2.5-coder:1.5b"));
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.model, Some("qwen2.5-coder:1.5b".to_string()));
        assert_eq!(parsed.temperature, Some(0.2));
    }

    #[test]
    fn test_request_explain_defaults_false_on_the_wire() {
        // Older clients omit the field; it must deserialize as false.
        let json = r#"{"query":"x","context":{"cwd":"/","shell":"sh","os":"linux"}}"#;
        let parsed: Request = serde_json::from_str(json).unwrap();
        assert!(!parsed.explain);

        let req = Request {
            query: "x".to_string(),
            context: Context {
                cwd: "/".into(),
                shell: "sh".to_string(),
                os: "linux".to_string(),
                distro: None,
                projects: Vec::new(),
                tools: Vec::new(),
                git: None,
                env_flags: Vec::new(),
            },
            model: None,
            temperature: None,
            explain: true,
        };
        let round: Request = serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert!(round.explain);
    }
}
