# llmcmd

A hyper-performant terminal command translator in Rust. Takes natural language input via a minimal TUI popup and outputs the exact shell command — nothing else. Think of it as a portable, terminal-native version of Cursor's Cmd+K, but laser-focused on command generation.

## Features

- **Sub-500ms latency** with daemon + client architecture
- **Minimal TUI** - single-line input popup, no bloat
- **Multiple backends** - Ollama (local), Anthropic Claude, OpenAI GPT
- **Shell integration** - Ctrl+K binding for zsh, bash, and fish
- **Context-aware** - knows your OS, shell, and current directory

## Quick Start

```bash
# Build and install (will also set up Ollama if needed)
./install.sh

# Start the daemon
llmcmd daemon start

# Press Ctrl+K in your shell, or:
llmcmd "find all rust files modified today"
# Output: fd -e rs --changed-within 1d
```

The installer will:
- Build the binary
- Set up Ollama (if not installed)
- Pull the default model
- Configure shell integration (Ctrl+K)

## Usage

### Interactive Mode (Default)

```bash
llmcmd                     # Opens TUI popup
llmcmd "query here"        # Pre-fills TUI with query
```

### Pipe Mode

```bash
llmcmd --pipe "list files" # No TUI, outputs command directly
```

### Daemon Management

```bash
llmcmd daemon start        # Start daemon in background
llmcmd daemon stop         # Stop the daemon
llmcmd daemon status       # Check if running, show backend info
llmcmd daemon run          # Run in foreground (for debugging)
```

### Configuration

```bash
llmcmd config              # Open config in $EDITOR
llmcmd install             # Show shell integration setup
llmcmd profiles            # List available profiles
```

### Model Management (Ollama)

```bash
llmcmd models list         # List installed models
llmcmd models pull <model> # Download a model
llmcmd models remove <model> # Remove a model
```

## Shell Integration

The installer will automatically set this up, but you can also add it manually.

Press **Ctrl+K** anywhere in your shell to open the TUI, type your request, and get a command inserted at your cursor.

### Zsh (~/.zshrc)

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

### Bash (~/.bashrc)

```bash
_llmcmd_readline() {
    local cmd
    cmd=$(llmcmd </dev/tty)
    READLINE_LINE="${READLINE_LINE}${cmd}"
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\C-k": _llmcmd_readline'
```

### Fish (~/.config/fish/config.fish)

```fish
function _llmcmd_fish
    set -l cmd (llmcmd </dev/tty)
    commandline -i $cmd
end
bind \ck _llmcmd_fish
```

> **Note:** The `</dev/tty` redirect is required for the TUI to work correctly in shell widgets.

## Configuration

Configuration file: `~/.config/llmcmd/config.toml`

### Ollama (Default)

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

[preferences]
modern_tools = true    # prefer rg/fd/bat over grep/find/cat
verbose_flags = true   # prefer --recursive over -r
```

### Anthropic Claude

```toml
[backend]
type = "anthropic"
default_profile = "default"
# api_key = "sk-ant-..." # Or set ANTHROPIC_API_KEY env var

[profiles.default]
model = "claude-3-5-haiku-latest"
temperature = 0.1
```

### OpenAI

```toml
[backend]
type = "openai"
default_profile = "default"
# api_key = "sk-..." # Or set OPENAI_API_KEY env var

[profiles.default]
model = "gpt-4o-mini"
temperature = 0.1
```

### Using Profiles

```bash
llmcmd --fast "query"           # Use fast profile (smaller model)
llmcmd --profile heavy "query"  # Use named profile
llmcmd --model gpt-4o "query"   # Override model directly
llmcmd profiles                 # List available profiles
```

## Architecture

```
┌─────────────────────────────────────────────────────┐
│  llmcmd-daemon                                      │
│  - Long-running process                             │
│  - Holds LLM connection                             │
│  - Listens on unix socket                           │
│  - Pre-cached system prompt                         │
└─────────────────────────────────────────────────────┘
                         ▲
                         │ Unix domain socket
                         ▼
┌─────────────────────────────────────────────────────┐
│  llmcmd (client)                                    │
│  - Tiny, instant startup (<30ms)                    │
│  - Renders minimal TUI input                        │
│  - Outputs command to stdout                        │
└─────────────────────────────────────────────────────┘
```

## Requirements

- Rust 1.70+
- One of:
  - [Ollama](https://ollama.ai/) with a code-focused model (recommended: `qwen2.5-coder:7b`)
  - Anthropic API key
  - OpenAI API key

## Building from Source

```bash
# Clone the repository
git clone https://github.com/deepc0py/incant.git
cd incant

# Build
cargo build --release

# The binary is at target/release/llmcmd
```

## Performance Targets

| Metric | Target |
|--------|--------|
| Client startup to TUI visible | <30ms |
| Query to response (Ollama, warm) | <500ms |
| Query to response (Claude API) | <1s |
| Memory (client) | <10MB |
| Memory (daemon, idle) | <50MB |

## License

MIT
