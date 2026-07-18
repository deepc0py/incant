## Summary

<!-- What changes and why. PRs are squash-merged: the PR title becomes the commit title, so phrase it as a conventional commit (e.g. `fix(daemon): ...`). -->

## Linked issue

Closes #N

## How verified

<!-- Commands run and observed output. "cargo test passes" alone is not verification for behavior changes — show the changed path exercised. -->

## Checklist

- [ ] Tests updated for behavior changes
- [ ] `cargo clippy --all-targets -- -D warnings` and `cargo fmt --all --check` pass locally
- [ ] No `llmcmd` regressions (paths, socket name, config dir)
- [ ] Docs updated if user-facing behavior changed
