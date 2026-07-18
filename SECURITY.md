# Security Policy

## Reporting a Vulnerability

Use [GitHub private vulnerability reporting](https://github.com/deepc0py/incant/security/advisories/new)
(Security → Report a vulnerability). Reports are acknowledged within a week.
Please do not open public issues for suspected vulnerabilities.

## Threat Model

incant is a natural-language-to-shell-command translator: a per-user daemon
that talks to an LLM backend, and a thin client that talks to the daemon over
a Unix domain socket. Understanding what it defends against — and what it
deliberately does not — matters more than any single control.

### What incant never does

- **incant never executes commands.** Output goes to stdout / the shell
  buffer; the user always reviews and presses Enter themselves. There is no
  auto-execution mode and none is planned.
- **incant never reads shell history.** The context sent to the model is
  exactly: OS/distro, shell name, cwd, project marker filenames, names of
  installed CLI tools from a fixed probe list, git branch + dirty/clean, and
  ssh/tmux/docker flags. Nothing else. (An opt-in history feature is tracked
  in #14 and will ship with redaction controls or not at all.)

### Trust boundaries

| Boundary | Control |
|---|---|
| Daemon socket (`$XDG_RUNTIME_DIR/incant.sock` or `~/.local/run/incant.sock`) | Parent directory enforced `0700` at startup (pre-existing loose dirs are tightened); socket `chmod 0600` immediately after bind. Only the owning user can connect. |
| Config file (may contain API keys) | Directory `0700`, file written `0600`; re-saving re-tightens a loosened file. Environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) are the recommended way to supply keys. |
| IPC framing | Length-prefixed JSON with a hard 1 MB frame cap; malformed frames fail the connection, never the daemon. |
| LLM backends | Local Ollama by default. Cloud backends (Anthropic, OpenAI) are explicit opt-in via config; when enabled, the query and the context listed above are sent to that provider. |

### Local-first privacy

The default configuration runs entirely on-device via Ollama: queries,
context, and generated commands never leave the machine. This is a design
pillar, not an accident — cloud backends exist for users who choose the
capability/privacy trade, and choosing one is a deliberate config edit.

### The safety analysis is advisory

The daemon flags generated commands that look destructive (`rm -rf` on broad
paths, `dd` onto block devices, `curl | sh`, sudoers tampering, fork bombs,
…) and the client prints warnings on stderr. This is a **heuristic guardrail
against accidents, not a security boundary**:

- A silent rule miss is expected for novel or obfuscated commands.
- It must never be used to "sanitize" untrusted input for execution.
- `safe` means "no known-bad pattern matched", nothing stronger.

### LLM output is untrusted

Generated commands are model output and may be wrong, subtly harmful, or —
if a backend or model is compromised — malicious. The product contract is
that a human reviews every command before running it. The safety layer
reduces the odds of an accidental footgun; the human stays the boundary.

### Out of scope

- Attacks by root or by the same user account (a same-user process can
  already do anything incant can).
- Compromise of the configured LLM backend or model weights.
- The security of commands the user chooses to run after review.

## Supply Chain

- CI runs `cargo audit` (RustSEC advisories, warnings denied) and
  `cargo deny` (license allowlist, yanked-crate denial, registry
  restrictions) on every PR.
- CodeQL static analysis runs on every PR and weekly.
- All GitHub Actions are pinned to full commit SHAs; workflow tokens are
  read-only; checkout credentials are not persisted.
- Dependabot keeps dependencies and action pins current.

## Supported Versions

Only the latest release receives security fixes.
