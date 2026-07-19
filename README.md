# incant

[![CI](https://github.com/deepc0py/incant/actions/workflows/ci.yml/badge.svg)](https://github.com/deepc0py/incant/actions/workflows/ci.yml)
[![CodeQL](https://github.com/deepc0py/incant/actions/workflows/codeql.yml/badge.svg)](https://github.com/deepc0py/incant/actions/workflows/codeql.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV: 1.88](https://img.shields.io/badge/MSRV-1.88-orange.svg)](Cargo.toml)

Natural language to shell commands. Instantly.

`incant` is an AI-powered command-line tool that turns natural language into shell commands, written in Rust and built for local LLMs. Press **Ctrl+K**, describe what you want, get the exact command. Sub-500ms with on-device models via Ollama, under a second with Claude or GPT. No copy-paste, no browser, no context switching -- and by default, nothing leaves your machine.

```
  $ ctrl+k
  ┌──────────────── incant ────────────────┐
  │ find rust files modified today          │
  └────────────────────────────────────────-┘
  $ fd -e rs --changed-within 1d
```

## Why incant?

Every terminal user has the same experience. You know what you want to do. You can describe it in plain English in about four seconds. But the exact flags, the argument order, the syntax quirks of `find` vs `fd`, `sed` on Linux vs macOS, `tar` with or without the dash -- that's where the flow breaks. You pause. You open a browser tab. You scan three Stack Overflow answers. You copy, paste, adjust. Ninety seconds gone for a command you'll forget again next month.

This happens to everyone. Junior devs, staff engineers, sysadmins who've been at it for twenty years. The command-line surface area is enormous and nobody holds all of it in their head. The friction isn't ignorance -- it's the mismatch between how fast you can think and how slow it is to look things up.

incant closes that gap. It runs as a daemon with your shell context already loaded -- OS, shell, cwd, project type, git state, which modern tools you actually have installed. You describe the intent, it returns the exact command. The response comes back in the same terminal where you need it, before you'd have finished typing the URL to search for it.

## Install

```bash
git clone https://github.com/deepc0py/incant.git
cd incant
./install.sh
```

The installer builds from source, sets up a default config, optionally installs [Ollama](https://ollama.ai/) with a local model, and wires up the **Ctrl+K** shell binding.

Requires Rust 1.88+ and one of:
- **Ollama** -- local models, no API key, fully private (recommended)
- **Anthropic API key** -- Claude
- **OpenAI API key** -- GPT

## Quick Start

```bash
# Start the daemon
incant daemon start

# Press Ctrl+K in your shell, or:
incant "list all docker containers that exited with an error"
# docker ps -a --filter "status=exited" --filter "exited=1"

incant "compress this directory excluding node_modules"
# tar --exclude='node_modules' -czf archive.tar.gz .

incant --pipe "disk usage sorted by size" | sh
# Pipe mode: no TUI, direct output, scriptable

incant --explain "find files modified in the last hour"
# fd --changed-within 1h            <- stdout: just the command
# "fd finds files; --changed-within limits to the last hour"  <- stderr
```

## Usage

```bash
incant                            # Interactive TUI popup
incant "query"                    # Direct: print + copy to clipboard (auto-starts daemon)
incant --pipe "query"             # Script mode: stdout only, no clipboard, no auto-start
incant --explain "query"          # Also print a short explanation to stderr
incant --fast "query"             # Use fast profile (smaller/faster model)
incant --profile heavy "query"    # Use a named profile
incant --model gpt-4o "query"     # Override model directly

incant daemon start|stop|status   # Daemon lifecycle
incant models list|pull|remove    # Ollama model management
incant config                     # Open config in $EDITOR
incant profiles                   # List available profiles
incant install                    # Show shell integration setup
```

## Safety Warnings

The whole premise of incant is that you might not fully know the command you asked for. So the daemon inspects every generated command against a tested rule table -- broad recursive deletes, raw disk writes, `curl | sh`, fork bombs, force-pushes, SQL drops, and friends -- and warns on stderr before you press Enter:

```
$ incant --pipe "delete everything"
!! destructive: recursively force-deletes the filesystem root, home directory, or everything in the current directory
rm -rf /
```

Three levels: `safe` (silence), `caution`, `destructive`. Warnings never touch stdout, so pipes and shell integration stay clean. This is an advisory guardrail against accidents, not a sandbox -- incant never executes anything; you always review the command yourself. Disable with `safety_warnings = false` under `[preferences]`.

## Architecture

```
┌───────────────────────────────────────────────┐
│  incant daemon (long-running)                 │
│                                               │
│  - Holds LLM connections + pre-cached prompt  │
│  - Async Tokio runtime                        │
│  - Ollama / Anthropic / OpenAI backends       │
└───────────────────┬───────────────────────────┘
                    │ Unix domain socket
                    │ (length-prefixed JSON)
┌───────────────────┴───────────────────────────┐
│  incant client                                │
│                                               │
│  - Instant startup (<30ms)                    │
│  - Minimal TUI renders to stderr              │
│  - Command output to stdout                   │
└───────────────────────────────────────────────┘
```

The daemon stays warm with LLM connections and a pre-cached system prompt. The client is a thin TUI that sends your query over a Unix socket and prints the result. This split is why it feels instant -- the expensive work (model loading, connection setup) happens once.

## Configuration

Config lives at `~/.config/incant/config.toml`. Run `incant config` to edit it.

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
modern_tools = true      # prefer rg/fd/bat over grep/find/cat
verbose_flags = true     # prefer --recursive over -r
safety_warnings = true   # warn on stderr for destructive commands
```

See [`config.example.toml`](config.example.toml) for the full reference.

## Shell Integration

The installer sets this up automatically. Press **Ctrl+K** anywhere in your terminal to open the TUI, type your request, and the generated command is inserted at your cursor.

<details>
<summary>Manual setup (zsh / bash / fish)</summary>

**Zsh** (`~/.zshrc`):
```zsh
function _incant_widget() {
    local cmd
    cmd=$(incant </dev/tty)
    if [[ -n "$cmd" ]]; then
        LBUFFER+="$cmd"
    fi
    zle redisplay
}
zle -N _incant_widget
bindkey '^k' _incant_widget
```

**Bash** (`~/.bashrc`):
```bash
_incant_readline() {
    local cmd
    cmd=$(incant </dev/tty)
    READLINE_LINE="${READLINE_LINE}${cmd}"
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"\C-k": _incant_readline'
```

**Fish** (`~/.config/fish/config.fish`):
```fish
function _incant_fish
    set -l cmd (incant </dev/tty)
    commandline -i $cmd
end
bind \ck _incant_fish
```

The `</dev/tty` redirect is required for the TUI to work inside shell widgets.
</details>

## Security & Privacy

Local-first by design: the default Ollama backend keeps queries, context, and generated commands entirely on-device. Cloud backends are an explicit opt-in config edit. The daemon socket is owner-only (`0600` inside a `0700` runtime dir), config files holding API keys are written `0600`, and shell history is never read. The full threat model -- including what incant deliberately does *not* defend against -- lives in [SECURITY.md](SECURITY.md). Report vulnerabilities via [private advisory](https://github.com/deepc0py/incant/security/advisories/new), not public issues.

## Performance

| Metric | Value |
|---|---|
| Client startup | <30ms |
| Query to response (Ollama, warm) | <500ms |
| Query to response (Claude API) | <1s |
| Client memory | <10MB |
| Daemon memory (idle) | <50MB |

The release binary is built with LTO, single codegen unit, symbol stripping, and panic=abort.

## How it compares

| Tool | Difference |
|---|---|
| **GitHub Copilot CLI** | Cloud-only and subscription-gated. incant runs free local models by default; cloud is opt-in. |
| **thefuck** | Corrects a command *after* it fails. incant generates the right command before you run anything. |
| **shell-gpt / zsh-codex** | Python scripts calling cloud APIs per keystroke. incant is a compiled Rust daemon: warm connections, sub-500ms local inference, safety warnings, no interpreter startup. |
| **Warp AI** | A whole terminal replacement. incant is one keybinding inside the shell you already use. |

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
# Binary: target/release/incant
```

```bash
cargo test   # 65 unit tests + 11 end-to-end integration tests (hermetic, no network)
```

Development workflow, commit conventions, and the safety-rule testing contract are in [CONTRIBUTING.md](CONTRIBUTING.md). Release history lives in [CHANGELOG.md](CHANGELOG.md).

## License

Apache-2.0
