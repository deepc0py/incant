# Contributing to incant

## Development setup

1. Install Rust via [rustup](https://rustup.rs/). The minimum supported Rust version is **1.88** (enforced by `rust-version` in `Cargo.toml`).
2. Clone and build:

   ```sh
   git clone https://github.com/deepc0py/incant
   cd incant
   cargo build
   ```

3. Optional: install [Ollama](https://ollama.com/) for live end-to-end testing. The default backend talks to a local Ollama instance; unit tests do not require it. The Anthropic and OpenAI backends need an API key in `~/.config/incant/config.toml`.

## Build, test, lint

CI runs all of the following on every PR. Run them locally before pushing:

```sh
cargo test                                    # unit + integration tests
cargo clippy --all-targets -- -D warnings     # lint; warnings are errors
cargo fmt --all --check                       # formatting
cargo audit                                   # RustSec advisories (cargo install cargo-audit)
cargo deny check                              # licenses/bans/advisories (cargo install cargo-deny)
```

Tests run on Ubuntu and macOS in CI; a change that only passes on one platform will not merge.

## Project layout

| Path | Purpose |
| --- | --- |
| `src/main.rs` | CLI entry point and argument parsing |
| `src/client/tui.rs` | interactive terminal UI |
| `src/client/socket.rs` | client side of the Unix-socket protocol |
| `src/daemon/server.rs` | daemon: socket server, request handling |
| `src/daemon/llm/` | LLM backends (`ollama.rs`, `anthropic.rs`, `openai.rs`) |
| `src/config.rs` | config file loading (`~/.config/incant/config.toml`, `$XDG_CONFIG_HOME` honored) |
| `src/context.rs` | environment context gathering (project markers, PATH probe, git state) |
| `src/protocol.rs` | client↔daemon wire types |
| `src/safety.rs` | advisory safety analysis of generated commands |

## Debugging the daemon

Run the daemon in the foreground with logs on stderr instead of letting the client auto-spawn it:

```sh
incant daemon run
```

Then issue requests from a second terminal (`incant "list open ports"`). `RUST_LOG=debug` increases verbosity. The socket lives at `$XDG_RUNTIME_DIR/incant.sock` (fallback `~/.local/run/incant.sock`); `incant daemon status` and `incant daemon stop` manage a backgrounded instance.

## Commit convention

This repo uses [Conventional Commits](https://www.conventionalcommits.org/). Types in use: `feat`, `fix`, `ci`, `docs`, `chore`, `build`, `refactor`, `test`. Append `!` for breaking changes.

Examples:

```
feat: add --explain flag routing explanation to stderr
fix(daemon): create socket directory with 0700 before bind
ci: pin actions to commit SHAs
refactor(safety)!: rename RiskLevel::Warning to RiskLevel::Caution
```

## Pull requests

- CI must be green: tests, clippy, fmt, cargo-audit, cargo-deny.
- Behavior changes require tests. A PR that changes what the code does but not what the tests assert is incomplete.
- PRs are **squash-merged**; the PR title becomes the commit title, so write it as a conventional commit (`feat: …`, `fix(daemon): …`).
- Keep PRs focused. Unrelated refactoring belongs in its own PR.

## Safety rules

`src/safety.rs` holds an advisory rule table. Every new rule must ship with:

- **positive tests** — commands the rule must flag, and
- **negative tests** — nearby-but-benign commands the rule must *not* flag (e.g. `rm -rf ./build` must not trip the broad-target rule).

The tests in `src/safety.rs` show the pattern. Rules are heuristics, not a sandbox; keep descriptions factual about what the command does, and prefer structural `unless` exceptions over clever regexes.

## Security

Do **not** report vulnerabilities in public issues. See [SECURITY.md](SECURITY.md) for the threat model and private reporting via [GitHub security advisories](https://github.com/deepc0py/incant/security/advisories/new).
