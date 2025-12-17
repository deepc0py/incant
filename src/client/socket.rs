//! Unix socket client for communicating with the daemon.

use crate::config::Config;
use crate::protocol::{framing, Context, Message, Request, Response};
use anyhow::{Context as AnyhowContext, Result};
use std::time::Duration;
use tokio::net::UnixStream;

/// Send a query to the daemon and return the generated command.
pub async fn send_query(
    query: String,
    context: Context,
    model: Option<String>,
    temperature: Option<f32>,
) -> Result<String> {
    let socket_path = Config::socket_path()?;

    // Connect with timeout
    let stream = tokio::time::timeout(
        Duration::from_secs(5),
        UnixStream::connect(&socket_path),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Connection timeout - is the daemon running?"))?
    .with_context(|| {
        format!(
            "Failed to connect to daemon at {}. Start it with: llmcmd daemon start",
            socket_path.display()
        )
    })?;

    send_query_to_stream(stream, query, context, model, temperature).await
}

/// Send a query to an existing stream.
async fn send_query_to_stream(
    mut stream: UnixStream,
    query: String,
    context: Context,
    model: Option<String>,
    temperature: Option<f32>,
) -> Result<String> {
    let request = Request {
        query,
        context,
        model,
        temperature,
    };
    let message = Message::Query(request);

    // Send the request
    framing::write_message(&mut stream, &message).await?;

    // Read the response with timeout
    let response: Response = tokio::time::timeout(
        Duration::from_secs(60), // LLM can take a while
        framing::read_message(&mut stream),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Request timeout - LLM took too long"))??;

    // Extract the command or error
    if let Some(command) = response.command {
        Ok(command)
    } else if let Some(error) = response.error {
        Err(anyhow::anyhow!("{}", error))
    } else {
        Err(anyhow::anyhow!("Invalid response from daemon"))
    }
}

/// Check if the daemon is reachable.
#[allow(dead_code)]
pub async fn check_daemon() -> Result<()> {
    let socket_path = Config::socket_path()?;

    if !socket_path.exists() {
        return Err(anyhow::anyhow!(
            "Daemon socket not found at {}. Start daemon with: llmcmd daemon start",
            socket_path.display()
        ));
    }

    let mut stream = tokio::time::timeout(
        Duration::from_secs(2),
        UnixStream::connect(&socket_path),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Connection timeout"))??;

    // Send status request
    framing::write_message(&mut stream, &Message::Status).await?;

    let response: Response = tokio::time::timeout(
        Duration::from_secs(2),
        framing::read_message(&mut stream),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Status check timeout"))??;

    if response.error.is_some() {
        return Err(anyhow::anyhow!(
            "Daemon returned error: {:?}",
            response.error
        ));
    }

    Ok(())
}
