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
    local cmd=$(llmcmd)
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
    local cmd=$(llmcmd)
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
    set -l cmd (llmcmd)
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

# Check Ollama (default backend)
check_ollama() {
    if command -v ollama &> /dev/null; then
        info "Ollama found"
        if curl -s http://localhost:11434/api/tags &> /dev/null; then
            info "Ollama is running"
        else
            warn "Ollama is installed but not running"
            warn "Start with: ollama serve"
        fi
    else
        warn "Ollama not found"
        warn "Install from https://ollama.ai/ for local LLM support"
        warn "Or configure an API backend in $CONFIG_DIR/config.toml"
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
    check_ollama
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
