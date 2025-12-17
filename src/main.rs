//! llmcmd - A hyper-performant terminal command translator.
//!
//! Takes natural language input via a minimal TUI popup and outputs the exact
//! shell command. Designed for sub-500ms latency with a daemon + client model.

mod client;
mod config;
mod context;
mod daemon;
mod protocol;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::ModelSelection;
use std::process::Command as ProcessCommand;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "llmcmd")]
#[command(author, version, about = "A hyper-performant terminal command translator")]
#[command(long_about = "Takes natural language input and outputs shell commands.\n\nPress Ctrl+K in your shell to invoke (after shell integration is set up).")]
struct Cli {
    /// Direct query mode - provide the query as an argument
    #[arg(value_name = "QUERY")]
    query: Option<String>,

    /// No TUI, just output the command (for scripting)
    #[arg(long)]
    pipe: bool,

    /// Use the fast profile (smaller, faster model)
    #[arg(short = 'f', long)]
    fast: bool,

    /// Use a named profile from config
    #[arg(short = 'p', long, value_name = "NAME")]
    profile: Option<String>,

    /// Override model (ignores profile)
    #[arg(short = 'm', long, value_name = "MODEL")]
    model: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the llmcmd daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Manage Ollama models (pull, list, remove)
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
    /// Open configuration file in $EDITOR
    Config,
    /// Install shell integration
    Install,
    /// List available profiles
    Profiles,
}

#[derive(Subcommand)]
enum ModelsAction {
    /// List locally available models
    List,
    /// Pull/download a model from Ollama registry
    Pull {
        /// Model name (e.g., qwen2.5-coder:7b, llama3.2:3b)
        model: String,
    },
    /// Remove a model from local storage
    Remove {
        /// Model name to remove
        model: String,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start the daemon in the background
    Start,
    /// Stop the running daemon
    Stop,
    /// Check daemon status
    Status,
    /// Run the daemon in the foreground (for debugging)
    Run,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Daemon { action }) => handle_daemon(action).await,
        Some(Commands::Models { action }) => handle_models(action).await,
        Some(Commands::Config) => handle_config(),
        Some(Commands::Install) => handle_install(),
        Some(Commands::Profiles) => handle_profiles(),
        None => {
            // Build model selection from CLI args
            let model_selection = ModelSelection {
                model: cli.model,
                profile: cli.profile,
                fast: cli.fast,
            };
            // Client mode - send query to daemon
            handle_query(cli.query, cli.pipe, model_selection).await
        }
    }
}

/// Handle daemon subcommands.
async fn handle_daemon(action: DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Start => start_daemon().await,
        DaemonAction::Stop => stop_daemon().await,
        DaemonAction::Status => daemon_status().await,
        DaemonAction::Run => run_daemon_foreground().await,
    }
}

