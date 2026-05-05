#!/bin/bash
# Build release binary and create distributable package

set -e

VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
BINARY_NAME="ainb"
TARGET_DIR="target/release"
DIST_DIR="dist"

echo "🔨 Building ainb v${VERSION}..."

# Build release binary
cargo build --release

# Create dist directory
mkdir -p "$DIST_DIR"

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case $ARCH in
    x86_64) ARCH="amd64" ;;
    arm64|aarch64) ARCH="arm64" ;;
esac

ARCHIVE_NAME="${BINARY_NAME}-${VERSION}-${OS}-${ARCH}"

echo "📦 Creating distribution package: ${ARCHIVE_NAME}"

# Create tarball
cd "$TARGET_DIR"
tar -czf "../../${DIST_DIR}/${ARCHIVE_NAME}.tar.gz" "$BINARY_NAME"
cd - > /dev/null

# Create checksum
cd "$DIST_DIR"
shasum -a 256 "${ARCHIVE_NAME}.tar.gz" > "${ARCHIVE_NAME}.tar.gz.sha256"
cd - > /dev/null

echo ""
echo "✅ Build complete!"
echo ""
echo "📍 Binary: ${TARGET_DIR}/${BINARY_NAME}"
echo "📦 Archive: ${DIST_DIR}/${ARCHIVE_NAME}.tar.gz"
echo "🔐 Checksum: ${DIST_DIR}/${ARCHIVE_NAME}.tar.gz.sha256"
echo ""
echo "To install locally:"
echo "  cargo install --path ."
echo ""
echo "Or copy binary directly:"
echo "  sudo cp ${TARGET_DIR}/${BINARY_NAME} /usr/local/bin/"
