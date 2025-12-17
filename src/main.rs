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
    /// Open configuration file in $EDITOR
    Config,
    /// Install shell integration
    Install,
    /// List available profiles
    Profiles,
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
async fn start_daemon() -> Result<()> {
    // Check if already running
    if daemon::server::is_daemon_running().await {
        println!("Daemon is already running");
        return Ok(());
    }

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

    println!("Daemon started with PID {}", child.id());

    // Wait a moment for it to initialize
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Verify it's running
    if daemon::server::is_daemon_running().await {
        println!("Daemon is ready");
    } else {
        eprintln!("Warning: Daemon may not have started correctly. Check logs.");
    }

    Ok(())
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
    local cmd=$(llmcmd)
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
    local cmd=$(llmcmd)
    READLINE_LINE="${{READLINE_LINE}}${{cmd}}"
    READLINE_POINT=${{#READLINE_LINE}}
}}
bind -x '"\C-k": _llmcmd_readline'"#);
    } else if shell.contains("fish") {
        println!("Add to ~/.config/fish/config.fish:\n");
        println!(r#"function _llmcmd_fish
    set -l cmd (llmcmd)
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
