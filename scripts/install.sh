#!/usr/bin/env sh
# shulker installer for Linux / macOS.
#
# What it does:
#   1. Detects (uname -s, uname -m) → release-asset triple.
#   2. Asks GitHub for the latest tag.
#   3. Downloads the matching .tar.gz, extracts it, drops `shulker` into
#      $SHULKER_INSTALL_DIR (default: ~/.local/bin).
#   4. Tells you to add the dir to PATH if it isn't already there.
#
# Env vars:
#   SHULKER_INSTALL_DIR  install dir (default: ~/.local/bin)
#   SHULKER_VERSION      pin a release tag (default: latest)
#   MC_TUI_INSTALL_DIR / MC_TUI_VERSION are accepted as fallbacks for
#   pre-rename users — new docs should use SHULKER_*.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/NihilDigit/shulker/main/scripts/install.sh | sh
#   SHULKER_INSTALL_DIR=/usr/local/bin curl -fsSL ... | sudo sh

set -e

REPO="NihilDigit/shulker"
INSTALL_DIR="${SHULKER_INSTALL_DIR:-${MC_TUI_INSTALL_DIR:-$HOME/.local/bin}}"

# 1. Platform detection
os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
    Linux/x86_64)              triple="x86_64-unknown-linux-gnu" ;;
    Linux/aarch64|Linux/arm64) triple="aarch64-unknown-linux-gnu" ;;
    Darwin/x86_64)             triple="x86_64-apple-darwin" ;;
    Darwin/arm64|Darwin/aarch64) triple="aarch64-apple-darwin" ;;
    *)
        echo "✗ unsupported platform: $os/$arch" >&2
        echo "  Supported: Linux x86_64/aarch64, macOS x86_64/aarch64." >&2
        exit 1
        ;;
esac

# 2. Latest tag via GitHub API (no jq dependency — grep + sed do the parse)
echo "→ resolving latest shulker release for $triple..."
api="https://api.github.com/repos/$REPO/releases/latest"
tag="$(curl -fsSL "$api" \
    | grep -m1 '"tag_name":' \
    | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
if [ -z "$tag" ]; then
    echo "✗ failed to resolve latest tag from $api" >&2
    echo "  Pass SHULKER_VERSION=vX.Y.Z to override." >&2
    exit 1
fi
# Allow explicit pin: SHULKER_VERSION=v1.0.0 sh -c "$(curl ... )"
tag="${SHULKER_VERSION:-${MC_TUI_VERSION:-$tag}}"
echo "→ tag: $tag"

# 3. Download + extract
asset="shulker-$tag-$triple.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "→ downloading $url"
curl -fSL --progress-bar "$url" -o "$tmp/$asset"
tar -xzf "$tmp/$asset" -C "$tmp"
extracted_dir="$tmp/shulker-$tag-$triple"
if [ ! -x "$extracted_dir/shulker" ]; then
    echo "✗ archive is missing the shulker binary at $extracted_dir/shulker" >&2
    exit 1
fi

# 4. Install
mkdir -p "$INSTALL_DIR"
install -m 0755 "$extracted_dir/shulker" "$INSTALL_DIR/shulker"
echo "✓ installed: $INSTALL_DIR/shulker"

# 5. PATH check
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo
        echo "⚠ $INSTALL_DIR is not in your PATH."
        echo "  Add this to your shell rc (~/.bashrc, ~/.zshrc, ~/.config/fish/config.fish, ...):"
        echo "    export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

echo
echo "Run:"
echo "  shulker --server-dir /path/to/your/server"
echo "  shulker new /path/to/fresh/server-dir   # scaffold a new Paper/Purpur server"
