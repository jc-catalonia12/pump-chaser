#!/usr/bin/env bash
# Stage ML model files for Tauri installer bundling.
# Called automatically by build_installers.sh / build_installers.ps1.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STAGING="$ROOT/release-assets/models"
SRC_ONNX="$ROOT/data/models/supervised.onnx"
SRC_ONLINE="$ROOT/data/models/online_model.json"
DB="$ROOT/data/mexc_trading_bot.db"
EXPORT_SCRIPT="$ROOT/scripts/export_onnx.py"

mkdir -p "$STAGING"
rm -f "$STAGING"/*

echo "==> Preparing release assets (ML models)"

# Optional: export ONNX from local training DB when missing.
if [[ ! -f "$SRC_ONNX" && -f "$DB" && -f "$EXPORT_SCRIPT" ]]; then
  if command -v python3 >/dev/null 2>&1; then
    echo "    supervised.onnx missing — exporting from $DB"
    python3 "$EXPORT_SCRIPT" --db "$DB" --out "$SRC_ONNX" || true
  fi
fi

if [[ ! -f "$SRC_ONNX" ]]; then
  echo "ERROR: Missing ONNX model: $SRC_ONNX" >&2
  echo "       Train/export first, e.g.:" >&2
  echo "         python3 scripts/export_onnx.py --db data/mexc_trading_bot.db --out data/models/supervised.onnx" >&2
  echo "       Or copy your trained supervised.onnx into data/models/" >&2
  exit 1
fi

cp "$SRC_ONNX" "$STAGING/supervised.onnx"
echo "    staged supervised.onnx ($(du -h "$STAGING/supervised.onnx" | cut -f1))"

if [[ -f "$SRC_ONLINE" ]]; then
  cp "$SRC_ONLINE" "$STAGING/online_model.json"
  echo "    staged online_model.json ($(du -h "$STAGING/online_model.json" | cut -f1))"
else
  echo "    online_model.json not found — skipping (online learner starts fresh)"
fi

echo "==> Release assets ready in release-assets/models/"
