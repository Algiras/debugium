#!/usr/bin/env bash
# Build WASM, sync to dist, and stamp index.html with content hashes.
# Usage: ./scripts/sync-ui.sh
set -euo pipefail

DIST="crates/debugium-ui/dist"
PKG="crates/debugium-ui/pkg"

# 1. Copy wasm-pack outputs to dist
cp "$PKG/debugium_ui.js"       "$DIST/pkg/"
cp "$PKG/debugium_ui_bg.wasm"  "$DIST/pkg/"

# 2. Compute short content hashes (first 8 chars of sha256)
hash_file() { shasum -a 256 "$1" | cut -c1-8; }

STYLE_HASH=$(hash_file "$DIST/style.css")
CM_HASH=$(hash_file "$DIST/pkg/cm_init.js")
JS_HASH=$(hash_file "$DIST/pkg/debugium_ui.js")
WASM_HASH=$(hash_file "$DIST/pkg/debugium_ui_bg.wasm")

# 3. Stamp template → index.html
sed -e "s/__STYLE_HASH__/$STYLE_HASH/g" \
    -e "s/__CM_HASH__/$CM_HASH/g" \
    -e "s/__JS_HASH__/$JS_HASH/g" \
    -e "s/__WASM_HASH__/$WASM_HASH/g" \
    "$DIST/index.html.template" > "$DIST/index.html"

echo "Synced dist with hashes: style=$STYLE_HASH cm=$CM_HASH js=$JS_HASH wasm=$WASM_HASH"
