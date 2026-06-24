# MEXC Trading Bot (Rust)

High-performance **MEXC USDT-M perpetual futures** trading bot — Rust migration of the Python Pump Chaser stack. Includes a built-in web dashboard, native desktop app (Tauri), **confluence** and **volume pump** strategies, live/paper execution, ONNX + online ML gating, shadow learning, settings hot-reload, and optional Telegram alerts.

## Contents

- [Architecture](#architecture)
- [Trading strategies](#trading-strategies)
- [First-time setup](#first-time-setup)
- [Dashboard tabs](#dashboard-tabs)
- [Configuration & data locations](#configuration)
- [Desktop app & installers](#desktop-app--installers)
- [GitHub Actions CI](#github-actions-free-tier)
- [API quick reference](#api-quick-reference)
- [Project layout](#project-layout)
- [Feature status](#feature-status)

---

## Architecture

```
Desktop app (Tauri) or browser  →  http://127.0.0.1:8001
                    ↓ HTTP + WebSocket
         Axum API (this project)
                    ↓
    Scanner · Signals · Risk · Execution · ML
                    ↓
    MEXC REST + WebSocket (public + private API)
```

**User data** (API keys, SQLite, trade history) lives outside the installer in a local data folder and is never bundled in builds.

**Trained ML models** (`supervised.onnx`, optional `online_model.json`) are bundled in release installers and copied into the user data folder on first launch (existing user models are not overwritten).

---

## Trading strategies

The bot can run one or both strategies in parallel. Set `trading.mode` in **Settings** or `config/settings.yaml`:

| Mode | Strategies active |
|------|-------------------|
| `confluence` | Confluence only |
| `pump` / `volume_pump` | Volume pump only |
| `both` | Confluence + volume pump |
| `all` | Confluence + volume pump (default) |
| `scalp` | Scalp stub (disabled unless `scalp.enabled: true`) |

Each strategy has **separate position slots** (`risk.max_confluence_positions`, `risk.max_volume_pump_positions`) so a full confluence book does not block pump entries and vice versa.

### Confluence

15m-style multi-factor setups on 1m data: volume, supply/demand zones, structure, market bias, optional HTF alignment (15m/30m), and liquidity-grab detection. Entries can use **sniper** (1m pin-bar trigger after HTF setup), **limit**, or **market** via `sniper.entry_mode`.

### Volume pump

Fast 1m **volume-anomaly** scanner ranked by universe turnover velocity. Designed for short holds with limit or market entry (`pump.entry_mode`).

**Two-phase confirmation** (`pump.confirmation_enabled`, default `true`) reduces fake pumps:

1. **Arm** — abnormal volume surge + score gates set a pending setup (TTL: `pump.confirmation_ttl_sec`, default 180s).
2. **Confirm** — entry fires only when gates pass:
   - **Breakout** (close beyond prior range + volume) **or** **market shift** (1m structure + bias)
   - **1m structure** and **market bias** (when enabled)
   - **Symbol HTF bias** (15m default, `pump.htf_enabled`)
   - **BTC/ETH macro** — blocks long pumps if either major is clearly dumping on HTF; blocks shorts if either is clearly pumping (`pump.macro_filter_enabled`)

Tune under **Settings → Volume Pump Strategy** or in `config/settings.yaml` under `pump:`.

### Live execution notes

- MEXC `vol` is **contracts**, not coin quantity — the bot converts using per-symbol `contractSize` and clamps to `maxVol`.
- Open positions show **strategy**, **entry type** (market / limit / sniper), and **pending** for unfilled limit orders.
- **Settings saved in the UI apply immediately** (scanner, risk, execution) without restarting the app. MEXC WebSocket URL changes still require a scanner stop/start.

---

## First-time setup

Follow these steps to go from zero to a running bot. All commands below assume you are in **this repository root**.

### 1. Install prerequisites

| Requirement | Notes |
|---------------|--------|
| [Rust](https://rustup.rs/) **1.85+** | `rustup update stable` |
| **macOS:** Xcode Command Line Tools | `xcode-select --install` |
| **Windows:** [VS C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) | Required only for building desktop installers |

Optional (only for exporting a bootstrap ONNX model via `scripts/export_onnx.py`):

```bash
python3 -m venv venv
source venv/bin/activate          # Windows: venv\Scripts\activate
pip install -r requirements.txt
```

Packages: `scikit-learn`, `skl2onnx`, `onnx`, `onnxruntime` (see `requirements.txt`).

### 2. Run the app

```bash
cargo run
```

This starts the **API server** and opens the **desktop window** with the dashboard at **http://127.0.0.1:8001/**.

| Command | What you get |
|---------|----------------|
| `cargo run` | **Default** — API + desktop window |
| `cargo run -p mexc-trading-bot` | API only (headless) — open http://127.0.0.1:8001/ in a browser |
| `cd desktop && cargo tauri dev` | Desktop with Tauri hot reload (see [Tauri dev](#tauri-dev-optional)) |

Verify the server:

```bash
curl http://127.0.0.1:8001/health
```

### 3. Connect MEXC API keys

1. Open the **Account** tab.
2. Under **API Credentials**, paste your MEXC Futures API key and secret.
3. Click **Connect account**.

Keys are saved to `data/secrets.json` in development, or to the user data folder when installed from a `.dmg` / `.msi` (see [Data locations](#data-locations)).

**MEXC key permissions:** enable **Futures / contract trading** read + trade. Do not enable withdrawal.

Alternative (env vars, useful for CI or headless):

```bash
export MEXC_API_KEY=your_key
export MEXC_API_SECRET=your_secret
```

### 4. Choose paper or live mode

Still on **Account → Execution Mode**:

| Mode | Behavior |
|------|----------|
| **Paper** | Simulated fills against live market data. Default for new installs. |
| **Live** | Real orders on MEXC. Requires valid API keys and `execution.live_trading_enabled: true` in config. |

Start with **Paper** until signals, risk limits, and wallet sync look correct.

For live trading, also review in `config/settings.yaml`:

```yaml
execution:
  live_trading_enabled: true   # master switch
  dry_run: false               # true = log orders without sending
  sync_exchange_positions: true
```

Use **Re-anchor from MEXC wallet** on the Account tab to sync paper/live equity from the exchange.

### 5. Start the scanner

1. Go to the **Trading** tab.
2. Click **Start trading** (or `POST /trading/start`).

The scanner polls USDT-M symbols, scores **confluence** and/or **volume pump** setups (per `trading.mode`), applies the ML gate, and opens positions when risk checks pass. Status appears on Trading (live snapshot) and **Signals** (paginated history). Volume pump scan messages use `[pump]` prefixes (e.g. armed, waiting breakout, macro block).

### 6. (Optional) Bootstrap the ML model

The bot uses a **supervised setup classifier** (ONNX) plus an **online learner** that improves from every resolved outcome.

**Automatic:** Once enough signals resolve (win / loss / expired), the online model trains in Rust — no Python required. Check progress on the **Training** tab.

**Optional cold start** — export an ONNX model from historical signal data:

```bash
# With venv active (pip install -r requirements.txt):
python scripts/export_onnx.py
# Or point at a specific DB:
python scripts/export_onnx.py --db data/mexc_trading_bot.db --out data/models/supervised.onnx
```

Output goes to `data/models/supervised.onnx` (path set by `ml.onnx_model_path` in config). Restart the bot after placing the file.

Key ML settings in `config/settings.yaml`:

```yaml
ml:
  enabled: true
  supervised_enabled: true
  supervised_threshold: 0.58   # minimum setup probability to pass gate
  hard_ml_gate: true           # block trades below threshold
  min_training_samples: 100
learning:
  enabled: true
  shadow_ml_rejects: true      # learn from ML-rejected setups (no trade)
  shadow_near_miss: true       # learn from near-miss confluence scores
```

### 7. (Optional) Telegram notifications

Get trade alerts and query bot status from Telegram.

1. Create a bot with [@BotFather](https://t.me/BotFather) and copy the **bot token**.
2. Find your **chat ID** (e.g. via [@userinfobot](https://t.me/userinfobot)).
3. On **Account → Telegram Notifications**, paste token + chat ID → **Connect Telegram**.
4. Open Telegram, chat with your bot, and press **Start** (required before messages can be delivered).
5. Click **Send test message** to confirm.
6. Toggle which events to receive (open, close, TP, SL, kill switch, volume pump armed/detected).

Trade alerts include the **strategy** label (Confluence, Volume Pump, etc.).

**Bot commands** (only from your configured chat):

| Command | Description |
|---------|-------------|
| `/info` | Wallet, open positions, scanner status, risk summary |
| `/start` | Welcome + short help |
| `/help` | Command list |

Telegram credentials are stored in `secrets.json` alongside MEXC keys — not in `settings.yaml`.

### 8. Review risk & settings

Use the **Settings** tab (or edit `config/settings.yaml`) for:

- **Trading mode** (`trading.mode`) and per-strategy position caps
- Confluence thresholds (`min_composite_score`, HTF filter, liquidity grab)
- Volume pump confirmation gates (`pump.confirmation_enabled`, breakout/macro/HTF filters)
- Circuit breakers (`max_consecutive_losses`, `loss_streak_cooldown_sec`, `max_drawdown_halt_pct`)
- Max hold time (`confluence.max_hold_sec`, `pump.max_hold_sec`) — signals that never hit TP or SL within this window are labeled **expired**
- Kill switch — Trading tab or `POST /kill-switch/activate`

Changes saved from the Settings tab are written to `settings.yaml` and **reloaded live** by the scanner, risk manager, and execution layer.

---

## Dashboard tabs

| Tab | Purpose |
|-----|---------|
| **Trading** | Start/stop bot, live snapshot, recent signals preview, activity feed |
| **Signals** | Full signal history (server-side pagination, 25 per page), chart overlay |
| **Positions** | Open positions (strategy, entry type, pending limits), **History** (closed trades, All/Live/Paper filter), manual close, P&amp;L chart |
| **Training** | ML stack status, outcome trends, shadow learning stats, win rate by side |
| **Account** | Paper/live mode, wallet, MEXC keys, Telegram |
| **Settings** | Edit `settings.yaml` fields from the UI |

---

## Configuration

| Item | Location |
|------|----------|
| Strategy & risk defaults | `config/settings.yaml` |
| API keys & Telegram | `data/secrets.json` (UI: Account tab) |
| Override config path | `MEXC_BOT_CONFIG=/path/to/settings.yaml` |
| Env overrides | `MEXC_BOT_*` with `__` for nesting, e.g. `MEXC_BOT_SERVER__PORT=9000` |
| Secrets path override | `MEXC_BOT_SECRETS_PATH=/path/to/secrets.json` |

### Data locations

| Context | Config | Secrets | Database | ML models |
|---------|--------|---------|----------|-----------|
| **Dev** (`cargo run`) | `config/settings.yaml` | `data/secrets.json` | `data/mexc_trading_bot.db` | `data/models/` |
| **macOS app** | `~/Library/Application Support/MEXC Trading Bot/config/settings.yaml` | same folder / `secrets.json` | same folder / `data/` | same folder / `models/` |
| **Windows app** | `%LOCALAPPDATA%\MEXC Trading Bot\` | (same structure) | | |

On first launch from an installer, default config and bundled ML models are copied into the user data folder. API keys and trade history are never included in the build.

---

## Desktop app & installers

The Tauri desktop shell wraps the same web UI and starts the API server on port **8001**. End users can install a `.dmg` (macOS) or `.msi` / setup `.exe` (Windows) without installing Rust.

### Tauri dev (optional)

For hot reload while working on the desktop shell:

```bash
cargo install tauri-cli --version "^2.0.0"
cd desktop && cargo tauri dev
```

If the API is already running on `:8001`, Tauri attaches to it instead of starting a duplicate server.

Saving API credentials writes to the user data directory (packaged) or `data/secrets.json` (dev). A `.taurignore` at the workspace root excludes `data/` so `cargo tauri dev` does not restart the app when credentials change.

### Build prerequisites (installer builds only)

| Platform | Install |
|----------|---------|
| **All** | [Rust](https://rustup.rs/) 1.85+, `cargo install tauri-cli --version "^2.0.0"` |
| **macOS** | Xcode Command Line Tools (`xcode-select --install`) |
| **Windows** | [Visual Studio C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) |

**Important:** Build on each target OS locally — you **cannot** produce a Windows installer from macOS (or vice versa) with the scripts below. Use [GitHub Actions](#github-actions-free-tier) to build Windows from a Mac via CI.

### Local build (with bundled ML models)

1. Ensure `data/models/supervised.onnx` exists (train online or export via `scripts/export_onnx.py`).
2. Run the platform script from the **repo root**:

**macOS / Linux:**

```bash
chmod +x scripts/*.sh
./scripts/build_installers.sh
```

**Windows (PowerShell):**

```powershell
.\scripts\build_installers.ps1
```

The scripts:

1. Copy `data/models/supervised.onnx` (+ optional `online_model.json`) into `release-assets/models/`
2. Run `cargo tauri build --release`
3. Copy artifacts to `dist/macos/` or `dist/windows/`

Manual build (without the wrapper script):

```bash
./scripts/prepare_release_assets.sh
cd desktop && cargo tauri build --release
```

### Installer output

| Location | Contents |
|----------|----------|
| `dist/macos/` | Convenience copies: `.dmg`, `.app` |
| `dist/windows/` | Convenience copies: `.msi`, NSIS `*-setup.exe` |
| `desktop/src-tauri/target/release/bundle/` | Full Tauri output (same files) |

Typical artifact names:

| OS | Files |
|----|--------|
| **macOS** | `dmg/MEXC Trading Bot_0.1.0_*.dmg`, `macos/MEXC Trading Bot.app` |
| **Windows** | `msi/*.msi`, `nsis/*-setup.exe` |

### Bundled vs user data

| Bundled in installer (read-only) | User data (never in installer) |
|----------------------------------|--------------------------------|
| `config/settings.yaml` (defaults) | API keys (`secrets.json`) |
| `web/` dashboard | SQLite database |
| `release-assets/models/` (ONNX + optional online weights) | User-retrained models (never overwritten on upgrade) |
| App binary + WebView | |

On first launch, defaults and bundled models are seeded into:

- **macOS:** `~/Library/Application Support/MEXC Trading Bot/`
- **Windows:** `%LOCALAPPDATA%\MEXC Trading Bot\`

### Distribution notes

- **macOS:** Sign and notarize with an Apple Developer ID before wide distribution (avoids Gatekeeper warnings).
- **Windows:** WebView2 bootstrapper is embedded (`embedBootstrapper` in `desktop/src-tauri/tauri.conf.json`). Authenticode signing reduces SmartScreen prompts.

---

## GitHub Actions (free tier)

Build macOS **and** Windows installers from a Mac by pushing to GitHub — a free account is enough.

**Workflow file:** `.github/workflows/build-installers.yml`

**Triggers:**

- Manual: **Actions → Build installers → Run workflow**
- Auto: push to `main` or `master` (code changes; markdown-only pushes are ignored)

| Job | Runner | Artifact |
|-----|--------|----------|
| `build-macos` | `macos-latest` | `macos-installer` — `.dmg` + `.app` |
| `build-windows` | `windows-latest` | `windows-installer` — `.msi` + setup `.exe` |

Both jobs run the same build scripts as local builds, so the ONNX model is bundled in the installer.

**ML model for CI** — either:

1. **Commit** `data/models/supervised.onnx` to the repo (recommended for team sharing), or
2. Set repository **Secrets** (Settings → Secrets and variables → Actions):
   - `SUPERVISED_ONNX_B64` — base64 of `supervised.onnx` (required if not committed)
   - `ONLINE_MODEL_JSON_B64` — optional base64 of `online_model.json`

Generate base64 locally:

```bash
base64 -i data/models/supervised.onnx | pbcopy          # macOS
base64 -i data/models/online_model.json | pbcopy       # optional
```

Download built installers from the workflow run → **Artifacts**.

**Minute usage (private repos):** Windows builds bill at 2×, macOS at 10×. Public repos get unlimited Actions minutes.

---

## API quick reference

```bash
curl http://127.0.0.1:8001/health
curl http://127.0.0.1:8001/risk
curl http://127.0.0.1:8001/live/snapshot
curl "http://127.0.0.1:8001/signals?limit=25&offset=0"
curl http://127.0.0.1:8001/positions/history?paper=all&limit=100
curl http://127.0.0.1:8001/ml/status
curl -X POST http://127.0.0.1:8001/trading/start
curl -X POST http://127.0.0.1:8001/trading/stop
```

WebSocket live updates: `ws://127.0.0.1:8001/ws`

Full route list: `src/api/mod.rs` (~50 HTTP routes + `/ws`).

---

## Project layout

```
mexc-trading-bot-rust/           # this repo (standalone)
├── Cargo.toml
├── requirements.txt             # optional Python deps (ONNX export only)
├── config/settings.yaml         # defaults (strategy, risk, ML, learning)
├── web/                         # dashboard (HTML/CSS/JS)
├── desktop/                     # Tauri shell (see tauri.conf.json)
├── .github/workflows/           # CI installer builds
├── data/                        # SQLite, secrets, models (partially gitignored)
├── scripts/
│   ├── export_onnx.py           # optional ONNX bootstrap from signal DB
│   ├── prepare_release_assets.sh
│   ├── build_installers.sh      # macOS/Linux installer build
│   ├── build_installers.ps1     # Windows installer build
│   └── build_release.sh         # OS-detecting entry point
├── release-assets/models/       # staged for installer (gitignored binaries)
├── src/
│   ├── main.rs                  # Axum server entry
│   ├── scanner/                 # kline poll, confluence + pump loops
│   ├── signals/                 # confluence, volume pump, sniper, zones
│   ├── risk/                    # RiskManager, circuit breakers
│   ├── execution/               # paper + live traders
│   ├── ml/                      # ONNX inference + online learner
│   ├── learning/                # shadow signal capture
│   ├── api/                     # REST + WebSocket handlers
│   └── utils/                   # secrets, alerts, telegram_bot, paths
└── tests/
```

---

## Feature status

See [MIGRATION.md](./MIGRATION.md) for the full tracker.

| Area | Status |
|------|--------|
| Config + SQLite | ✓ |
| Confluence scanner + sniper entry | ✓ |
| Volume pump (two-phase confirmation, BTC/ETH macro) | ✓ |
| Per-strategy position slots | ✓ |
| Paper + live execution (contract-aware sizing) | ✓ |
| Built-in web UI + Tauri desktop | ✓ |
| Settings hot-reload from UI | ✓ |
| ONNX inference + ML gate | ✓ |
| Online ML (Rust, no Python at runtime) | ✓ |
| Shadow learning + outcome resolution | ✓ |
| Telegram trade alerts + strategy labels + `/info` bot | ✓ |
| Server-side signals pagination | ✓ |
| Position history + entry mode on open positions | ✓ |
| Builtin backtest API | ✓ |
| Scalp strategy | Stub (`scalp.enabled: false`) |
| In-process sklearn training | Use `scripts/export_onnx.py` + `requirements.txt` or online learner |

---

## License

MIT
