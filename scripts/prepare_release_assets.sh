#!/usr/bin/env bash
# Stage dev config, ML models, and training database for Tauri installer bundling.
# Called by scripts/build_release.sh (and build_installers.sh).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODELS_STAGING="$ROOT/release-assets/models"
DATA_STAGING="$ROOT/release-assets/data"
SRC_ONNX="$ROOT/data/models/supervised.onnx"
SRC_ONLINE="$ROOT/data/models/online_model.json"
SRC_DB="$ROOT/data/mexc_trading_bot.db"
SRC_CONFIG="$ROOT/config/settings.yaml"
EXPORT_SCRIPT="$ROOT/scripts/export_onnx.py"

mkdir -p "$MODELS_STAGING" "$DATA_STAGING"
rm -f "$MODELS_STAGING"/*
rm -f "$DATA_STAGING"/*
rm -f "$DATA_STAGING"/mexc_trading_bot.db-*

echo "==> Preparing release assets (config, models, training DB)"

# ── Config (bundled via tauri.conf.json ../../config/**/*) ─────────────────────
if [[ ! -f "$SRC_CONFIG" ]]; then
  echo "ERROR: Missing config: $SRC_CONFIG" >&2
  exit 1
fi
echo "    config/settings.yaml ($(wc -l < "$SRC_CONFIG" | tr -d ' ') lines) — bundled with installer"

# ── ML models ────────────────────────────────────────────────────────────────
if [[ ! -f "$SRC_ONNX" && -f "$SRC_DB" && -f "$EXPORT_SCRIPT" ]]; then
  if command -v python3 >/dev/null 2>&1; then
    echo "    supervised.onnx missing — exporting from $SRC_DB"
    python3 "$EXPORT_SCRIPT" --db "$SRC_DB" --out "$SRC_ONNX" || true
  fi
fi

if [[ ! -f "$SRC_ONNX" ]]; then
  echo "ERROR: Missing ONNX model: $SRC_ONNX" >&2
  echo "       Train/export first, e.g.:" >&2
  echo "         python3 scripts/export_onnx.py --db data/mexc_trading_bot.db --out data/models/supervised.onnx" >&2
  exit 1
fi

cp "$SRC_ONNX" "$MODELS_STAGING/supervised.onnx"
echo "    staged supervised.onnx ($(du -h "$MODELS_STAGING/supervised.onnx" | cut -f1))"

if [[ -f "$SRC_ONLINE" ]]; then
  cp "$SRC_ONLINE" "$MODELS_STAGING/online_model.json"
  echo "    staged online_model.json ($(du -h "$MODELS_STAGING/online_model.json" | cut -f1))"
else
  echo "    online_model.json not found — skipping (online learner starts fresh)"
fi

sha256_file() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

if [[ -f "$SRC_DB" ]]; then
  DEST_DB="$DATA_STAGING/mexc_trading_bot.db"
  if command -v sqlite3 >/dev/null 2>&1; then
    sqlite3 "$SRC_DB" ".backup '$DEST_DB'"
    echo "    staged mexc_trading_bot.db ($(du -h "$DEST_DB" | cut -f1), checkpointed via sqlite3)"
  else
    cp "$SRC_DB" "$DEST_DB"
    echo "    staged mexc_trading_bot.db ($(du -h "$DEST_DB" | cut -f1), raw copy — install sqlite3 for safer checkpoint)"
  fi
  if command -v sqlite3 >/dev/null 2>&1; then
    SIGNALS=$(sqlite3 "$DEST_DB" "SELECT COUNT(*) FROM signals;" 2>/dev/null || echo "?")
    LEARNS=$(sqlite3 "$DEST_DB" "SELECT COUNT(*) FROM audit_log WHERE event_type='model_learn';" 2>/dev/null || echo "?")
    echo "    training data: ${SIGNALS} signals, ${LEARNS} model_learn events"
  fi
else
  echo "    WARN: No dev database at data/mexc_trading_bot.db — fresh installs will start empty"
  DEST_DB=""
fi

# ── Seed manifest (drives settings / DB / model sync on first launch after rebuild) ─
MANIFEST="$ROOT/release-assets/seed.manifest"
SETTINGS_SHA=$(sha256_file "$SRC_CONFIG")
DB_SHA=""
if [[ -n "${DEST_DB:-}" && -f "$DEST_DB" ]]; then
  DB_SHA=$(sha256_file "$DEST_DB")
fi
ONNX_SHA=$(sha256_file "$MODELS_STAGING/supervised.onnx")
ONLINE_SHA=""
if [[ -f "$MODELS_STAGING/online_model.json" ]]; then
  ONLINE_SHA=$(sha256_file "$MODELS_STAGING/online_model.json")
fi

cat > "$MANIFEST" <<EOF
{
  "settings_sha256": "$SETTINGS_SHA",
  "db_sha256": "$DB_SHA",
  "supervised_onnx_sha256": "$ONNX_SHA",
  "online_model_sha256": "$ONLINE_SHA"
}
EOF
echo "    wrote seed.manifest (settings + training fingerprints)"

echo "==> Release assets ready in release-assets/"
