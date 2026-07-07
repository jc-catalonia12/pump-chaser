#!/usr/bin/env bash
# One-time signing-key setup for the Tauri auto-updater.
#
# Run this script ONCE on any machine, then follow the printed instructions.
# Do NOT re-run after publishing a release — changing the key pair breaks
# existing installs (they cannot verify the new signatures).
#
# Prerequisites: cargo + tauri-cli installed.
#
# Usage:
#   ./scripts/setup_signing.sh
set -euo pipefail

CONF="$(cd "$(dirname "$0")/.." && pwd)/desktop/src-tauri/tauri.conf.json"
KEY_DIR="$HOME/.tauri/mexc-bot"

echo ""
echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║        MEXC Trading Bot — Tauri Updater Signing Key Setup       ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo ""

if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found. Install Rust from https://rustup.rs/" >&2
  exit 1
fi

if ! cargo tauri --version >/dev/null 2>&1; then
  echo "==> Installing tauri-cli (one-time)..."
  cargo install tauri-cli --version "^2.0.0" --locked
fi

if [[ -f "$KEY_DIR/private.key" ]]; then
  echo "⚠️  A key pair already exists at $KEY_DIR"
  echo "   If you really want to regenerate (BREAKS existing installs!), delete the"
  echo "   directory first:  rm -rf \"$KEY_DIR\""
  echo ""
  PUBKEY=$(cat "$KEY_DIR/public.key" 2>/dev/null || echo "")
else
  echo "==> Generating signing key pair at $KEY_DIR ..."
  mkdir -p "$KEY_DIR"
  # The signer generates two files: private.key and public.key
  # We pass a fixed output path so the files are predictable.
  cargo tauri signer generate --output-path "$KEY_DIR"
  PUBKEY=$(cat "$KEY_DIR/public.key")
  echo ""
  echo "✅ Key pair generated."
fi

PRIVKEY=$(cat "$KEY_DIR/private.key")

echo ""
echo "════════════════════════════════════════════════════════════════════"
echo ""
echo "📋  Step 1 — Paste the PUBLIC KEY into tauri.conf.json"
echo ""
echo "   File: $CONF"
echo "   Key in JSON: plugins > updater > pubkey"
echo ""
echo "   Public key:"
echo ""
echo "$PUBKEY"
echo ""
echo "════════════════════════════════════════════════════════════════════"
echo ""
echo "🔐  Step 2 — Add secrets to your GitHub repository"
echo ""
echo "   Go to: GitHub → your repo → Settings → Secrets → Actions"
echo ""
echo "   Secret name                       Value"
echo "   ───────────────────────────────── ────────────────────────────────"
echo "   TAURI_SIGNING_PRIVATE_KEY         (contents of $KEY_DIR/private.key)"
echo "   TAURI_SIGNING_PRIVATE_KEY_PASSWORD (empty if you set no password above)"
echo "   TAURI_UPDATER_PUBKEY              (same public key as above)"
echo "   TAURI_UPDATE_ENDPOINT             https://github.com/OWNER/REPO/releases/latest/download/latest.json"
echo "   SUPERVISED_ONNX_B64               (optional — base64 of data/models/supervised.onnx)"
echo ""
echo "   To base64-encode the private key:"
echo "     macOS:  base64 -i \"$KEY_DIR/private.key\" | pbcopy"
echo "     Linux:  base64 -w0 \"$KEY_DIR/private.key\""
echo ""
echo "════════════════════════════════════════════════════════════════════"
echo ""
echo "🚀  Step 3 — Update tauri.conf.json endpoint"
echo ""
echo "   Also replace the endpoint placeholder in tauri.conf.json:"
echo "     plugins > updater > endpoints"
echo "   with:"
echo "     https://github.com/YOUR_GITHUB_USERNAME/mexc-trading-bot-rust/releases/latest/download/latest.json"
echo ""
echo "════════════════════════════════════════════════════════════════════"
echo ""
echo "🏷️  Step 4 — Release a new version"
echo ""
echo "   # Bump the version (patch: 0.1.6 → 0.1.7):"
echo "   bash scripts/bump_version.sh"
echo ""
echo "   # Commit, tag, and push:"
echo "   git add -A && git commit -m 'chore: release v\$(cat VERSION)'"
echo "   git tag -a \"v\$(cat VERSION)\" -m \"Release v\$(cat VERSION)\""
echo "   git push && git push --tags"
echo ""
echo "   ➜ The GitHub Actions release.yml workflow starts automatically."
echo "     It builds, signs, and publishes the installers + latest.json."
echo "     Existing installs check for updates on next launch."
echo ""
echo "════════════════════════════════════════════════════════════════════"
echo ""
echo "⚠️  Keep your private key SAFE and out of the repository."
echo "    If it leaks, regenerate — but this will break existing installs"
echo "    until users reinstall from scratch."
echo ""
