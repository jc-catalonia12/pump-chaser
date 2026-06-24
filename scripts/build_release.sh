#!/usr/bin/env bash
# One-shot release build: bump version, bundle dev assets, produce installer.
#
# Bundles into the .dmg / .msi:
#   - config/settings.yaml   (your strategy / risk / ML settings)
#   - data/models/*          (supervised.onnx + online_model.json)
#   - data/mexc_trading_bot.db (signals, training history, positions)
#
# Version: auto-bumps patch in VERSION (0.1.0 → 0.1.1) each run unless --no-bump.
#
# Usage (from repo root):
#   ./scripts/build_release.sh
#   ./scripts/build_release.sh --no-bump
#   ./scripts/build_release.sh --set-version 0.2.0
#   ./scripts/build_release.sh -- --target aarch64-apple-darwin
#
# Output: dist/macos/ or dist/windows/
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/desktop/src-tauri/target}"

NO_BUMP=0
BUILD_ARGS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-bump)
      NO_BUMP=1
      shift
      ;;
    --set-version)
      if [[ $# -lt 2 ]]; then
        echo "ERROR: --set-version requires a value (e.g. 0.2.0)" >&2
        exit 1
      fi
      printf '%s\n' "$2" > "$ROOT/VERSION"
      NO_BUMP=1
      shift 2
      ;;
    --)
      shift
      BUILD_ARGS=("$@")
      break
      ;;
    *)
      BUILD_ARGS+=("$1")
      shift
      ;;
  esac
done

if [[ $NO_BUMP -eq 1 ]]; then
  bash "$ROOT/scripts/sync_version.sh"
else
  bash "$ROOT/scripts/bump_version.sh"
fi
RELEASE_VERSION="$(tr -d ' \n\r' < "$ROOT/VERSION")"

bash "$ROOT/scripts/prepare_release_assets.sh"

if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found — install Rust from https://rustup.rs/" >&2
  exit 1
fi

if ! cargo tauri --version >/dev/null 2>&1; then
  echo "==> Installing tauri-cli..."
  cargo install tauri-cli --version "^2.0.0" --locked
fi

echo "==> Building installer v$RELEASE_VERSION (release)..."
cd "$ROOT/desktop"
if ((${#BUILD_ARGS[@]} > 0)); then
  cargo tauri build "${BUILD_ARGS[@]}"
else
  cargo tauri build
fi

BUNDLE="$ROOT/desktop/src-tauri/target/release/bundle"

if [[ "$(uname -s)" == "Darwin" ]]; then
  DIST="$ROOT/dist/macos"
  mkdir -p "$DIST"
  # Wipe all previous artifacts so only the latest version remains.
  rm -f  "$DIST/"*.dmg
  rm -rf "$DIST/"*.app
  if compgen -G "$BUNDLE/dmg/"*.dmg >/dev/null 2>&1; then
    cp -f "$BUNDLE/dmg/"*.dmg "$DIST/"
  fi
  if compgen -G "$BUNDLE/macos/"*.app >/dev/null 2>&1; then
    cp -R "$BUNDLE/macos/"*.app "$DIST/"
  fi
else
  DIST="$ROOT/dist/linux"
  mkdir -p "$DIST"
  rm -f "$DIST/"*.deb "$DIST/"*.AppImage
  if compgen -G "$BUNDLE/deb/"*.deb >/dev/null 2>&1; then
    cp -f "$BUNDLE/deb/"*.deb "$DIST/"
  fi
  if compgen -G "$BUNDLE/appimage/"*.AppImage >/dev/null 2>&1; then
    cp -f "$BUNDLE/appimage/"*.AppImage "$DIST/"
  fi
fi

echo ""
echo "==> Build complete — v$RELEASE_VERSION"
echo "    Installers: $DIST"
ls -lh "$DIST" 2>/dev/null || ls -lh "$BUNDLE" 2>/dev/null || true
echo ""
echo "Fresh installs seed dev settings, models, and training DB on first launch."
echo "Rebuilds sync when seed.manifest changes (settings always; DB/models when safe)."
echo "API keys (secrets.json) are never bundled — users enter them in the Account tab."
