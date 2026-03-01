#!/usr/bin/env bash
set -euo pipefail

REPO="Algiras/debugium"
INSTALL_DIR="${DEBUGIUM_INSTALL_DIR:-$HOME/.local/bin}"
BASE_URL="https://github.com/${REPO}/releases/latest/download"

# ── Detect OS / arch ─────────────────────────────────────────────────────────
OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64)  ARTIFACT="debugium-linux-x86_64" ;;
      aarch64) ARTIFACT="debugium-linux-aarch64" ;;
      arm64)   ARTIFACT="debugium-linux-aarch64" ;;
      *)       echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
    esac
    EXT="tar.gz"
    ;;
  Darwin)
    case "$ARCH" in
      x86_64) ARTIFACT="debugium-macos-x86_64" ;;
      arm64)  ARTIFACT="debugium-macos-aarch64" ;;
      *)      echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
    esac
    EXT="tar.gz"
    ;;
  *)
    echo "Unsupported OS: $OS. On Windows, download from:" >&2
    echo "  https://github.com/${REPO}/releases/latest" >&2
    exit 1
    ;;
esac

FILENAME="${ARTIFACT}.${EXT}"
URL="${BASE_URL}/${FILENAME}"

# ── Download ──────────────────────────────────────────────────────────────────
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "Downloading Debugium from ${URL} ..."
if command -v curl &>/dev/null; then
  curl -fSL "$URL" -o "$TMP/$FILENAME"
elif command -v wget &>/dev/null; then
  wget -q "$URL" -O "$TMP/$FILENAME"
else
  echo "Error: curl or wget is required." >&2
  exit 1
fi

# ── Extract ───────────────────────────────────────────────────────────────────
echo "Extracting ..."
tar -xzf "$TMP/$FILENAME" -C "$TMP"

# ── Install binary ────────────────────────────────────────────────────────────
mkdir -p "$INSTALL_DIR"
mv "$TMP/debugium" "$INSTALL_DIR/debugium"
chmod +x "$INSTALL_DIR/debugium"

# ── Install dist assets ───────────────────────────────────────────────────────
# The tarball nests dist under crates/debugium-ui/dist/
ASSETS_DIR="${DEBUGIUM_ASSETS_DIR:-$HOME/.debugium/dist}"
DIST_SRC="$TMP/crates/debugium-ui/dist"
if [ -d "$DIST_SRC" ]; then
  mkdir -p "$ASSETS_DIR"
  cp -r "$DIST_SRC/." "$ASSETS_DIR/"
  echo "UI assets installed to: $ASSETS_DIR"
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo ""
echo "✓ Debugium installed to: $INSTALL_DIR/debugium"
echo ""

# Shell PATH hint
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
  echo "Add to your shell profile:"
  echo '  export PATH="'"$INSTALL_DIR"':$PATH"'
  echo ""
fi

echo "Usage:"
echo "  debugium launch <program.py> --adapter python"
echo "  debugium launch <program.js> --adapter node"
echo "  debugium launch <binary>     --adapter rust"
echo ""
echo "Docs: https://github.com/${REPO}#readme"
