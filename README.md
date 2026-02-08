# incant

Natural language to shell commands. Instantly.

`incant` is a terminal-native command translator written in Rust. Press **Ctrl+K**, describe what you want, get the exact command. Sub-500ms with local models, under a second with cloud APIs. No copy-paste, no browser, no context switching.

```
  $ ctrl+k
  ┌──────────────── llmcmd ────────────────┐
  │ find rust files modified today          │
  └────────────────────────────────────────-┘
  $ fd -e rs --changed-within 1d
```

## Why incant?

Every terminal user has the same experience. You know what you want to do. You can describe it in plain English in about four seconds. But the exact flags, the argument order, the syntax quirks of `find` vs `fd`, `sed` on Linux vs macOS, `tar` with or without the dash -- that's where the flow breaks. You pause. You open a browser tab. You scan three Stack Overflow answers. You copy, paste, adjust. Ninety seconds gone for a command you'll forget again next month.

This happens to everyone. Junior devs, staff engineers, sysadmins who've been at it for twenty years. The command-line surface area is enormous and nobody holds all of it in their head. The friction isn't ignorance -- it's the mismatch between how fast you can think and how slow it is to look things up.

incant closes that gap. It runs as a daemon with your shell context already loaded -- OS, shell, cwd, installed tools. You describe the intent, it returns the exact command. The response comes back in the same terminal where you need it, before you'd have finished typing the URL to search for it.

## Install

```bash
git clone https://github.com/deepc0py/incant.git
cd incant
./install.sh
```

