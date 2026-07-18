# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-07-17

### Added

- Advisory safety analysis of generated commands: a 23-rule table classifies each command as safe, caution, or destructive. Rule families cover broad recursive deletion, raw disk writes and formatting, world-writable permission changes, piping downloaded scripts to shells, fork bombs, destructive git operations, SQL drops, firewall flushes, and system power commands. Warnings print to stderr and can be disabled via `preferences.safety_warnings`.
- `-e`/`--explain` mode: a plain-language explanation of the generated command goes to stderr while the command itself stays on stdout, so pipes remain clean.
- Enriched prompt context: project markers (e.g. `Cargo.toml`, `package.json`), a PATH probe for installed tools, git branch and dirty state gathered with a single `git status` invocation, and ssh/tmux/docker session flags. Shell history is never read.
- CI pipeline: `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, tests on Ubuntu and macOS, `cargo audit`, and `cargo deny check`. All GitHub Actions are pinned to commit SHAs; Dependabot keeps them and Cargo dependencies current.
- CodeQL static analysis.
- `SECURITY.md` documenting the threat model and private vulnerability reporting.
- Issue and PR templates and contributor documentation (`CONTRIBUTING.md`).

### Changed

- **BREAKING:** the project was renamed from `llmcmd` to `incant` — binary, socket, and config directory. Migrate with `mv ~/.config/llmcmd ~/.config/incant` and re-run `install.sh` to refresh shell integration.
- The config path now follows XDG (`$XDG_CONFIG_HOME/incant/`, default `~/.config/incant/`) on all Unix platforms, including macOS.
- TUI stack upgraded to ratatui 0.30 and tui-input 0.15.

### Fixed

- Pipe-mode (`--pipe`) failures previously exited with no diagnostic at all; errors now always print to stderr.
- macOS no longer resolves the config file to the wrong directory.
- License metadata in `Cargo.toml` corrected from MIT to Apache-2.0, matching the `LICENSE` file.
- Socket and config files are created with restrictive permissions (see Security).

### Security

- Cleared all outstanding RustSec advisories: RUSTSEC-2026-0007 (`bytes`), four `rustls-webpki` advisories, an `anyhow` unsoundness advisory, `paste` and the old `lru` dropped via the ratatui/tui-input upgrade, and `atty` removed.
- The daemon socket is created `0600` inside a `0700` runtime directory (`$XDG_RUNTIME_DIR/incant.sock` or `~/.local/run/incant.sock`).
- The config file is written with `0600` permissions.

## [0.1.0] - 2026-02-08

### Added

- Initial release: daemon and client communicating over a Unix socket, three LLM backends (Ollama, Anthropic, OpenAI), interactive TUI, profiles, model management (`models list|pull|remove`), and `install.sh` shell integration.

[Unreleased]: https://github.com/deepc0py/incant/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/deepc0py/incant/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/deepc0py/incant/releases/tag/v0.1.0
