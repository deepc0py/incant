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
}

impl Response {
    /// Create a successful response with a command.
    pub fn success(command: String) -> Self {
        Self {
            command: Some(command),
            error: None,
        }
    }

    /// Create an error response.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            command: None,
            error: Some(message.into()),
        }
    }
}

/// Status information for the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// Whether the daemon is running.
    pub running: bool,
    /// The backend type being used.
    pub backend: String,
    /// The model being used.
    pub model: String,
    /// Process ID of the daemon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// Message type for IPC communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// A query request from the client.
    Query(Request),
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
        let resp = Response::success("ls -la".to_string());
        assert_eq!(resp.command, Some("ls -la".to_string()));
        assert!(resp.error.is_none());
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
            },
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.query, req.query);
    }
}