The installer builds from source, sets up a default config, optionally installs [Ollama](https://ollama.ai/) with a local model, and wires up the **Ctrl+K** shell binding.

Requires Rust 1.70+ and one of:
- **Ollama** -- local models, no API key, fully private (recommended)
- **Anthropic API key** -- Claude
- **OpenAI API key** -- GPT

## Quick Start

```bash
# Start the daemon
llmcmd daemon start

# Press Ctrl+K in your shell, or:
llmcmd "list all docker containers that exited with an error"
# docker ps -a --filter "status=exited" --filter "exited=1"

llmcmd "compress this directory excluding node_modules"
# tar --exclude='node_modules' -czf archive.tar.gz .

llmcmd --pipe "disk usage sorted by size" | sh
# Pipe mode: no TUI, direct output, scriptable
```

## Usage

```bash
llmcmd                            # Interactive TUI popup
llmcmd "query"                    # TUI with pre-filled query
llmcmd --pipe "query"             # No TUI, direct stdout (for scripting)
llmcmd --fast "query"             # Use fast profile (smaller/faster model)
llmcmd --profile heavy "query"    # Use a named profile
llmcmd --model gpt-4o "query"     # Override model directly

llmcmd daemon start|stop|status   # Daemon lifecycle
llmcmd models list|pull|remove    # Ollama model management
llmcmd config                     # Open config in $EDITOR
llmcmd profiles                   # List available profiles
llmcmd install                    # Show shell integration setup
```

## Architecture

```
┌───────────────────────────────────────────────┐
│  llmcmd daemon (long-running)                 │
│                                               │
│  - Holds LLM connections + pre-cached prompt  │
│  - Async Tokio runtime                        │
│  - Ollama / Anthropic / OpenAI backends       │
└───────────────────┬───────────────────────────┘
                    │ Unix domain socket
                    │ (length-prefixed JSON)
┌───────────────────┴───────────────────────────┐
│  llmcmd client                                │
│                                               │
│  - Instant startup (<30ms)                    │
│  - Minimal TUI renders to stderr              │
│  - Command output to stdout                   │
└───────────────────────────────────────────────┘
```

The daemon stays warm with LLM connections and a pre-cached system prompt. The client is a thin TUI that sends your query over a Unix socket and prints the result. This split is why it feels instant -- the expensive work (model loading, connection setup) happens once.

## Configuration

Config lives at `~/.config/llmcmd/config.toml`. Run `llmcmd config` to edit it.

### Ollama (default -- fully local, no API key)

```toml
[backend]
type = "ollama"
host = "http://localhost:11434"
default_profile = "default"

[profiles.default]
model = "qwen2.5-coder:7b"
temperature = 0.1

[profiles.fast]
model = "qwen2.5-coder:1.5b"
temperature = 0.1
```

### Cloud Backends

```toml
# Anthropic
[backend]
type = "anthropic"
# Set ANTHROPIC_API_KEY env var, or:
# api_key = "sk-ant-..."

[profiles.default]
model = "claude-3-5-haiku-latest"
temperature = 0.1
```

```toml
# OpenAI
[backend]
type = "openai"
# Set OPENAI_API_KEY env var, or:
# api_key = "sk-..."

[profiles.default]
model = "gpt-4o-mini"
temperature = 0.1
```

### Preferences

```toml
[preferences]
modern_tools = true    # prefer rg/fd/bat over grep/find/cat
verbose_flags = true   # prefer --recursive over -r
```

See [`config.example.toml`](config.example.toml) for the full reference.

## Shell Integration

The installer sets this up automatically. Press **Ctrl+K** anywhere in your terminal to open the TUI, type your request, and the generated command is inserted at your cursor.

<details>
<summary>Manual setup (zsh / bash / fish)</summary>

**Zsh** (`~/.zshrc`):
```zsh
function _llmcmd_widget() {
    local cmd
    cmd=$(llmcmd </dev/tty)
    if [[ -n "$cmd" ]]; then
        LBUFFER+="$cmd"
    fi
    zle redisplay
}
zle -N _llmcmd_widget
bindkey '^k' _llmcmd_widget
```

**Bash** (`~/.bashrc`):
```bash
_llmcmd_readline() {
    local cmd
    cmd=$(llmcmd </dev/tty)
    READLINE_LINE="${READLINE_LINE}${cmd}"
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\C-k": _llmcmd_readline'
```

**Fish** (`~/.config/fish/config.fish`):
```fish
function _llmcmd_fish
    set -l cmd (llmcmd </dev/tty)
    commandline -i $cmd
end
bind \ck _llmcmd_fish
```

The `</dev/tty` redirect is required for the TUI to work inside shell widgets.
</details>

## Performance

| Metric | Value |
|---|---|
| Client startup | <30ms |
| Query to response (Ollama, warm) | <500ms |
| Query to response (Claude API) | <1s |
| Client memory | <10MB |
| Daemon memory (idle) | <50MB |

The release binary is built with LTO, single codegen unit, symbol stripping, and panic=abort.

## Where this is going

incant currently does one thing well: single-command translation. But the architecture -- a persistent daemon with pluggable LLM backends and full shell context -- enables a lot more:

- **Multi-step workflow generation.** A query like `"set up a new Rust project with CI, a Dockerfile, and a gitignore"` maps to an ordered sequence of commands with dependency awareness -- the daemon already has the context to know what's scaffolded and what's missing.
- **Correction learning from edits.** The shell widget can diff what incant generated against what you actually ran. Over time, a per-user correction log teaches the system your preferences -- your aliases, your flag style, which tools you actually have installed.
- **Streaming token output.** The daemon's async Tokio runtime and the length-prefixed socket protocol support chunked responses. The TUI can render partial commands as tokens arrive instead of blocking on full inference.
- **Shell history and alias awareness.** The context module (`context.rs`) currently gathers OS, shell, and cwd. Extending it to parse `~/.zsh_history`, alias definitions, and environment variables would let the model generate commands that match how *you* work, not how a generic user works. This is entirely opt-in and user-controlled — history contains sensitive data (paths, hostnames, credentials that slipped into commands), so incant will never read it without explicit consent. When enabled, users configure exactly what gets shared: last N commands only, redaction patterns, or a curated alias-only mode.
- **Backend-agnostic inference.** The `Backend` enum dispatches to Ollama, Anthropic, and OpenAI today. The same pattern extends to Groq, Mistral, or local GGUF models via llama.cpp -- anything that accepts a system prompt and returns text.
- **Pipeline composition.** `"find large log files from last week and compress them"` should produce a working one-liner with pipes, not a single command. The system prompt already understands the user's shell -- it can generate `find | xargs` vs `fd -x` depending on what's available.

## Building from Source

```bash
git clone https://github.com/deepc0py/incant.git
cd incant
cargo build --release
# Binary: target/release/llmcmd
```

```bash
cargo test   # Run tests
```

## License

Apache-2.0
