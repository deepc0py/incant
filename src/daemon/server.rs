//! Platform-native IPC server for the daemon.
//!
//! Handles client connections and routes requests to the LLM backend.

use crate::config::Config;
use crate::daemon::llm::{create_backend, Backend};
use crate::protocol::{framing, Message, Response};
use crate::transport::{self, Endpoint, Listener, ServerStream};
use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::{debug, error, info};

/// The daemon server that listens for client connections.
pub struct DaemonServer {
    config: Config,
    endpoint: Endpoint,
    backend: Arc<Backend>,
}

impl DaemonServer {
    /// Create a new daemon server.
    pub fn new(config: Config) -> Result<Self> {
        let endpoint = transport::endpoint()?;
        let backend = create_backend(&config);

        Ok(Self {
            config,
            endpoint,
            backend: Arc::new(backend),
        })
    }

    /// Run the daemon server.
    pub async fn run(&self) -> Result<()> {
        // Wrap the startup logic to catch and report errors
        match self.run_inner().await {
            Ok(()) => Ok(()),
            Err(e) => {
                // Write error to startup status file
                let _ = Self::write_startup_status(&format!("ERROR: {:#}", e)).await;
                Err(e)
            }
        }
    }

    /// Inner run logic that can fail during startup.
    async fn run_inner(&self) -> Result<()> {
        // Perform health check
        info!("Checking backend health...");
        self.backend.health_check().await.with_context(|| {
            format!(
                "Backend health check failed for {} ({}).\n\nPossible causes:\n  - Ollama is not running (start with: ollama serve)\n  - Wrong API key for cloud backends\n  - Network connectivity issues",
                self.backend.name(),
                self.backend.model()
            )
        })?;
        info!(
            "Backend ready: {} ({})",
            self.backend.name(),
            self.backend.model()
        );

        let mut listener = Listener::bind(&self.endpoint)?;

        info!("Daemon listening on {}", self.endpoint);

        // Write PID file
        self.write_pid_file().await?;

        // Write success status
        Self::write_startup_status("OK").await?;

        // Accept connections
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let backend = Arc::clone(&self.backend);
                    let config = self.config.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(stream, backend, config).await {
                            error!("Error handling client: {}", e);
                        }
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }

    /// Write the PID file.
    async fn write_pid_file(&self) -> Result<()> {
        let pid_path = Config::pid_path()?;
        crate::transport::write_private_file(&pid_path, std::process::id().to_string().as_bytes())?;
        info!("PID file written to {}", pid_path.display());
        Ok(())
    }

    /// Write startup status to a file for parent process to read.
    async fn write_startup_status(status: &str) -> Result<()> {
        let status_path = Config::startup_status_path()?;
        crate::transport::write_private_file(&status_path, status.as_bytes())?;
        Ok(())
    }

    /// Get the daemon endpoint.
    #[allow(dead_code)]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

/// Handle a single client connection.
async fn handle_client(
    mut stream: ServerStream,
    backend: Arc<Backend>,
    config: Config,
) -> Result<()> {
    debug!("Client connected");

    // Read the message
    let message: Message = framing::read_message(&mut stream).await?;

    let response = match message {
        Message::Query(request) => {
            debug!("Received query: {}", request.query);

            // Build the system prompt
            let system_prompt = config.build_system_prompt(&request.context);

            // Extract model and temperature overrides from request
            let model_override = request.model.as_deref();
            let temperature_override = request.temperature;

            if model_override.is_some() {
                debug!("Using model override: {:?}", model_override);
            }

            // Generate the command
            match backend
                .generate(
                    &system_prompt,
                    &request.query,
                    model_override,
                    temperature_override,
                )
                .await
            {
                Ok(command) => {
                    debug!("Generated command: {}", command);
                    let risk = crate::safety::assess(&command);
                    if !risk.is_safe() {
                        debug!("Safety findings: {:?}", risk.findings);
                    }
                    if request.explain {
                        match explain_command(
                            &backend,
                            &command,
                            model_override,
                            temperature_override,
                        )
                        .await
                        {
                            Ok(explanation) => {
                                Response::success(command, risk).with_explanation(explanation)
                            }
                            Err(e) => {
                                error!("Explanation failed: {}", e);
                                Response::error(format!("Explanation failed: {}", e))
                            }
                        }
                    } else {
                        Response::success(command, risk)
                    }
                }
                Err(e) => {
                    error!("Generation failed: {}", e);
                    Response::error(e.to_string())
                }
            }
        }
        Message::Status => {
            // Return status information
            Response::plain(format!("Backend: {} ({})", backend.name(), backend.model()))
        }
        Message::Shutdown => {
            info!("Received shutdown request");
            // Named pipes need no cleanup; Unix removes its socket file.
            if let Ok(endpoint) = transport::endpoint() {
                let _ = transport::cleanup(&endpoint).await;
            }
            if let Ok(pid_path) = Config::pid_path() {
                let _ = tokio::fs::remove_file(&pid_path).await;
            }
            std::process::exit(0);
        }
    };

    // Write the response
    framing::write_message(&mut stream, &response).await?;
    debug!("Response sent");

    Ok(())
}

/// System prompt for the explanation pass. Kept separate from command
/// generation so each call does exactly one job.
const EXPLAIN_SYSTEM_PROMPT: &str = "You explain shell commands to someone learning the terminal.\n\nRules:\n- Reply in 1-3 short plain-text lines\n- Describe what the command does and what each notable flag means\n- No markdown, no code fences, no preamble";

/// Ask the backend for a short explanation of an already-generated command.
async fn explain_command(
    backend: &Backend,
    command: &str,
    model_override: Option<&str>,
    temperature_override: Option<f32>,
) -> Result<String> {
    backend
        .generate(
            EXPLAIN_SYSTEM_PROMPT,
            command,
            model_override,
            temperature_override,
        )
        .await
}

/// Check if the daemon is running.
pub async fn is_daemon_running() -> bool {
    let Ok(endpoint) = transport::endpoint() else {
        return false;
    };
    let Ok(mut stream) = transport::connect(&endpoint).await else {
        return false;
    };
    if framing::write_message(&mut stream, &Message::Status)
        .await
        .is_err()
    {
        return false;
    }
    framing::read_message::<_, Response>(&mut stream)
        .await
        .is_ok()
}

/// Stop the running daemon.
pub async fn stop_daemon() -> Result<()> {
    let endpoint = transport::endpoint()?;
    let mut stream = transport::connect(&endpoint)
        .await
        .context("Failed to connect to daemon")?;

    framing::write_message(&mut stream, &Message::Shutdown).await?;
    info!("Shutdown request sent");

    Ok(())
}

/// Get the daemon's PID if running.
pub async fn get_daemon_pid() -> Option<u32> {
    if let Ok(pid_path) = Config::pid_path() {
        #[cfg(windows)]
        {
            crate::transport::ensure_private_directory(pid_path.parent()?).ok()?;
            crate::transport::secure_existing_file(&pid_path).ok()?;
        }
        if let Ok(contents) = tokio::fs::read_to_string(&pid_path).await {
            return contents.trim().parse().ok();
        }
    }
    None
}
