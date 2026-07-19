//! Platform-native client for communicating with the daemon.

use crate::protocol::{framing, Context, Message, Request, Response};
use crate::safety::Assessment;
use crate::transport::{self, ClientStream};
use anyhow::{Context as AnyhowContext, Result};
use std::time::Duration;

/// A generated command together with the daemon's advisory risk assessment
/// and optional explanation.
pub struct GeneratedCommand {
    pub command: String,
    pub risk: Option<Assessment>,
    pub explanation: Option<String>,
}

/// Send a query to the daemon and return the generated command.
pub async fn send_query(
    query: String,
    context: Context,
    model: Option<String>,
    temperature: Option<f32>,
    explain: bool,
) -> Result<GeneratedCommand> {
    let endpoint = transport::endpoint()?;

    // Windows validates the pipe owner's SID inside connect(), before this
    // function can send any context or prompt data.
    let stream = tokio::time::timeout(Duration::from_secs(5), transport::connect(&endpoint))
        .await
        .map_err(|_| anyhow::anyhow!("Connection timeout - is the daemon running?"))?
        .with_context(|| {
            format!("Failed to connect to daemon at {endpoint}. Start it with: incant daemon start")
        })?;

    send_query_to_stream(stream, query, context, model, temperature, explain).await
}

/// Send a query to an existing stream.
async fn send_query_to_stream(
    mut stream: ClientStream,
    query: String,
    context: Context,
    model: Option<String>,
    temperature: Option<f32>,
    explain: bool,
) -> Result<GeneratedCommand> {
    let request = Request {
        query,
        context,
        model,
        temperature,
        explain,
    };
    let message = Message::Query(Box::new(request));

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
        Ok(GeneratedCommand {
            command,
            risk: response.risk,
            explanation: response.explanation,
        })
    } else if let Some(error) = response.error {
        Err(anyhow::anyhow!("{}", error))
    } else {
        Err(anyhow::anyhow!("Invalid response from daemon"))
    }
}

/// Check if the daemon is reachable.
#[allow(dead_code)]
pub async fn check_daemon() -> Result<()> {
    let endpoint = transport::endpoint()?;

    let mut stream = tokio::time::timeout(Duration::from_secs(2), transport::connect(&endpoint))
        .await
        .map_err(|_| anyhow::anyhow!("Connection timeout"))??;

    // Send status request
    framing::write_message(&mut stream, &Message::Status).await?;

    let response: Response =
        tokio::time::timeout(Duration::from_secs(2), framing::read_message(&mut stream))
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
