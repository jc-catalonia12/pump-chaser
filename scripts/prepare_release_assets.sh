#!/usr/bin/env bash
# Stage config, ML models, and training database for Tauri installer bundling.
# Called by scripts/build_release.sh and CI installer workflows.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODELS_STAGING="$ROOT/release-assets/models"
DATA_STAGING="$ROOT/release-assets/data"
SRC_PRODUCTION="$ROOT/data/models/production.onnx"
SRC_SUPERVISED="$ROOT/data/models/supervised.onnx"
SRC_ONLINE="$ROOT/data/models/online_model.json"
SRC_SCHEMA="$ROOT/data/models/feature_schema.json"
SRC_METRICS="$ROOT/data/models/production.metrics.json"
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

# ── ML models (prefer V2 production.onnx; keep supervised.onnx as fallback) ───
if [[ ! -f "$SRC_PRODUCTION" && ! -f "$SRC_SUPERVISED" && -f "$SRC_DB" && -f "$EXPORT_SCRIPT" ]]; then
  if command -v python3 >/dev/null 2>&1; then
    echo "    ONNX missing — exporting from $SRC_DB"
    python3 "$EXPORT_SCRIPT" --db "$SRC_DB" --out "$SRC_SUPERVISED" || true
  fi
fi

# Prefer production.onnx; fall back to supervised.onnx and stage both names so
# older seeds and V2 settings (onnx_model_path=production.onnx) both work.
SRC_PRIMARY=""
if [[ -f "$SRC_PRODUCTION" ]]; then
  SRC_PRIMARY="$SRC_PRODUCTION"
elif [[ -f "$SRC_SUPERVISED" ]]; then
  SRC_PRIMARY="$SRC_SUPERVISED"
fi

if [[ -z "$SRC_PRIMARY" ]]; then
  echo "ERROR: Missing ONNX model. Need one of:" >&2
  echo "         $SRC_PRODUCTION" >&2
  echo "         $SRC_SUPERVISED" >&2
  exit 1
fi

cp "$SRC_PRIMARY" "$MODELS_STAGING/production.onnx"
cp "$SRC_PRIMARY" "$MODELS_STAGING/supervised.onnx"
echo "    staged production.onnx + supervised.onnx from $(basename "$SRC_PRIMARY") ($(du -h "$MODELS_STAGING/production.onnx" | cut -f1))"

if [[ -f "$SRC_SCHEMA" ]]; then
  cp "$SRC_SCHEMA" "$MODELS_STAGING/feature_schema.json"
  echo "    staged feature_schema.json"
fi
if [[ -f "$SRC_METRICS" ]]; then
  cp "$SRC_METRICS" "$MODELS_STAGING/production.metrics.json"
  echo "    staged production.metrics.json"
fi

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
    PAPER_EQ=$(awk '/^  paper_initial_equity:/ {print $2; exit}' "$SRC_CONFIG")
    if [[ -n "$PAPER_EQ" ]]; then
      sqlite3 "$DEST_DB" "UPDATE portfolio_state SET equity=${PAPER_EQ}, peak_equity=${PAPER_EQ}, daily_pnl=0, weekly_pnl=0, paper_pnl_total=0 WHERE id=1;"
      echo "    reset bundled portfolio equity → ${PAPER_EQ} USDT"
    fi
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
PRODUCTION_SHA=$(sha256_file "$MODELS_STAGING/production.onnx")
SUPERVISED_SHA=$(sha256_file "$MODELS_STAGING/supervised.onnx")
ONLINE_SHA=""
if [[ -f "$MODELS_STAGING/online_model.json" ]]; then
  ONLINE_SHA=$(sha256_file "$MODELS_STAGING/online_model.json")
fi
SCHEMA_SHA=""
if [[ -f "$MODELS_STAGING/feature_schema.json" ]]; then
  SCHEMA_SHA=$(sha256_file "$MODELS_STAGING/feature_schema.json")
fi

cat > "$MANIFEST" <<EOF
{
  "settings_sha256": "$SETTINGS_SHA",
  "db_sha256": "$DB_SHA",
  "production_onnx_sha256": "$PRODUCTION_SHA",
  "supervised_onnx_sha256": "$SUPERVISED_SHA",
  "online_model_sha256": "$ONLINE_SHA",
  "feature_schema_sha256": "$SCHEMA_SHA"
}
EOF
echo "    wrote seed.manifest (settings + training fingerprints)"

echo "==> Release assets ready in release-assets/"
