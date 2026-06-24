# MEXC Trading Bot — Rust Migration Tracker

> Renamed from **Pump Chaser** → **MEXC Trading Bot**

## Step 1 — Setup ✓

- [x] Cargo workspace, modules, Axum API `:8001`, SQLite, RiskManager

## Step 2 — Port critical modules ✓

- [x] Config & DB layer
- [x] RiskManager (+ kill switch, open from signal)
- [x] MEXC Exchange Client (REST + WebSocket)
- [x] Confluence signal engine (indicators, zones, scoring)
- [x] Paper execution (SL, time exit, PnL)
- [x] API route parity (44 HTTP + `/ws`)
- [x] Scanner loop (kline refresh + ticker processing)

## Step 3 — ML integration ✓ (inference)

- [x] Technical feature builder (10-dim, matches Python `FEATURE_COLUMNS`)
- [x] ONNX inference via `tract-onnx` + hard ML gate
- [x] Export script: `scripts/export_onnx.py`
- [ ] PyO3 training bridge (`--features ml-python`)

## Step 4 — Live trading ✓ (private API)

- [x] MEXC private REST client (HMAC signing, orders, positions, wallet)
- [x] `LiveTrader` (open/close, dry-run, leverage, rollback)
- [x] Credential storage (`data/secrets.json` or env vars)
- [x] Wallet + re-anchor endpoints

## Step 5 — Dashboard ✓ (Phase A)

- [x] Built-in web UI at `http://127.0.0.1:8001/` — Trading, Signals, Positions, Account
- [x] WebSocket live updates + HTTP fallback
- [x] Phase B: Readiness, P&L charts, TradingView + Lightweight Charts overlays
- [x] Phase C: Tauri desktop shell (`desktop/src-tauri`)
- [ ] Streamlit optional via `MEXC_BOT_API_URL`

## Step 6 — Validation

- [ ] Side-by-side Python vs Rust signal comparison
- [ ] `proptest` risk invariants

## Still stubbed / Python-only

- Backtest / walk-forward / training scheduler
- Installer build endpoints
- Pump + scalp strategies (stubs remain)
- In-process ML training (use Python export → ONNX)

## Credentials

```bash
export MEXC_API_KEY=...
export MEXC_API_SECRET=...
export LIVE_TRADING=false   # set true only when ready
```

Or save via `PUT /user/credentials` (dashboard) → `data/secrets.json`.

## ONNX model

```bash
source venv/bin/activate
pip install -r requirements.txt
python scripts/export_onnx.py --db data/mexc_trading_bot.db
```

Enable hard gate in `config/settings.yaml`: `ml.hard_ml_gate: true`

## Run

```bash
cargo run
curl -X POST http://127.0.0.1:8001/trading/start
```
