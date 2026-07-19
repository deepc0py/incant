#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PTY_HELPER="$REPO_ROOT/tests/installer_pty.py"
SYSTEM_PATH=$PATH
TEST_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/incant-installer-tests.XXXXXX")
trap 'rm -rf "$TEST_ROOT"' EXIT

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    exit 1
}

assert_status() {
    local expected=$1
    if [ "$RUN_STATUS" -ne "$expected" ]; then
        printf '%s\n' "$RUN_OUTPUT" >&2
        fail "expected exit status $expected, got $RUN_STATUS"
    fi
}

assert_output_contains() {
    case "$RUN_OUTPUT" in
        *"$1"*) ;;
        *)
            printf '%s\n' "$RUN_OUTPUT" >&2
            fail "output did not contain: $1"
            ;;
    esac
}

assert_file_contains() {
    local file=$1
    local expected=$2
    [ -f "$file" ] || fail "expected file to exist: $file"
    grep -Fq -- "$expected" "$file" || fail "$file did not contain: $expected"
}

assert_not_exists() {
    [ ! -e "$1" ] || fail "expected path not to exist: $1"
}

new_fixture() {
    local name=$1
    local shell_path=${2:-/bin/bash}

    CASE_ROOT="$TEST_ROOT/$name"
    CASE_PROJECT="$CASE_ROOT/project"
    CASE_HOME="$CASE_ROOT/home"
    CASE_INSTALL_DIR="$CASE_ROOT/bin"
    CASE_CONFIG_DIR="$CASE_ROOT/config"
    CASE_MOCK_BIN="$CASE_ROOT/mock-bin"
    CASE_SHELL=$shell_path

    mkdir -p "$CASE_PROJECT" "$CASE_HOME" "$CASE_MOCK_BIN"
    cp "$REPO_ROOT/install.sh" "$REPO_ROOT/config.example.toml" "$CASE_PROJECT/"

    cat > "$CASE_MOCK_BIN/cargo" <<'MOCK'
#!/usr/bin/env bash
set -e
if [ "${1:-}" != "build" ] || [ "${2:-}" != "--release" ] || [ "$#" -ne 2 ]; then
    exit 91
fi
mkdir -p target/release
printf '#!/bin/sh\nexit 0\n' > target/release/incant
chmod +x target/release/incant
MOCK

    cat > "$CASE_MOCK_BIN/ollama" <<'MOCK'
#!/usr/bin/env bash
if [ "${1:-}" = "list" ]; then
    printf 'NAME ID SIZE MODIFIED\nqwen2.5-coder:7b test 0B now\n'
fi
MOCK

    cat > "$CASE_MOCK_BIN/curl" <<'MOCK'
#!/usr/bin/env bash
exit 0
MOCK

    chmod +x "$CASE_MOCK_BIN/cargo" "$CASE_MOCK_BIN/ollama" "$CASE_MOCK_BIN/curl"
}

installer_env() {
    env \
        HOME="$CASE_HOME" \
        INSTALL_DIR="$CASE_INSTALL_DIR" \
        CONFIG_DIR="$CASE_CONFIG_DIR" \
        SHELL="$CASE_SHELL" \
        PATH="$CASE_MOCK_BIN:$SYSTEM_PATH" \
        "$@"
}

run_with_null_stdin() {
    set +e
    RUN_OUTPUT=$(cd "$CASE_PROJECT" && installer_env bash ./install.sh "$@" </dev/null 2>&1)
    RUN_STATUS=$?
    set -e
}

run_with_piped_yes() {
    set +e
    RUN_OUTPUT=$(cd "$CASE_PROJECT" && printf 'y\n' | installer_env bash ./install.sh "$@" 2>&1)
    RUN_STATUS=$?
    set -e
}

run_with_prompt_answer() {
    local answer=$1
    shift
    set +e
    RUN_OUTPUT=$(cd "$CASE_PROJECT" && installer_env python3 "$PTY_HELPER" prompt "$answer" bash ./install.sh "$@" 2>&1)
    RUN_STATUS=$?
    set -e
}

run_without_controlling_tty() {
    set +e
    RUN_OUTPUT=$(cd "$CASE_PROJECT" && installer_env python3 "$PTY_HELPER" no-controlling-tty bash ./install.sh "$@" 2>&1)
    RUN_STATUS=$?
    set -e
}

new_fixture unknown-argument
run_with_null_stdin --unknown
assert_status 2
assert_output_contains "Unknown argument: --unknown"
assert_output_contains "Usage: install.sh [--with-shell-integration | --no-shell-integration]"
assert_not_exists "$CASE_PROJECT/target"
assert_not_exists "$CASE_INSTALL_DIR/incant"

for order in with-first no-first; do
    new_fixture "mutually-exclusive-$order"
    if [ "$order" = "with-first" ]; then
        run_with_null_stdin --with-shell-integration --no-shell-integration
    else
        run_with_null_stdin --no-shell-integration --with-shell-integration
    fi
    assert_status 2
    assert_output_contains "are mutually exclusive"
    assert_output_contains "Usage: install.sh [--with-shell-integration | --no-shell-integration]"
    assert_not_exists "$CASE_PROJECT/target"
done

new_fixture explicit-with-bash /bin/bash
run_with_null_stdin --with-shell-integration
assert_status 0
assert_file_contains "$CASE_HOME/.bashrc" "bind -x '\"\\C-k\": _incant_readline'"
assert_file_contains "$CASE_INSTALL_DIR/incant" "exit 0"

new_fixture explicit-with-zsh /bin/zsh
run_with_null_stdin --with-shell-integration
assert_status 0
assert_file_contains "$CASE_HOME/.zshrc" "bindkey '^k' _incant_widget"

new_fixture explicit-without
run_with_piped_yes --no-shell-integration
assert_status 0
assert_output_contains "Skipping shell integration (--no-shell-integration)"
assert_not_exists "$CASE_HOME/.bashrc"

new_fixture piped-default
run_with_piped_yes
assert_status 0
assert_output_contains "Skipping shell integration: non-interactive input detected; use --with-shell-integration to enable it"
assert_not_exists "$CASE_HOME/.bashrc"

new_fixture no-controlling-tty
run_without_controlling_tty
assert_status 0
assert_output_contains "Skipping shell integration: no controlling TTY available; use --with-shell-integration to enable it"
assert_not_exists "$CASE_HOME/.bashrc"

new_fixture interactive-prompt
run_with_prompt_answer y
assert_status 0
assert_output_contains "Install shell integration? [y/N]"
assert_output_contains "Shell integration added"
assert_file_contains "$CASE_HOME/.bashrc" "bind -x '\"\\C-k\": _incant_readline'"

printf 'installer behavior tests: PASS\n'
