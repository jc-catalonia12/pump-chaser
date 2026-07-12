# MEXC Trading Bot (Rust)

High-performance **MEXC USDT-M perpetual futures** trading bot with a **historical-candle ML pipeline** (V2): offline training from MEXC kline history → walk-forward validation → ONNX registry, and a Rust live path that only does inference. Includes a built-in web dashboard, native desktop app (Tauri), live/paper execution, decision engine, risk management, and optional Telegram alerts.

Everything runs **on localhost** — no cloud APIs are required for trading or (optional) local LLM regime classification.

## Contents

- [Architecture](#architecture)
- [The AI pipeline](#the-ai-pipeline)
- [Historical training (V2)](#historical-training-v2)
- [First-time setup](#first-time-setup)
- [Auto-updates](#auto-updates)
- [Local LLM regime layer (optional)](#local-llm-regime-layer-optional)
- [Validation & going live](#validation--going-live)
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
  Scanner → Bar Features → ONNX Inference → Decision Engine → Risk → Execution
                ↑                         ↑
        Ollama regime (optional)   Sentiment / macro HTF
                    ↓
    MEXC REST + WebSocket (public + private API)

Offline (Python):
  MEXC history → parquet → features → labels → walk-forward → models/production.onnx
```

**User data** (API keys, SQLite, trade history) lives outside the installer in a local data folder and is never bundled in builds.

**Trained ML models** (`production.onnx`) are loaded for live inference only. The bot does **not** retrain from incoming live candles by default (`ml.online_learning_enabled: false`).

---

## The AI pipeline

The bot runs a **single AI-driven strategy** (`trading.mode: ai`). Each candidate trade flows through:

### 1. Feature engine (24 bar-level features)

Live HTF candles (default Min15) are encoded to match the historical training schema: EMA ratios, RSI, MACD, ATR%, ADX, VWAP distance, Bollinger width, volume/volatility, candle anatomy, returns/momentum, trend strength, and cyclical time. See `src/ml/features.rs` and `training/schema.py`.

### 2. ONNX classifier (3-class)

`production.onnx` predicts **NO_TRADE / LONG / SHORT**. The bot uses P(LONG) or P(SHORT) for the candidate side. Online SGD learning from live outcomes is **off by default**.

### 3. Local LLM regime (optional, Ollama)

Background regime classification still feeds the **decision engine** (not the ONNX input vector). If Ollama is offline, regime stays neutral.

### 4. Decision engine (single go/no-go authority)

Combines ML side probability, expected value in R (`EV = p·RR − (1−p)`), reward:risk, regime alignment, and sentiment.

### 5. Risk manager (safety net)

Unchanged, deliberately boring: daily loss limit, max drawdown halt, circuit breaker, kill switch, per-symbol and concurrent-position caps, min profit filter.

---

## Historical training (V2)

Train offline from token candle history (not live signal outcomes):

```bash
python3 -m venv venv
source venv/bin/activate
pip install -r requirements.txt

# Easiest: auto-pick top liquid USDT-M symbols, train, promote
python -m training pipeline --days 180 --interval Min15 --top 20
```

| Path | Role |
|------|------|
| `data/raw/{SYMBOL}_{tf}.csv.gz` | Historical OHLCV |
| `data/features/`, `data/labels/` | Features + triple-barrier labels |
| `datasets/training.csv.gz` | Joined training set |
| `models/production.onnx` | Live model (also copied to `data/models/`) |

Labels: LONG if +1.5% before −0.8%; SHORT if −1.5% before +0.8%; else NO_TRADE.

Enable weekly auto-retrain with `ml.auto_retrain_enabled: true`. Legacy `scripts/export_onnx.py` is **deprecated**.

---

## First-time setup

Follow these steps to go from zero to a running bot. All commands below assume you are in **this repository root**.

### 1. Install prerequisites

| Requirement | Notes |
|---------------|--------|
| [Rust](https://rustup.rs/) **1.85+** | `rustup update stable` |
| **macOS:** Xcode Command Line Tools | `xcode-select --install` |
| **Windows:** [VS C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) | Required only for building desktop installers |

Optional (historical ML training / ONNX export):

```bash
python3 -m venv venv
source venv/bin/activate          # Windows: venv\Scripts\activate
pip install -r requirements.txt
python -m training pipeline --symbols BTC_USDT ETH_USDT --days 90 --interval Min15
```

Packages: `pandas`, `pyarrow`, `lightgbm`, `scikit-learn`, `skl2onnx`, `onnx`, `onnxruntime` (see `requirements.txt`).

**Ollama (LLM regime layer) — handled automatically by the desktop app.**
The installer / first launch detects whether Ollama is present on your machine.
If it is missing it downloads and silently installs it (macOS: Homebrew / install.sh;
Windows: `OllamaSetup.exe /S` — no UAC prompt required).
Ollama starts automatically when the bot opens and stops cleanly when you close it
so it never keeps running in the background.
The default model (`llama3.2`, ~2 GB) is pulled on first launch in the background;
trading continues immediately while the download completes.

> **Headless / `cargo run` mode:** Ollama is managed separately. Install it from
> https://ollama.com, run `ollama serve` in a terminal, and the bot's LLM regime
> layer will pick it up automatically. If it is not running, the regime stays neutral
> and the bot trades on ML alone — nothing blocks.

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
| **Live** | Real orders on MEXC. Requires valid API keys, `execution.live_trading_enabled: true`, and a **passing acceptance gate** (see [Validation](#validation--going-live)). |

Start with **Paper** until the acceptance gate passes.

For live trading, also review in `config/settings.yaml`:

```yaml
execution:
  live_trading_enabled: true   # master switch
  dry_run: false               # true = log orders without sending
  sync_exchange_positions: true
```

Use **Re-anchor from MEXC wallet** on the Account tab to sync paper/live equity from the exchange.

### 5. Start the scanner

1. Click **Start** in the sidebar (or `POST /trading/start`).
2. The scanner polls USDT-M symbols, builds features, runs the ML ensemble and decision engine, and opens positions when risk checks pass.
3. Watch the **Dashboard** (live scan + regime + sentiment) and **Signals** tab (per-signal ML %, EV, R:R, and decision reason).

### 6. Let the model learn

The online model trains automatically on every resolved outcome (win / loss / expired) — check progress on the **Training** tab (ensemble blend, Kelly sizing, gate accuracy, win rate by side).

**Optional ONNX cold start / retrain:**

```bash
# With venv active (pip install -r requirements.txt):
python scripts/export_onnx.py
# Or point at a specific DB:
python -m training pipeline --symbols BTC_USDT ETH_USDT --days 180 --interval Min15
# Writes models/production.onnx (+ copy under data/models/)
```

With `ml.auto_retrain_enabled: true` the bot runs this itself on a schedule and hot-reloads the result — no restart needed.

Key ML settings in `config/settings.yaml`:

```yaml
ml:
  enabled: true
  supervised_enabled: true
  supervised_threshold: 0.58   # minimum win probability to pass the gate
  hard_ml_gate: true           # block trades below threshold
  min_training_samples: 100
  kelly_fraction: 0.25         # fractional Kelly for risk/leverage scaling
  auto_retrain_enabled: false  # background ONNX retrain (needs Python)
decision:
  enabled: true
  min_expected_value: 0.0      # require non-negative EV (in R)
  min_reward_risk: 0.8
learning:
  enabled: true
  shadow_ml_rejects: true      # learn from rejected setups (no trade)
```

### 7. (Optional) Telegram notifications

Get trade alerts and query bot status from Telegram.

1. Create a bot with [@BotFather](https://t.me/BotFather) and copy the **bot token**.
2. Find your **chat ID** (e.g. via [@userinfobot](https://t.me/userinfobot)).
3. On **Account → Telegram Notifications**, paste token + chat ID → **Connect Telegram**.
4. Open Telegram, chat with your bot, and press **Start** (required before messages can be delivered).
5. Click **Send test message** to confirm.

**Bot commands** (only from your configured chat): `/info`, `/sync`, `/run`, `/stop`, `/start`, `/help`.

Telegram credentials are stored in `secrets.json` alongside MEXC keys — not in `settings.yaml`.

---

## Auto-updates

### For end-users (no action needed)

Once the updater is configured by the developer (see below), the app **checks for a new release automatically** 6 seconds after the dashboard opens. If a newer version is available on GitHub Releases, a small banner appears in the bottom-right corner:

> **Update available: v0.2.0** — [Install & Restart] [Later]

Clicking **Install & Restart** downloads the update in the background and relaunches the app. All user data (API keys, SQLite database, trained models) is preserved — only the application binary and bundled web UI are replaced.

**No prerequisites are needed by end-users** — the `.dmg` (macOS) and `.msi` (Windows) installers are fully self-contained:

| Requirement | Handled by |
|---|---|
| Rust runtime | Compiled into the binary — not needed by users |
| WebView2 (Windows) | Embedded in the installer (`embedBootstrapper`) |
| WebKit (macOS) | Built into macOS since 10.13 |
| API keys | Entered in the app — not bundled |
| ML models | Bundled in the installer and seeded on first launch |
| Ollama | **Automatic** — the desktop app installs, starts, and stops Ollama for you |
| Python | Optional — only if you want to manually retrain the ONNX model |

### For developers: one-time updater setup

Before your first versioned release, run the setup script once:

```bash
./scripts/setup_signing.sh
```

It walks you through:
1. Generating a signing key pair (`cargo tauri signer generate`)
2. Copying the **public key** into `desktop/src-tauri/tauri.conf.json`
3. Adding **4 GitHub Secrets** to your repo (Settings → Secrets → Actions):

| Secret | Value |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | The private key (base64 of `~/.tauri/mexc-bot/private.key`) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Key password (empty if you chose none) |
| `TAURI_UPDATER_PUBKEY` | The public key (same as in `tauri.conf.json`) |
| `TAURI_UPDATE_ENDPOINT` | `https://github.com/YOUR_USERNAME/mexc-trading-bot-rust/releases/latest/download/latest.json` |

Also update the two placeholder URLs in `desktop/src-tauri/tauri.conf.json` and `web/app.js` with your actual GitHub username.

### Releasing a new version

```bash
# 1. Bump the version (0.1.6 → 0.1.7):
bash scripts/bump_version.sh

# 2. Commit, tag, and push:
git add -A && git commit -m "chore: release v$(cat VERSION)"
git tag -a "v$(cat VERSION)" -m "Release v$(cat VERSION)"
git push && git push --tags
```

The `.github/workflows/release.yml` workflow starts automatically:
- Builds signed macOS (Intel + Apple Silicon) and Windows installers
- Generates `latest.json` with signatures
- Creates a GitHub Release with all artifacts
- Existing installs pick up the update on next launch

---

## Local LLM regime layer (optional)

Runs entirely on localhost via [Ollama](https://ollama.com):

```bash
ollama pull llama3.2      # one-time, ~2GB
ollama serve              # usually already running as a service
```

Settings (**Settings → Local LLM Regime** or `config/settings.yaml`):

```yaml
llm:
  enabled: true
  base_url: http://localhost:11434
  model: llama3.2
  poll_interval_sec: 300   # classify every 5 minutes, cached in between
  timeout_sec: 30
```

The current regime appears on the **Dashboard** (Fear & Greed card) and **Training → Market Sentiment**. Check `GET /llm/status` for raw output and error diagnostics. When Ollama is unreachable the bot logs a warning, holds a **neutral regime**, and keeps trading on ML alone.

---

## Validation & going live

The backtester replays resolved signal history with a unified **R-based PnL model** (win = +RR·risk, loss = −risk, expired = quarter-R time-stop, fees included, compounding equity).

On the **Training** tab (Advanced diagnostics):

| Action | What it does |
|--------|--------------|
| **Backtest** | Replay history through the fixed ML threshold gate |
| **Walk-forward** | Train on the first 80%, report out-of-sample accuracy **and** traded PnL |
| **Acceptance gate** | Replay through the **decision engine**, then check the go-live gates |

The acceptance gate (`POST /backtest/acceptance`) checks against `config/settings.yaml`:

```yaml
backtest:
  acceptance_min_trades: 50
  acceptance_min_win_rate: 0.55
  acceptance_min_profit_factor: 1.3
  acceptance_min_expectancy: 0.0
  acceptance_max_drawdown: 0.20
```

**All checks must pass before enabling live trading.** The UI shows each check with actual vs required values.

---

## Dashboard tabs

| Tab | Purpose |
|-----|---------|
| **Dashboard** | Start/stop bot, portfolio KPIs, Fear & Greed + news sentiment + LLM regime, live scan feed, top signals |
| **Signals** | Full signal history with ML %, EV (R), R:R, and decision reason (hover a row); chart overlay |
| **Positions** | Open positions, **History** (closed trades, All/Live/Paper filter), manual close |
| **Readiness** | Production-readiness score and validation gates |
| **Training** | Ensemble status, Kelly sizing, gate accuracy, win rate by side, sentiment + regime, backtest / walk-forward / **acceptance gate** |
| **P&L** | Realized P&L history and daily breakdown |
| **Account** | Paper/live mode, wallet, MEXC keys, Telegram |
| **Settings** | Edit `settings.yaml` from the UI — includes ML, LLM regime, and decision-engine sections |

---

## Configuration

| Item | Location |
|------|----------|
| Strategy & risk defaults | `config/settings.yaml` |
| API keys & Telegram | `data/secrets.json` (UI: Account tab) |
| Override config path | `MEXC_BOT_CONFIG=/path/to/settings.yaml` |
| Env overrides | `MEXC_BOT_*` with `__` for nesting, e.g. `MEXC_BOT_SERVER__PORT=9000` |
| Secrets path override | `MEXC_BOT_SECRETS_PATH=/path/to/secrets.json` |

Settings saved from the UI are written to `settings.yaml` and **reloaded live** by the scanner, risk manager, ML pipeline, LLM service, and execution layer.

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

1. Ensure `data/models/production.onnx` exists (`python -m training pipeline ...`).
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

1. Copy `data/models/production.onnx` (+ optional `feature_schema.json`) into `release-assets/models/`
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

### Bundled vs user data

| Bundled in installer (read-only) | User data (never in installer) |
|----------------------------------|--------------------------------|
| `config/settings.yaml` (defaults) | API keys (`secrets.json`) |
| `web/` dashboard | SQLite database |
| `release-assets/models/` (ONNX + optional online weights) | User-retrained models (never overwritten on upgrade) |
| App binary + WebView | |

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

**ML model for CI** — either commit `data/models/supervised.onnx` to the repo, or set repository **Secrets**: `SUPERVISED_ONNX_B64` (base64 of `supervised.onnx`), optional `ONLINE_MODEL_JSON_B64`.

```bash
base64 -i data/models/supervised.onnx | pbcopy          # macOS
```

Download built installers from the workflow run → **Artifacts**.

---

## API quick reference

```bash
curl http://127.0.0.1:8001/health
curl http://127.0.0.1:8001/risk
curl http://127.0.0.1:8001/live/snapshot
curl "http://127.0.0.1:8001/signals?limit=25&offset=0"
curl http://127.0.0.1:8001/positions/history?paper=all&limit=100
curl http://127.0.0.1:8001/ml/status          # ensemble + Kelly + gate state
curl http://127.0.0.1:8001/llm/status         # LLM regime + Ollama health
curl -X POST http://127.0.0.1:8001/backtest -d '{}'
curl -X POST http://127.0.0.1:8001/backtest/acceptance -d '{}'   # go-live gate
curl -X POST http://127.0.0.1:8001/walk-forward -d '{}'
curl -X POST http://127.0.0.1:8001/trading/start
curl -X POST http://127.0.0.1:8001/trading/stop
```

WebSocket live updates: `ws://127.0.0.1:8001/ws`

Full route list: `src/api/mod.rs`.

---

## Project layout

```
mexc-trading-bot-rust/           # this repo (standalone)
├── Cargo.toml
├── requirements.txt             # Python deps for historical training
├── UPGRADE-V2.0.0.md            # Historical ML rebuild roadmap
├── UPGRADE-v1.0.0.md            # AI-first rebuild plan + phase log
├── config/settings.yaml         # defaults (risk, ML, LLM, decision, backtest)
├── training/                    # V2 offline pipeline (download→features→train→ONNX)
├── web/                         # dashboard (HTML/CSS/JS)
├── desktop/                     # Tauri shell (see tauri.conf.json)
├── .github/workflows/           # CI installer builds
├── data/                        # SQLite, secrets, raw/features/labels, models
├── datasets/                    # joined training.csv.gz
├── models/                      # production.onnx / candidate.onnx / archive/
├── scripts/
│   ├── export_onnx.py           # DEPRECATED signal-DB export
│   ├── prepare_release_assets.sh
│   ├── build_installers.sh      # macOS/Linux installer build
│   ├── build_installers.ps1     # Windows installer build
│   └── build_release.sh         # OS-detecting entry point
├── release-assets/models/       # staged for installer (gitignored binaries)
├── src/
│   ├── main.rs                  # Axum server entry
│   ├── scanner/                 # kline poll, AI signal loop, historical retrain
│   ├── signals/                 # AI candidate generator, signal types
│   ├── ai/                      # LLM regime service + decision engine
│   ├── ml/                      # bar features (24-dim), ONNX 3-class, optional online
│   ├── backtest/                # R-based replay, walk-forward, acceptance gate
│   ├── risk/                    # RiskManager, circuit breakers
│   ├── execution/               # paper + live traders
│   ├── learning/                # shadow signal capture
│   ├── api/                     # REST + WebSocket handlers
│   └── utils/                   # secrets, alerts, telegram_bot, paths
└── tests/
```

---

## Feature status

See [UPGRADE-v1.0.0.md](./UPGRADE-v1.0.0.md) for the full phase-by-phase rebuild log.

| Area | Status |
|------|--------|
| Config + SQLite | ✓ |
| AI signal pipeline (33-feature engine) | ✓ |
| Online + ONNX ensemble with Kelly sizing | ✓ |
| Automated ONNX retrain + hot reload | ✓ |
| Local LLM regime layer (Ollama — auto-install, auto-start/stop) | ✓ |
| Unified decision engine (EV / R:R / regime gates) | ✓ |
| R-based backtest + walk-forward + acceptance gate | ✓ |
| Paper + live execution (contract-aware sizing) | ✓ |
| Built-in web UI + Tauri desktop | ✓ |
| Settings hot-reload from UI | ✓ |
| Shadow learning + outcome resolution | ✓ |
| Telegram trade alerts + `/info` bot | ✓ |
| In-app auto-update notifications + one-click install | ✓ |
| Signed GitHub Releases via CI (macOS + Windows) | ✓ |
| Server-side signals pagination | ✓ |

---

## License

MIT
