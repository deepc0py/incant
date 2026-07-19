#!/bin/bash
# incant installation script
# Installs the binary and sets up shell integration

set -e

INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
CONFIG_DIR="${CONFIG_DIR:-$HOME/.config/incant}"
SHELL_INTEGRATION_MODE="prompt"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    exit 1
}

usage() {
    printf 'Usage: %s [--with-shell-integration | --no-shell-integration]\n' "${0##*/}"
}

argument_error() {
    printf '%b\n' "${RED}[ERROR]${NC} $1" >&2
    usage >&2
    return 2
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --with-shell-integration)
                if [ "$SHELL_INTEGRATION_MODE" = "skip" ]; then
                    argument_error "--with-shell-integration and --no-shell-integration are mutually exclusive"
                    return $?
                fi
                SHELL_INTEGRATION_MODE="install"
                ;;
            --no-shell-integration)
                if [ "$SHELL_INTEGRATION_MODE" = "install" ]; then
                    argument_error "--with-shell-integration and --no-shell-integration are mutually exclusive"
                    return $?
                fi
                SHELL_INTEGRATION_MODE="skip"
                ;;
            *)
                argument_error "Unknown argument: $1"
                return $?
                ;;
        esac
        shift
    done
}

interactive_tty_available() {
    [ -t 0 ] && ( : </dev/tty ) 2>/dev/null
}

prompt_yes_no() {
    local prompt=$1
    local default_answer=$2
    local answer=""

    if ! IFS= read -r -n 1 -p "$prompt" answer </dev/tty; then
        return 1
    fi
    printf '\n' >/dev/tty

    case "$answer" in
        [Yy])
            return 0
            ;;
        [Nn])
            return 1
            ;;
        *)
            [ "$default_answer" = "yes" ]
            ;;
    esac
}


# Check if Rust is installed
check_rust() {
    if ! command -v cargo &> /dev/null; then
        error "Rust is not installed. Install from https://rustup.rs/"
    fi
}

# Build the project
build_project() {
    info "Building incant..."
    cargo build --release
}

# Install the binary
install_binary() {
    info "Installing to $INSTALL_DIR..."
    mkdir -p "$INSTALL_DIR"
    cp target/release/incant "$INSTALL_DIR/"
    chmod +x "$INSTALL_DIR/incant"

    # Check if INSTALL_DIR is in PATH
    if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
        warn "$INSTALL_DIR is not in your PATH"
        warn "Add the following to your shell config:"
        warn "  export PATH=\"\$PATH:$INSTALL_DIR\""
    fi
}

# Create default config
create_config() {
    if [ ! -f "$CONFIG_DIR/config.toml" ]; then
        info "Creating default configuration..."
        mkdir -p "$CONFIG_DIR"
        cp config.example.toml "$CONFIG_DIR/config.toml"
        info "Config created at $CONFIG_DIR/config.toml"
    else
        info "Config already exists at $CONFIG_DIR/config.toml"
    fi
}

# Setup shell integration
setup_shell_integration() {
    local shell_name
    local shell_config=""
    local integration=""
    local reply=""

    if [ "$SHELL_INTEGRATION_MODE" = "skip" ]; then
        info "Skipping shell integration (--no-shell-integration)"
        return
    fi

    if [ "$SHELL_INTEGRATION_MODE" = "prompt" ]; then
        if [ ! -t 0 ]; then
            info "Skipping shell integration: non-interactive input detected; use --with-shell-integration to enable it"
            return
        fi

        if ! ( : </dev/tty ) 2>/dev/null; then
            info "Skipping shell integration: no controlling TTY available; use --with-shell-integration to enable it"
            return
        fi
    fi

    shell_name=$(basename "$SHELL")

    case "$shell_name" in
        zsh)
            shell_config="$HOME/.zshrc"
            # Variables in this snippet must expand in the user's shell.
            # shellcheck disable=SC2016
            integration='
# incant shell integration
function _incant_widget() {
    local cmd
    # Connect stdin to /dev/tty so TUI can read input
    # stdout is captured, stderr and TUI go to terminal
    cmd=$(incant </dev/tty)
    if [[ -n "$cmd" ]]; then
        LBUFFER+="$cmd"
    fi
    zle redisplay
}
zle -N _incant_widget
bindkey '"'"'^k'"'"' _incant_widget'
            ;;
        bash)
            shell_config="$HOME/.bashrc"
            # Variables in this snippet must expand in the user's shell.
            # shellcheck disable=SC2016
            integration='
# incant shell integration
_incant_readline() {
    local cmd
    # Connect stdin to /dev/tty so TUI can read input
    cmd=$(incant </dev/tty)
    READLINE_LINE="${READLINE_LINE}${cmd}"
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"'"'"\C-k": _incant_readline'"'"''
            ;;
        fish)
            shell_config="$HOME/.config/fish/config.fish"
            # Variables in this snippet must expand in the user's shell.
            # shellcheck disable=SC2016
            integration='
# incant shell integration
function _incant_fish
    # Connect stdin to /dev/tty so TUI can read input
    set -l cmd (incant </dev/tty)
    commandline -i $cmd