/// Start the daemon in the background.
/// Note: All output goes to stderr to avoid polluting stdout (which may be captured by shell).
async fn start_daemon() -> Result<()> {
    // Check if already running
    if daemon::server::is_daemon_running().await {
        eprintln!("Daemon is already running");
        return Ok(());
    }

    // Clear any existing startup status file
    let status_path = config::Config::startup_status_path()?;
    let _ = std::fs::remove_file(&status_path);

    // Get the current executable path
    let exe = std::env::current_exe().context("Failed to get current executable path")?;

    // Spawn the daemon process
    let child = ProcessCommand::new(&exe)
        .args(["daemon", "run"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to start daemon process")?;

    let pid = child.id();
    eprintln!("Starting daemon (PID {})...", pid);

    // Wait for startup status (poll the status file)
    let max_wait = 3000; // 3 seconds max
    let poll_interval = 100; // 100ms
    let mut waited = 0;

    while waited < max_wait {
        tokio::time::sleep(tokio::time::Duration::from_millis(poll_interval)).await;
        waited += poll_interval;

        // Check if status file exists
        if let Ok(status) = std::fs::read_to_string(&status_path) {
            if status.starts_with("OK") {
                eprintln!("Daemon is ready");
                return Ok(());
            } else if status.starts_with("ERROR:") {
                // Clean up the status file
                let _ = std::fs::remove_file(&status_path);
                let error_msg = status.strip_prefix("ERROR: ").unwrap_or(&status);
                eprintln!("\nDaemon failed to start:\n{}", error_msg);
                std::process::exit(1);
            }
        }

        // Also check if process is still alive
        // If socket exists and responds, we're good
        if daemon::server::is_daemon_running().await {
            eprintln!("Daemon is ready");
            return Ok(());
        }
    }

    // Timeout - check status file one more time
    if let Ok(status) = std::fs::read_to_string(&status_path) {
        let _ = std::fs::remove_file(&status_path);
        if status.starts_with("ERROR:") {
            let error_msg = status.strip_prefix("ERROR: ").unwrap_or(&status);
            eprintln!("\nDaemon failed to start:\n{}", error_msg);
            std::process::exit(1);
        }
    }

    eprintln!("\nDaemon startup timed out. Run 'llmcmd daemon run' to see errors.");
    std::process::exit(1);
}

/// Stop the running daemon.
async fn stop_daemon() -> Result<()> {
    if !daemon::server::is_daemon_running().await {
        println!("Daemon is not running");
        return Ok(());
    }

    daemon::server::stop_daemon().await?;
    println!("Daemon stopped");
    Ok(())
}

/// Show daemon status.
async fn daemon_status() -> Result<()> {
    if daemon::server::is_daemon_running().await {
        let config = config::Config::load()?;
        let pid = daemon::server::get_daemon_pid().await;

        println!("Daemon: running");
        if let Some(pid) = pid {
            println!("PID: {}", pid);
        }
        println!("Backend: {}", config.backend_type());
        println!("Default model: {}", config.model_name());
        println!("Default profile: {}", config.default_profile());
        println!(
            "Socket: {}",
            config::Config::socket_path()?.display()
        );
    } else {
        println!("Daemon: not running");
        println!("Start with: llmcmd daemon start");
    }
    Ok(())
}

/// Run the daemon in the foreground.
async fn run_daemon_foreground() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("llmcmd=info".parse().unwrap())
                .add_directive("reqwest=warn".parse().unwrap()),
        )
        .init();

    info!("Starting llmcmd daemon...");

    let config = config::Config::load().context("Failed to load configuration")?;
    info!(
        "Using backend: {} (default model: {})",
        config.backend_type(),
        config.model_name()
    );

    let server = daemon::DaemonServer::new(config)?;
    server.run().await
}

/// Handle models subcommand (for Ollama).
async fn handle_models(action: ModelsAction) -> Result<()> {
    let config = config::Config::load()?;

    // Get Ollama host from config
    let host = match &config.backend {
        config::BackendConfig::Ollama { host, .. } => host.clone(),
        _ => {
            // Use default Ollama host even if not the active backend
            "http://localhost:11434".to_string()
        }
    };

    match action {
        ModelsAction::List => list_models(&host).await,
        ModelsAction::Pull { model } => pull_model(&host, &model).await,
        ModelsAction::Remove { model } => remove_model(&host, &model).await,
    }
}

/// List available Ollama models.
async fn list_models(host: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/tags", host);

    let response = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .context("Failed to connect to Ollama. Is it running?")?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to list models: {}",
            response.status()
        ));
    }

    let data: serde_json::Value = response.json().await?;

    println!("Available Models");
    println!("================\n");

    if let Some(models) = data.get("models").and_then(|m| m.as_array()) {
        if models.is_empty() {
            println!("No models installed.");
            println!("\nPull a model with: llmcmd models pull <model>");
            println!("Example: llmcmd models pull qwen2.5-coder:7b");
        } else {
            for model in models {
                let name = model.get("name").and_then(|n| n.as_str()).unwrap_or("unknown");
                let size = model
                    .get("size")
                    .and_then(|s| s.as_u64())
                    .map(|s| format_size(s))
                    .unwrap_or_else(|| "?".to_string());
                let modified = model
                    .get("modified_at")
                    .and_then(|m| m.as_str())
                    .map(|s| s.split('T').next().unwrap_or(s))
                    .unwrap_or("?");

                println!("  {} ({}) - {}", name, size, modified);
            }
        }
    } else {
        println!("No models found.");
    }

    Ok(())
}

