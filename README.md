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
# Build and install
./install.sh

# Or manually:
cargo build --release
cp target/release/llmcmd ~/.local/bin/

# Start the daemon
llmcmd daemon start

# Use it!
llmcmd "find all rust files modified today"
# Output: fd -e rs --changed-within 1d
```

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
```

## Shell Integration

After installation, add shell integration to invoke llmcmd with Ctrl+K:

### Zsh (~/.zshrc)

```zsh
function _llmcmd_widget() {
    local cmd=$(llmcmd)
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
    local cmd=$(llmcmd)
    READLINE_LINE="${READLINE_LINE}${cmd}"
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\C-k": _llmcmd_readline'
```

### Fish (~/.config/fish/config.fish)

```fish
function _llmcmd_fish
    set -l cmd (llmcmd)
    commandline -i $cmd
end
bind \ck _llmcmd_fish
```

## Configuration

Configuration file: `~/.config/llmcmd/config.toml`

### Ollama (Default)

```toml
[backend]
type = "ollama"
model = "qwen2.5-coder:7b"
host = "http://localhost:11434"

[preferences]
modern_tools = true    # prefer rg/fd/bat over grep/find/cat
verbose_flags = true   # prefer --recursive over -r
```

### Anthropic Claude

```toml
[backend]
type = "anthropic"
model = "claude-3-5-haiku-latest"
# api_key = "sk-ant-..." # Or set ANTHROPIC_API_KEY env var
```

### OpenAI

```toml
[backend]
type = "openai"
model = "gpt-4o-mini"
# api_key = "sk-..." # Or set OPENAI_API_KEY env var
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
