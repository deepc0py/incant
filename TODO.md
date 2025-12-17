# Claude Code Project Prompt: llmcmd

## Project Overview

Build a **hyper-performant terminal command translator** in Rust. The tool takes natural language input via a minimal TUI popup and outputs the exact shell command — nothing else. Think of it as a portable, terminal-native version of Cursor's Cmd+K, but laser-focused on command generation.

### Core User Flow
```
1. User hits Ctrl+K in terminal
2. Minimal input popup appears (gum-style or ratatui)
3. User types: "ripgrep for TODO, include subdirs, show line numbers"
4. Popup closes, command appears in shell buffer: rg -n "TODO" .
5. User reviews, hits Enter to execute
```

### Non-Goals
- This is NOT a chatbot — no explanations, no markdown, no conversation
- No command execution — only injection into the shell buffer for user review
- No heavy GUI frameworks — terminal-native only

---

## Architecture

### Daemon + Client Model (for sub-500ms latency)

```
┌─────────────────────────────────────────────────────┐
│  llmcmd-daemon                                      │
│  - Long-running process                             │
│  - Holds LLM connection (Ollama or API)             │
│  - Listens on unix socket: ~/.local/run/llmcmd.sock │
│  - Pre-cached system prompt                         │
│  - Handles inference                                │
└─────────────────────────────────────────────────────┘
                         ▲
                         │ Unix domain socket (not HTTP — saves ~50ms)
                         ▼
┌─────────────────────────────────────────────────────┐
│  llmcmd (client binary)                             │
│  - Tiny, instant startup                            │
│  - Renders minimal TUI input                        │
│  - Sends query to daemon                            │
│  - Receives command string                          │
│  - Outputs to stdout (for shell integration)        │
└─────────────────────────────────────────────────────┘
```

### Why This Architecture?
- Model loading and connection overhead happens once (daemon startup)
- Client is just IPC + TUI — should start in <20ms
- Unix sockets are faster than HTTP localhost

---

## Technical Requirements

### Daemon (`llmcmd-daemon`)

**Responsibilities:**
1. Start on user login (or first invocation)
2. Connect to Ollama (default) or Claude API (configurable)
3. Listen on `$XDG_RUNTIME_DIR/llmcmd.sock` or `~/.local/run/llmcmd.sock`
4. Accept JSON requests: `{"query": "...", "context": {...}}`
5. Return raw command string (no JSON wrapper needed for response)
6. Handle concurrent requests (multiple terminals)

**LLM Backend Support:**
```rust
enum Backend {
    Ollama { model: String, host: String },  // default: qwen2.5-coder:7b, localhost:11434
    Anthropic { model: String, api_key: String },  // claude-3-5-haiku-latest
    OpenAI { model: String, api_key: String },
}
```

**System Prompt (critical for output quality):**
```
You are a shell command generator. Your ONLY output is the exact command to run.

Rules:
- Output ONLY the command, nothing else
- No markdown, no backticks, no explanations
- No preamble like "Here's the command:" 
- If multiple commands needed, separate with && or ;
- Make reasonable assumptions for ambiguous requests
- Use common modern tools (ripgrep over grep, fd over find, bat over cat) when appropriate
- Prefer long flags for clarity (--recursive over -r) unless user implies brevity

Context:
OS: {os}
Shell: {shell}
CWD: {cwd}
```

**Config File:** `~/.config/llmcmd/config.toml`
```toml
[backend]
type = "ollama"  # or "anthropic", "openai"
model = "qwen2.5-coder:7b"
host = "http://localhost:11434"  # for ollama
# api_key = "..." # for cloud providers, prefer env var ANTHROPIC_API_KEY

[preferences]
modern_tools = true  # prefer rg/fd/bat over grep/find/cat
verbose_flags = true  # prefer --long-flags
```

### Client (`llmcmd`)

**Responsibilities:**
1. Render minimal TUI input prompt
2. Gather terminal context (cwd, shell, OS)
3. Send query to daemon via unix socket
4. Print command to stdout
5. Exit immediately

**TUI Requirements:**
- Single-line input field (or multi-line if query is long)
- Minimal chrome — just a prompt indicator and input
- Support paste (Ctrl+V)
- Escape to cancel
- Enter to submit
- Should feel like `gum input` but faster

**Suggested crates:**
- `ratatui` + `crossterm` for TUI (or `tui-input` for simpler impl)
- `tokio` for async socket communication
- `serde` / `serde_json` for IPC serialization

**Context Gathering:**
```rust
struct Context {
    cwd: PathBuf,
    shell: String,      // $SHELL
    os: String,         // output of `uname -a`
    distro: Option<String>,  // /etc/os-release
    // Future: last_command, last_exit_code, recent_history
}
```

### CLI Interface

```bash
# Client commands
llmcmd                     # Interactive TUI mode (default)
llmcmd "query here"        # Direct query mode, still shows TUI briefly
llmcmd --pipe "query"      # No TUI, just output (for scripting)
llmcmd daemon start        # Start daemon in background
llmcmd daemon stop         # Stop daemon
llmcmd daemon status       # Check if running, show backend info
llmcmd config              # Open config in $EDITOR
```

---

## Shell Integration