/// Format bytes to human-readable size.
fn format_size(bytes: u64) -> String {
    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;

    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0}MB", bytes as f64 / MB as f64)
    } else {
        format!("{}B", bytes)
    }
}

/// Pull/download a model from Ollama.
async fn pull_model(host: &str, model: &str) -> Result<()> {
    println!("Pulling model: {}", model);
    println!("This may take a while depending on model size...\n");

    let client = reqwest::Client::new();
    let url = format!("{}/api/pull", host);

    let response = client
        .post(&url)
        .json(&serde_json::json!({ "name": model, "stream": true }))
        .timeout(std::time::Duration::from_secs(3600)) // 1 hour timeout for large models
        .send()
        .await
        .context("Failed to connect to Ollama. Is it running?")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("Failed to pull model: {} - {}", status, body));
    }

    // Stream the response to show progress
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut last_status = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        // Each line is a JSON object
        for line in String::from_utf8_lossy(&chunk).lines() {
            if line.is_empty() {
                continue;
            }
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(status) = json.get("status").and_then(|s| s.as_str()) {
                    // Only print if status changed
                    if status != last_status {
                        if let Some(completed) = json.get("completed").and_then(|c| c.as_u64()) {
                            if let Some(total) = json.get("total").and_then(|t| t.as_u64()) {
                                let pct = (completed as f64 / total as f64 * 100.0) as u32;
                                print!("\r{}: {}% ({}/{})", status, pct, format_size(completed), format_size(total));
                                std::io::Write::flush(&mut std::io::stdout())?;
                            }
                        } else {
                            println!("{}", status);
                        }
                        last_status = status.to_string();
                    } else if json.get("completed").is_some() {
                        // Update progress on same line
                        if let Some(completed) = json.get("completed").and_then(|c| c.as_u64()) {
                            if let Some(total) = json.get("total").and_then(|t| t.as_u64()) {
                                let pct = (completed as f64 / total as f64 * 100.0) as u32;
                                print!("\r{}: {}% ({}/{})", status, pct, format_size(completed), format_size(total));
                                std::io::Write::flush(&mut std::io::stdout())?;
                            }
                        }
                    }
                }
            }
        }
    }

    println!("\n\nModel '{}' pulled successfully!", model);
    Ok(())
}

/// Remove a model from Ollama.
async fn remove_model(host: &str, model: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/delete", host);

    let response = client
        .delete(&url)
        .json(&serde_json::json!({ "name": model }))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("Failed to connect to Ollama. Is it running?")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Failed to remove model: {} - {}",
            status,
            body
        ));
    }

    println!("Model '{}' removed successfully.", model);
    Ok(())
}

/// Handle the config command.
fn handle_config() -> Result<()> {
    let config_path = config::Config::config_path()?;

    // Ensure config directory exists
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Create default config if it doesn't exist
    if !config_path.exists() {
        let default_config = config::Config::default();
        default_config.save()?;
        println!("Created default config at {}", config_path.display());
    }

    // Open in editor
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = ProcessCommand::new(&editor)
        .arg(&config_path)
        .status()
        .context("Failed to open editor")?;

    if !status.success() {
        eprintln!("Editor exited with non-zero status");
    }

    Ok(())
}