end
bind \ck _incant_fish'
            ;;
        *)
            warn "Unknown shell: $shell_name"
            warn "See 'incant install' for manual shell integration"
            return
            ;;
    esac

    # Check if already installed
    if grep -q "incant shell integration" "$shell_config" 2>/dev/null; then
        info "Shell integration already installed in $shell_config"
        return
    fi

    if [ "$SHELL_INTEGRATION_MODE" = "prompt" ]; then
        echo ""
        info "Shell integration for $shell_name"
        echo "This will add the following to $shell_config:"
        echo "$integration"
        echo ""

        if ! IFS= read -r -n 1 -p "Install shell integration? [y/N] " reply </dev/tty; then
            echo ""
            info "Skipping shell integration: unable to read from /dev/tty"
            info "Use --with-shell-integration to enable it non-interactively"
            return
        fi
        echo ""

        if [[ ! $reply =~ ^[Yy]$ ]]; then
            info "Skipping shell integration"
            info "Run 'incant install' later to see manual setup instructions"
            return
        fi
    fi

    echo "$integration" >> "$shell_config"
    info "Shell integration added to $shell_config"
    info "Restart your shell or run: source $shell_config"
}

# Detect OS
detect_os() {
    case "$(uname -s)" in
        Darwin)
            OS="macos"
            ;;
        Linux)
            OS="linux"
            ;;
        *)
            OS="unknown"
            ;;
    esac
}

# Install Ollama
install_ollama() {
    info "Installing Ollama..."

    if [ "$OS" = "macos" ]; then
        # Check if Homebrew is available
        if command -v brew &> /dev/null; then
            info "Installing via Homebrew..."
            brew install ollama
        else
            info "Installing via official installer..."
            curl -fsSL https://ollama.ai/install.sh | sh
        fi
    elif [ "$OS" = "linux" ]; then
        info "Installing via official installer..."
        curl -fsSL https://ollama.ai/install.sh | sh
    else
        error "Unsupported OS for automatic Ollama installation"
    fi

    if command -v ollama &> /dev/null; then
        info "Ollama installed successfully"
        return 0
    else
        error "Failed to install Ollama"
    fi
}

# Start Ollama service
start_ollama() {
    info "Starting Ollama..."

    if [ "$OS" = "macos" ]; then
        # On macOS, check if installed via brew (has service) or standalone
        if brew services list 2>/dev/null | grep -q ollama; then
            brew services start ollama
        else
            # Start in background
            nohup ollama serve > /dev/null 2>&1 &
            sleep 2
        fi
    elif [ "$OS" = "linux" ]; then
        # Try systemd first, fall back to manual
        if systemctl is-enabled ollama &> /dev/null 2>&1; then
            sudo systemctl start ollama
        else
            nohup ollama serve > /dev/null 2>&1 &
            sleep 2
        fi
    fi

    # Verify it started
    sleep 2
    if curl -s http://localhost:11434/api/tags &> /dev/null; then
        info "Ollama is now running"
        return 0
    else
        warn "Ollama may not have started. Try running 'ollama serve' manually."
        return 1
    fi
}

# Pull default model
pull_default_model() {
    local model="qwen2.5-coder:7b"

    # Check if model already exists
    if ollama list 2>/dev/null | grep -q "$model"; then
        info "Model $model already available"
        return 0
    fi

    echo ""
    info "Pulling default model: $model"
    info "This may take a few minutes depending on your connection..."
    echo ""

    if ollama pull "$model"; then
        info "Model $model pulled successfully"
        return 0
    else
        warn "Failed to pull model. You can try later with: ollama pull $model"
        return 1
    fi
}

# Check and setup Ollama (default backend)
setup_ollama() {
    detect_os

    if command -v ollama &> /dev/null; then
        info "Ollama found"
    else
        echo ""
        info "Ollama is not installed (required for local LLM support)"
        echo ""

        if ! interactive_tty_available; then
            warn "Skipping Ollama installation: interactive terminal input is unavailable"
            warn "Configure an API backend (Anthropic/OpenAI) in $CONFIG_DIR/config.toml"
            warn "Or install Ollama later from https://ollama.ai/"
            return 0
        fi

        if prompt_yes_no "Install Ollama now? [Y/n] " yes; then
            install_ollama
        else
            warn "Skipping Ollama installation"
            warn "Configure an API backend (Anthropic/OpenAI) in $CONFIG_DIR/config.toml"
            warn "Or install Ollama later from https://ollama.ai/"
            return 0
        fi
    fi

    # Check if running
    if curl -s http://localhost:11434/api/tags &> /dev/null; then
        info "Ollama is running"
    else
        echo ""

        if ! interactive_tty_available; then
            warn "Skipping Ollama startup: interactive terminal input is unavailable"
            warn "Ollama is not running. Start with: ollama serve"
            return 0
        fi

        if prompt_yes_no "Start Ollama now? [Y/n] " yes; then
            start_ollama
        else
            warn "Ollama is not running. Start with: ollama serve"
            return 0
        fi
    fi

    # Check if Ollama is running before trying to pull
    if curl -s http://localhost:11434/api/tags &> /dev/null; then
        echo ""

        if ! interactive_tty_available; then
            info "Skipping model download: interactive terminal input is unavailable"
            info "Pull later with: ollama pull qwen2.5-coder:7b"
            return 0
        fi

        if prompt_yes_no "Pull the default model (qwen2.5-coder:7b, ~4.7GB)? [Y/n] " yes; then
            pull_default_model
        else
            info "Skipping model download. Pull later with: ollama pull qwen2.5-coder:7b"
        fi
    fi
}

# Main installation flow
main() {
    parse_args "$@"

    echo "================================"
    echo "  incant Installation"
    echo "================================"
    echo ""

    check_rust
    build_project
    install_binary
    create_config
    setup_ollama
    setup_shell_integration

    echo ""
    echo "================================"
    echo "  Installation Complete!"
    echo "================================"
    echo ""
    info "Start the daemon: incant daemon start"
    info "Then press Ctrl+K in your shell to use incant"
    echo ""
}

main "$@"