### Zsh Integration (`~/.zshrc`)
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

### Bash Integration (`~/.bashrc`)
```bash
_llmcmd_readline() {
    local cmd=$(llmcmd)
    READLINE_LINE="${READLINE_LINE}${cmd}"
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\C-k": _llmcmd_readline'
```

### Fish Integration (`~/.config/fish/config.fish`)
```fish
function _llmcmd_fish
    set -l cmd (llmcmd)
    commandline -i $cmd
end
bind \ck _llmcmd_fish
```

**Installer script should offer to append these automatically.**

---

## Project Structure

```
llmcmd/
├── Cargo.toml
├── README.md
├── src/
│   ├── main.rs           # CLI entry point, subcommand routing
│   ├── client/
│   │   ├── mod.rs
│   │   ├── tui.rs        # Ratatui input widget
│   │   └── socket.rs     # Unix socket client
│   ├── daemon/
│   │   ├── mod.rs
│   │   ├── server.rs     # Socket server, request handling
│   │   └── llm/
│   │       ├── mod.rs    # Backend trait
│   │       ├── ollama.rs
│   │       ├── anthropic.rs
│   │       └── openai.rs
│   ├── config.rs         # Config parsing
│   ├── context.rs        # System context gathering
│   └── protocol.rs       # IPC message types
├── install.sh            # Shell integration installer
└── config.example.toml
```

---

## Performance Targets

| Metric | Target |
|--------|--------|
| Client startup to TUI visible | <30ms |
| Query to response (Ollama, warm) | <500ms |
| Query to response (Claude API) | <1s |
| Memory (client) | <10MB |
| Memory (daemon, idle) | <50MB (excluding model if local) |

### Optimization Strategies
1. Client binary should be minimal — no heavy deps
2. Daemon keeps connection alive to Ollama/API
3. Pre-build system prompt once at daemon startup
4. Use `tokio::io` for non-blocking socket I/O
5. Consider `jemalloc` or `mimalloc` if memory allocation is bottleneck

---

## Implementation Phases

### Phase 1: MVP
- [ ] Basic daemon with Ollama support
- [ ] Unix socket IPC
- [ ] Minimal TUI client (just input + output)
- [ ] Direct query mode (`llmcmd "query"`)
- [ ] Basic config file

### Phase 2: Polish
- [ ] Claude/OpenAI backend support
- [ ] Shell integration installer script
- [ ] Daemon auto-start on first client invocation
- [ ] `daemon start/stop/status` commands
- [ ] Streaming responses (show command as it generates)

### Phase 3: Advanced
- [ ] Context: capture last command + exit code
- [ ] Context: recent shell history
- [ ] Safety warnings for dangerous commands (rm -rf, dd, etc.)
- [ ] Command history/favorites
- [ ] `--explain` flag for learning mode (returns command + explanation)

---

## Error Handling

- If daemon not running: auto-start it, or show helpful error
- If Ollama not running: clear error message with instructions
- If API key missing: point to config file
- If query fails: show error in TUI, don't crash
- Timeout after 30s with graceful message

---

## Testing Strategy

1. **Unit tests:** Config parsing, context gathering, protocol serialization
2. **Integration tests:** Daemon IPC round-trip
3. **Manual testing matrix:**
   - Ollama with various models
   - Claude API
   - Different shells (bash, zsh, fish)
   - Linux + macOS

---

## Dependencies (Suggested)

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
ratatui = "0.28"
crossterm = "0.28"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
dirs = "5"
reqwest = { version = "0.12", features = ["json", "stream"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

---

## Reference Implementation Hints

### Ollama API (POST to /api/generate)
```rust
#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    prompt: String,
    stream: bool,
    system: String,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    temperature: f32,  // 0.1 for deterministic commands
    num_predict: i32,  // limit output length, e.g., 200 tokens
}
```

### Unix Socket Quick Start
```rust
use tokio::net::{UnixListener, UnixStream};

// Server
let listener = UnixListener::bind(socket_path)?;
loop {
    let (stream, _) = listener.accept().await?;
    tokio::spawn(handle_client(stream));
}

// Client
let mut stream = UnixStream::connect(socket_path).await?;
stream.write_all(request.as_bytes()).await?;
let mut response = String::new();
stream.read_to_string(&mut response).await?;
```

---

## User Context

The developer building this:
- 10 years programming experience (Python, Bash, TypeScript)
- B.S. Computer Science, M.S. Cybersecurity  
- 15 years IT experience
- Comfortable with systems programming concepts
- Learning Rust through this project is acceptable
- Primary use case: debugging OS issues, learning terminal commands

---

## Success Criteria

The tool is "done" when:
1. Ctrl+K opens input in <50ms
2. "ripgrep for TODO in all files with line numbers" → `rg -n "TODO" .`
3. Command appears in shell buffer, user hits Enter to run
4. Works on both Linux and macOS
5. Switching between Ollama and Claude requires only config change

---

## First Task

Start with the daemon + client architecture. Get a basic round-trip working:
1. Daemon starts and listens on unix socket
2. Client connects, sends hardcoded query
3. Daemon calls Ollama, returns response
4. Client prints response

Then layer in the TUI and config system.
