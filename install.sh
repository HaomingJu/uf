#!/bin/sh
set -e

REPO="HaomingJu/uf"
BIN="uf"
INSTALL_DIR="${UF_INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS and arch
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin)
    case "$ARCH" in
      arm64)  TARGET="aarch64-apple-darwin" ;;
      x86_64) TARGET="x86_64-apple-darwin" ;;
      *)      echo "Unsupported architecture: $ARCH"; exit 1 ;;
    esac
    ;;
  Linux)
    case "$ARCH" in
      aarch64|arm64) TARGET="aarch64-unknown-linux-gnu" ;;
      x86_64)        TARGET="x86_64-unknown-linux-gnu" ;;
      *)             echo "Unsupported architecture: $ARCH"; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS"
    exit 1
    ;;
esac

# Get latest version tag
VERSION="${UF_VERSION:-$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')}"

if [ -z "$VERSION" ]; then
  echo "Failed to fetch latest version. Set UF_VERSION manually: UF_VERSION=v0.1.0 ./install.sh"
  exit 1
fi

ARCHIVE="${BIN}-${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/$REPO/releases/download/$VERSION/$ARCHIVE"

echo "Installing uf $VERSION ($TARGET) -> $INSTALL_DIR/$BIN"

# Download and extract
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "$URL" -o "$TMP/$ARCHIVE"
tar -xzf "$TMP/$ARCHIVE" -C "$TMP"

# Install
mkdir -p "$INSTALL_DIR"
install -m 755 "$TMP/$BIN" "$INSTALL_DIR/$BIN"

echo "Done. Make sure $INSTALL_DIR is in your PATH."
echo "  Add to ~/.zshrc or ~/.bashrc:"
echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
