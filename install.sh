#!/bin/bash
# llmcmd installation script
# Installs the binary and sets up shell integration

set -e

INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
CONFIG_DIR="${CONFIG_DIR:-$HOME/.config/llmcmd}"

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

# Check if Rust is installed
check_rust() {
    if ! command -v cargo &> /dev/null; then
        error "Rust is not installed. Install from https://rustup.rs/"
    fi
}

# Build the project
build_project() {
    info "Building llmcmd..."
    cargo build --release
}

# Install the binary
install_binary() {
    info "Installing to $INSTALL_DIR..."
    mkdir -p "$INSTALL_DIR"
    cp target/release/llmcmd "$INSTALL_DIR/"
    chmod +x "$INSTALL_DIR/llmcmd"

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
    local shell_name=$(basename "$SHELL")
    local shell_config=""
    local integration=""

    case "$shell_name" in
        zsh)
            shell_config="$HOME/.zshrc"
            integration='
# llmcmd shell integration
function _llmcmd_widget() {
    local cmd
    # Connect stdin to /dev/tty so TUI can read input
    # stdout is captured, stderr and TUI go to terminal
    cmd=$(llmcmd </dev/tty)
    if [[ -n "$cmd" ]]; then
        LBUFFER+="$cmd"
    fi
    zle redisplay
}
zle -N _llmcmd_widget
bindkey '"'"'^k'"'"' _llmcmd_widget'
            ;;
        bash)
            shell_config="$HOME/.bashrc"
            integration='
# llmcmd shell integration
_llmcmd_readline() {
    local cmd
    # Connect stdin to /dev/tty so TUI can read input
    cmd=$(llmcmd </dev/tty)
    READLINE_LINE="${READLINE_LINE}${cmd}"
    READLINE_POINT=${#READLINE_LINE}
}
bind -x '"'"'"\C-k": _llmcmd_readline'"'"''
            ;;
        fish)
            shell_config="$HOME/.config/fish/config.fish"
            integration='
# llmcmd shell integration
function _llmcmd_fish
    # Connect stdin to /dev/tty so TUI can read input
    set -l cmd (llmcmd </dev/tty)
    commandline -i $cmd
end
bind \ck _llmcmd_fish'
            ;;
        *)
            warn "Unknown shell: $shell_name"
            warn "See 'llmcmd install' for manual shell integration"
            return
            ;;
    esac

    # Check if already installed
    if grep -q "llmcmd shell integration" "$shell_config" 2>/dev/null; then
        info "Shell integration already installed in $shell_config"
        return
    fi

    # Ask user
    echo ""
    info "Shell integration for $shell_name"
    echo "This will add the following to $shell_config:"
    echo "$integration"
    echo ""
    read -p "Install shell integration? [y/N] " -n 1 -r
    echo ""

    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo "$integration" >> "$shell_config"
        info "Shell integration added to $shell_config"
        info "Restart your shell or run: source $shell_config"
    else
        info "Skipping shell integration"
        info "Run 'llmcmd install' later to see manual setup instructions"
    fi
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
        read -p "Install Ollama now? [Y/n] " -n 1 -r
        echo ""

        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
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
        read -p "Start Ollama now? [Y/n] " -n 1 -r
        echo ""

        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
            start_ollama
        else
            warn "Ollama is not running. Start with: ollama serve"
            return 0
        fi
    fi

    # Check if Ollama is running before trying to pull
    if curl -s http://localhost:11434/api/tags &> /dev/null; then
        # Offer to pull default model
        echo ""
        read -p "Pull the default model (qwen2.5-coder:7b, ~4.7GB)? [Y/n] " -n 1 -r
        echo ""

        if [[ ! $REPLY =~ ^[Nn]$ ]]; then
            pull_default_model
        else
            info "Skipping model download. Pull later with: ollama pull qwen2.5-coder:7b"
        fi
    fi
}

# Main installation flow
main() {
    echo "================================"
    echo "  llmcmd Installation"
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
    info "Start the daemon: llmcmd daemon start"
    info "Then press Ctrl+K in your shell to use llmcmd"
    echo ""
}

main "$@"
