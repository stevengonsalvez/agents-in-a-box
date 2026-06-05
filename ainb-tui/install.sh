#!/bin/bash
# Install script for ainb
# Usage: curl -fsSL https://raw.githubusercontent.com/stevengonsalvez/agents-in-a-box/main/ainb-tui/install.sh | bash
#
# Options:
#   INSTALL_DIR=/custom/path  - Custom install directory (default: /usr/local/bin or ~/.local/bin)
#   VERSION=0.1.0             - Specific version to install (default: latest)

set -e

REPO="stevengonsalvez/agents-in-a-box"
BINARY_NAME="ainb"
GITHUB_API="https://api.github.com/repos/${REPO}/releases"
GITHUB_RELEASES="https://github.com/${REPO}/releases/download"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { echo -e "${BLUE}==>${NC} $1"; }
success() { echo -e "${GREEN}==>${NC} $1"; }
warn() { echo -e "${YELLOW}==>${NC} $1"; }
error() { echo -e "${RED}==>${NC} $1" >&2; exit 1; }

# Detect OS
detect_os() {
    case "$(uname -s)" in
        Darwin*) echo "darwin" ;;
        Linux*)  echo "linux" ;;
        MINGW*|MSYS*|CYGWIN*) error "Windows is not supported. Please use WSL." ;;
        *) error "Unsupported operating system: $(uname -s)" ;;
    esac
}

# Detect architecture
detect_arch() {
    case "$(uname -m)" in
        x86_64|amd64) echo "x86_64" ;;
        arm64|aarch64) echo "aarch64" ;;
        *) error "Unsupported architecture: $(uname -m)" ;;
    esac
}

# Get latest version from GitHub
get_latest_version() {
    if command -v curl &> /dev/null; then
        curl -fsSL "${GITHUB_API}/latest" | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/'
    elif command -v wget &> /dev/null; then
        wget -qO- "${GITHUB_API}/latest" | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/'
    else
        error "Neither curl nor wget found. Please install one of them."
    fi
}

# Determine install directory
get_install_dir() {
    if [ -n "${INSTALL_DIR}" ]; then
        echo "${INSTALL_DIR}"
    elif [ -w "/usr/local/bin" ]; then
        echo "/usr/local/bin"
    else
        mkdir -p "${HOME}/.local/bin"
        echo "${HOME}/.local/bin"
    fi
}

# Download file
download() {
    local url="$1"
    local output="$2"

    if command -v curl &> /dev/null; then
        curl -fsSL "$url" -o "$output"
    elif command -v wget &> /dev/null; then
        wget -q "$url" -O "$output"
    else
        error "Neither curl nor wget found."
    fi
}

# Verify checksum
verify_checksum() {
    local file="$1"
    local expected="$2"

    local actual
    if command -v sha256sum &> /dev/null; then
        actual=$(sha256sum "$file" | cut -d' ' -f1)
    elif command -v shasum &> /dev/null; then
        actual=$(shasum -a 256 "$file" | cut -d' ' -f1)
    else
        warn "Cannot verify checksum - sha256sum/shasum not found"
        return 0
    fi

    if [ "$actual" != "$expected" ]; then
        error "Checksum verification failed!\nExpected: $expected\nActual: $actual"
    fi
}

main() {
    echo ""
    echo -e "${GREEN}╔═══════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║             ainb Installer                ║${NC}"
    echo -e "${GREEN}║   Terminal UI for Claude Code Sessions    ║${NC}"
    echo -e "${GREEN}╚═══════════════════════════════════════════╝${NC}"
    echo ""

    # Detect platform
    local os=$(detect_os)
    local arch=$(detect_arch)
    info "Detected platform: ${os}-${arch}"

    # Get version
    local version="${VERSION:-$(get_latest_version)}"
    if [ -z "$version" ]; then
        error "Could not determine latest version. Set VERSION=x.x.x manually."
    fi
    info "Installing version: ${version}"

    # Build target name
    local target
    case "${os}-${arch}" in
        darwin-x86_64)  target="x86_64-apple-darwin" ;;
        darwin-aarch64) target="aarch64-apple-darwin" ;;
        linux-x86_64)   target="x86_64-unknown-linux-gnu" ;;
        linux-aarch64)
            warn "ARM64 Linux binaries are not pre-built."
            warn "Please install via cargo:"
            echo ""
            echo "  cargo install --git https://github.com/${REPO} --branch main ainb"
            echo ""
            exit 0
            ;;
        *) error "Unsupported platform: ${os}-${arch}" ;;
    esac

    # URLs
    local archive_name="${BINARY_NAME}-${version}-${target}.tar.gz"
    local download_url="${GITHUB_RELEASES}/v${version}/${archive_name}"
    local checksum_url="${download_url}.sha256"

    # Create temp directory
    local tmp_dir=$(mktemp -d)
    trap "rm -rf $tmp_dir" EXIT

    # Download
    info "Downloading ${archive_name}..."
    download "$download_url" "${tmp_dir}/${archive_name}" || error "Download failed. Check if version v${version} exists."

    # Download and verify checksum
    info "Verifying checksum..."
    download "$checksum_url" "${tmp_dir}/checksum.sha256" 2>/dev/null || warn "Checksum file not found, skipping verification"
    if [ -f "${tmp_dir}/checksum.sha256" ]; then
        local expected_checksum=$(cat "${tmp_dir}/checksum.sha256" | cut -d' ' -f1)
        verify_checksum "${tmp_dir}/${archive_name}" "$expected_checksum"
        success "Checksum verified"
    fi

    # Extract
    info "Extracting..."
    tar -xzf "${tmp_dir}/${archive_name}" -C "${tmp_dir}"

    # Install
    local install_dir=$(get_install_dir)
    info "Installing to ${install_dir}..."

    if [ -w "$install_dir" ]; then
        mv "${tmp_dir}/${BINARY_NAME}" "${install_dir}/${BINARY_NAME}"
        chmod +x "${install_dir}/${BINARY_NAME}"
    else
        sudo mv "${tmp_dir}/${BINARY_NAME}" "${install_dir}/${BINARY_NAME}"
        sudo chmod +x "${install_dir}/${BINARY_NAME}"
    fi

    # Verify installation
    if command -v "$BINARY_NAME" &> /dev/null; then
        success "Installation complete!"
        echo ""
        echo -e "  Run ${GREEN}${BINARY_NAME}${NC} to start"
        echo ""
    else
        success "Binary installed to ${install_dir}/${BINARY_NAME}"
        echo ""

        # Check if install_dir is in PATH
        if [[ ":$PATH:" != *":${install_dir}:"* ]]; then
            warn "Add ${install_dir} to your PATH:"
            echo ""
            echo "  # Add to ~/.bashrc or ~/.zshrc:"
            echo "  export PATH=\"${install_dir}:\$PATH\""
            echo ""
        fi
    fi

    echo -e "${BLUE}Documentation:${NC} https://github.com/${REPO}"
    echo ""
}

main "$@"
