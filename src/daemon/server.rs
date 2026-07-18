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
        let backend = create_backend(&config);

        Ok(Self {
            config,
            socket_path,
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
        // Ensure the runtime directory exists and is private. XDG_RUNTIME_DIR
        // is 0700 per spec; the ~/.local/run fallback is created (or
        // tightened) to 0700 so no other local user can reach the socket,
        // even during the window between bind() and chmod below.
        if let Some(parent) = self.socket_path.parent() {
            ensure_private_dir(parent).with_context(|| {
                format!("Failed to secure socket directory: {}", parent.display())
            })?;
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

        // Bind to the socket, then restrict it to the owning user. The
        // private parent directory above already blocks access during the
        // bind-to-chmod window; this is defense in depth.
        let listener = UnixListener::bind(&self.socket_path)
            .with_context(|| format!("Failed to bind to socket: {}", self.socket_path.display()))?;
        restrict_to_owner(&self.socket_path).with_context(|| {
            format!(
                "Failed to restrict socket permissions: {}",
                self.socket_path.display()
            )
        })?;

        info!("Daemon listening on {}", self.socket_path.display());

        // Write PID file
        self.write_pid_file().await?;

        // Write success status
        Self::write_startup_status("OK").await?;

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

    /// Write startup status to a file for parent process to read.
    async fn write_startup_status(status: &str) -> Result<()> {
        let status_path = Config::startup_status_path()?;
        if let Some(parent) = status_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&status_path, status).await?;
        Ok(())
    }

    /// Get the socket path.
    #[allow(dead_code)]
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

/// Create `dir` if needed and ensure it is accessible only by its owner
/// (mode 0700). Applies to pre-existing directories too, so a loose
/// `~/.local/run` from an earlier install gets tightened.
fn ensure_private_dir(dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            info!("Tightened permissions on {} to 0700", dir.display());
        }
    }
    Ok(())
}

/// Restrict a filesystem entry (the daemon socket) to owner read/write.
fn restrict_to_owner(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode_of(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    #[cfg(unix)]
    fn ensure_private_dir_creates_with_0700() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode_of(&dir), 0o700);
    }

    #[test]
    #[cfg(unix)]
    fn ensure_private_dir_tightens_existing_loose_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode_of(&dir), 0o700);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn bound_socket_is_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("t.sock");
        let _listener = tokio::net::UnixListener::bind(&sock).unwrap();
        restrict_to_owner(&sock).unwrap();
        assert_eq!(mode_of(&sock), 0o600);
    }
}
