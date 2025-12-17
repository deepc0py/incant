//! Unix socket server for the daemon.
//!
//! Handles client connections and routes requests to the LLM backend.

use crate::config::Config;
use crate::daemon::llm::{create_backend, Backend};
use crate::protocol::{framing, Message, Response};
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, error, info};

/// The daemon server that listens for client connections.
pub struct DaemonServer {
    config: Config,
    socket_path: PathBuf,
    backend: Arc<Backend>,
}

impl DaemonServer {
    /// Create a new daemon server.
    pub fn new(config: Config) -> Result<Self> {
        let socket_path = Config::socket_path()?;
        let backend = create_backend(&config.backend);

        Ok(Self {
            config,
            socket_path,
            backend: Arc::new(backend),
        })
    }

    /// Run the daemon server.
    pub async fn run(&self) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.socket_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create socket directory: {}", parent.display()))?;
        }

        // Remove existing socket file
        if self.socket_path.exists() {
            tokio::fs::remove_file(&self.socket_path)
                .await
                .with_context(|| {
                    format!(
                        "Failed to remove existing socket: {}",
                        self.socket_path.display()
                    )
                })?;
        }

        // Perform health check
        info!("Checking backend health...");
        self.backend.health_check().await.with_context(|| {
            format!(
                "Backend health check failed for {} ({})",
                self.backend.name(),
                self.backend.model()
            )
        })?;
        info!(
            "Backend ready: {} ({})",
            self.backend.name(),
            self.backend.model()
        );

        // Bind to the socket
        let listener = UnixListener::bind(&self.socket_path)
            .with_context(|| format!("Failed to bind to socket: {}", self.socket_path.display()))?;

        info!("Daemon listening on {}", self.socket_path.display());

        // Write PID file
        self.write_pid_file().await?;

        // Accept connections
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
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
        if let Some(parent) = pid_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let pid = std::process::id();
        tokio::fs::write(&pid_path, pid.to_string()).await?;
        info!("PID file written to {}", pid_path.display());
        Ok(())
    }

    /// Get the socket path.
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }
}

/// Handle a single client connection.
async fn handle_client(
    mut stream: UnixStream,
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

            // Generate the command
            match backend.generate(&system_prompt, &request.query).await {
                Ok(command) => {
                    debug!("Generated command: {}", command);
                    Response::success(command)
                }
                Err(e) => {
                    error!("Generation failed: {}", e);
                    Response::error(e.to_string())
                }
            }
        }
        Message::Status => {
            // Return status information
            Response::success(format!(
                "Backend: {} ({})",
                backend.name(),
                backend.model()
            ))
        }
        Message::Shutdown => {
            info!("Received shutdown request");
            // Clean up and exit
            if let Ok(socket_path) = Config::socket_path() {
                let _ = tokio::fs::remove_file(&socket_path).await;
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

/// Check if the daemon is running.
pub async fn is_daemon_running() -> bool {
    if let Ok(socket_path) = Config::socket_path() {
        if socket_path.exists() {
            // Try to connect
            if let Ok(mut stream) = UnixStream::connect(&socket_path).await {
                // Send a status request
                if framing::write_message(&mut stream, &Message::Status)
                    .await
                    .is_ok()
                {
                    if let Ok(_response) = framing::read_message::<_, Response>(&mut stream).await {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Stop the running daemon.
pub async fn stop_daemon() -> Result<()> {
    let socket_path = Config::socket_path()?;
    if !socket_path.exists() {
        return Err(anyhow::anyhow!("Daemon is not running"));
    }

    let mut stream = UnixStream::connect(&socket_path)
        .await
        .context("Failed to connect to daemon")?;

    framing::write_message(&mut stream, &Message::Shutdown).await?;
    info!("Shutdown request sent");

    Ok(())
}

/// Get the daemon's PID if running.
pub async fn get_daemon_pid() -> Option<u32> {
    if let Ok(pid_path) = Config::pid_path() {
        if let Ok(contents) = tokio::fs::read_to_string(&pid_path).await {
            return contents.trim().parse().ok();
        }
    }
    None
}
