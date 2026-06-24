#!/usr/bin/env bash
# Build macOS desktop installers (.app + .dmg) with bundled ONNX model.
#
# Prerequisites:
#   - Rust 1.85+ (rustup)
#   - Xcode Command Line Tools
#   - cargo install tauri-cli --version "^2.0.0"
#
# Usage (from anywhere):
#   ./scripts/build_installers.sh
#   ./scripts/build_installers.sh -- --target aarch64-apple-darwin
#
# Output:
#   dist/macos/          — copied .app + .dmg
#   desktop/src-tauri/target/release/bundle/
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Keep artifacts in-repo (Cursor/sandbox may otherwise redirect CARGO_TARGET_DIR).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/desktop/src-tauri/target}"

bash "$ROOT/scripts/prepare_release_assets.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found — install Rust from https://rustup.rs/" >&2
  exit 1
fi

if ! cargo tauri --version >/dev/null 2>&1; then
  echo "==> Installing tauri-cli..."
  cargo install tauri-cli --version "^2.0.0" --locked
fi

echo "==> Building macOS installer (release)..."
cd "$ROOT/desktop"
cargo tauri build "$@"

BUNDLE="$ROOT/desktop/src-tauri/target/release/bundle"
DIST="$ROOT/dist/macos"
mkdir -p "$DIST"

# Copy artifacts for easy distribution.
if compgen -G "$BUNDLE/dmg/"*.dmg >/dev/null 2>&1; then
  cp -f "$BUNDLE/dmg/"*.dmg "$DIST/"
fi
if compgen -G "$BUNDLE/macos/"*.app >/dev/null 2>&1; then
  rm -rf "$DIST/"*.app 2>/dev/null || true
  cp -R "$BUNDLE/macos/"*.app "$DIST/"
fi

echo ""
echo "==> Build complete"
echo "    Installers: $DIST"
ls -lh "$DIST" 2>/dev/null || ls -lh "$BUNDLE/dmg" "$BUNDLE/macos" 2>/dev/null || true
