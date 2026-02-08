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

You know the command exists. You just can't remember if it's `tar -xzf` or `tar -xvf`, whether `find` takes `-name` or `-iname` first, or what the `awk` syntax is for the third column. incant removes that friction entirely. It knows your OS, your shell, your working directory, and your preferred tools.

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