/// Handle the install command for shell integration.
fn handle_install() -> Result<()> {
    println!("Shell Integration Setup");
    println!("=======================\n");

    let shell = std::env::var("SHELL").unwrap_or_default();

    if shell.contains("zsh") {
        println!("Add to ~/.zshrc:\n");
        println!(r#"function _llmcmd_widget() {{
    local cmd
    cmd=$(llmcmd </dev/tty)
    if [[ -n "$cmd" ]]; then
        LBUFFER+="$cmd"
    fi
    zle redisplay
}}
zle -N _llmcmd_widget
bindkey '^k' _llmcmd_widget"#);
    } else if shell.contains("bash") {
        println!("Add to ~/.bashrc:\n");
        println!(r#"_llmcmd_readline() {{
    local cmd
    cmd=$(llmcmd </dev/tty)
    READLINE_LINE="${{READLINE_LINE}}${{cmd}}"
    READLINE_POINT=${{#READLINE_LINE}}
}}
bind -x '"\C-k": _llmcmd_readline'"#);
    } else if shell.contains("fish") {
        println!("Add to ~/.config/fish/config.fish:\n");
        println!(r#"function _llmcmd_fish
    set -l cmd (llmcmd </dev/tty)
    commandline -i $cmd
end
bind \ck _llmcmd_fish"#);
    } else {
        println!("Unknown shell: {}", shell);
        println!("\nManual setup required. See documentation for shell integration examples.");
    }

    println!("\n\nAfter adding the integration, restart your shell or run:");
    println!("  source ~/.zshrc  # or your shell's config file");

    Ok(())
}

/// Handle the profiles subcommand.
fn handle_profiles() -> Result<()> {
    let config = config::Config::load()?;

    println!("Available Profiles");
    println!("==================\n");

    let default_profile = config.default_profile();

    // Sort profile names for consistent output
    let mut profile_names: Vec<_> = config.profiles.keys().collect();
    profile_names.sort();

    for name in profile_names {
        if let Some(profile) = config.profiles.get(name) {
            let is_default = name == default_profile;
            let default_marker = if is_default { " (default)" } else { "" };
            println!(
                "  {}{}\n    model: {}\n    temperature: {}\n",
                name,
                default_marker,
                profile.model,
                profile.temperature.unwrap_or(0.1)
            );
        }
    }

    println!("Usage:");
    println!("  llmcmd --fast \"query\"           # Use 'fast' profile");
    println!("  llmcmd --profile heavy \"query\"  # Use 'heavy' profile");
    println!("  llmcmd --model custom:7b \"query\" # Override model directly");

    Ok(())
}

/// Handle query mode (TUI or pipe).
async fn handle_query(
    query: Option<String>,
    pipe_mode: bool,
    model_selection: ModelSelection,
) -> Result<()> {
    // Load config to resolve model selection
    let config = config::Config::load()?;
    let resolved_model = model_selection.resolve_model(&config);
    let resolved_temperature = model_selection.resolve_temperature(&config);

    // Ensure daemon is running, try to auto-start if not
    if !daemon::server::is_daemon_running().await {
        if pipe_mode {
            eprintln!("Daemon not running. Start with: llmcmd daemon start");
            std::process::exit(1);
        }

        // Try to auto-start
        eprintln!("Starting daemon...");
        start_daemon().await?;

        // Give it time to start
        tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;

        if !daemon::server::is_daemon_running().await {
            eprintln!("Failed to start daemon. Check your configuration.");
            std::process::exit(1);
        }
    }

    // Get the query
    let final_query = if pipe_mode {
        // In pipe mode, query must be provided
        query.ok_or_else(|| anyhow::anyhow!("Query required in --pipe mode"))?
    } else {
        // Run TUI
        match client::run_tui(query)? {
            client::tui::TuiResult::Query(q) => q,
            client::tui::TuiResult::Cancelled => {
                return Ok(());
            }
        }
    };

    // Gather context
    let ctx = context::gather_context()?;

    // Send query to daemon with model override
    match client::send_query(final_query, ctx, Some(resolved_model), Some(resolved_temperature))
        .await
    {
        Ok(command) => {
            // Output just the command to stdout
            println!("{}", command);
        }
        Err(e) => {
            if !pipe_mode {
                eprintln!("Error: {}", e);
            }
            std::process::exit(1);
        }
    }

    Ok(())
}
