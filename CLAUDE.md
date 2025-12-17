# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

llmcmd is a hyper-performant terminal command translator written in Rust. It takes natural language input via a minimal TUI popup and outputs shell commands. The tool uses a daemon + client architecture for sub-500ms latency.

## Build Commands

```bash
# Build
cargo build --release

# Run tests
cargo test

# Run with release optimizations
cargo run --release

# Install locally
./install.sh
# Or manually: cp target/release/llmcmd ~/.local/bin/
```

## Architecture

### Daemon + Client Model

The project uses a two-process architecture for performance:

```
llmcmd-daemon (long-running)          llmcmd (client)
├── Holds LLM connections             ├── Minimal TUI input
├── Unix socket listener              ├── Sends queries via socket
├── Pre-cached system prompt          └── Outputs command to stdout
└── Handles inference
    └── via Unix domain socket
```

### Source Structure

```
src/
├── main.rs           # CLI entry, subcommand routing (clap)
├── config.rs         # Config parsing, profiles, model selection
├── context.rs        # System context gathering (OS, shell, cwd)
├── protocol.rs       # IPC message types (Request/Response)
├── client/
│   ├── mod.rs
│   ├── tui.rs        # Ratatui-based input widget
│   └── socket.rs     # Unix socket client
└── daemon/
    ├── mod.rs
    ├── server.rs     # Unix socket server, request handling
    └── llm/
        ├── mod.rs        # LlmBackend trait
        ├── ollama.rs     # Ollama backend
        ├── anthropic.rs  # Claude API backend
        └── openai.rs     # OpenAI backend
```

### Key Components

- **LLM Backends**: All backends implement the `LlmBackend` async trait in `daemon/llm/mod.rs`. Adding a new backend requires implementing `generate()` and `name()` methods.
- **IPC Protocol**: JSON-based request/response over Unix domain socket (`protocol.rs`). Supports model overrides per request.
- **Profiles**: Configuration profiles allow preset model/temperature combinations (`config.rs`).

## CLI Usage

```bash
llmcmd                          # Interactive TUI mode
llmcmd "query"                  # TUI with pre-filled query
llmcmd --pipe "query"           # No TUI, direct output
llmcmd --fast "query"           # Use fast profile
llmcmd --profile heavy "query"  # Use named profile
llmcmd --model gpt-4o "query"   # Override model

llmcmd daemon start|stop|status|run
llmcmd models list|pull|remove  # Ollama model management
llmcmd config                   # Open config in $EDITOR
llmcmd profiles                 # List available profiles
```

## Configuration

Config file: `~/.config/llmcmd/config.toml`

Supports three backends:
- **Ollama** (default): Local models, requires Ollama running
- **Anthropic**: Claude API, uses `ANTHROPIC_API_KEY` env var
- **OpenAI**: GPT models, uses `OPENAI_API_KEY` env var
