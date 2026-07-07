//! Scanner orchestration with background kline + ticker loops.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, RwLock, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::ai::{LlmRegimeService, RegimeInputs};
use crate::config::{AppConfig, SharedAppConfig};
use crate::db::Database;
use crate::exchange::{MexcClient, TickerSnapshot};
use crate::execution::{cleanup_after_position_closed, reconcile_on_boot, LivePositionMonitor, LiveTrader, PaperTrader};
use crate::learning::ParamTuner;
use crate::ml::labels::{compute_r_multiple, soft_label_from_r};
use crate::ml::features::{normalize_feature_vector, TechnicalFeatureBuilder, FEATURE_DIM};
use crate::ml::{EnhanceOutcome, MlFeatureContext, MlPipeline};
use crate::risk::RiskManager;
use crate::sentiment::{sentiment_allows, SentimentService};
use crate::signals::macro_filter::htf_move_pct;
use crate::signals::state::Side;
use crate::signals::{MacroHtfState, PumpSignal, SymbolState, SymbolStates};
use crate::utils::{Alerter, UserSecrets};

const VALID_TRADING_MODES: &[&str] = &["ai"];

/// Dedupe window for shadow-signal saves per symbol+side (seconds).
const SHADOW_DEDUPE_SEC: i64 = 300;

/// How often the learning loop resolves closed trades and trains the model.
const LEARNING_INTERVAL_SEC: u64 = 30;

/// Funding rates only settle every few hours, so we refresh them far less
/// often than klines to keep REST load bounded across many tracked symbols.
const FUNDING_REFRESH_EVERY_N_CYCLES: u64 = 10;

/// Max pending signals shadow-resolved against price action per learning cycle.
/// Keeps the per-cycle MEXC kline fetches bounded.
const SIGNAL_RESOLVE_BATCH: u32 = 40;

fn json_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|n| n as f64))
        .or_else(|| v.as_u64().map(|n| n as f64))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

fn margin_usdt(size: f64, contract_size: f64, entry: f64, leverage: i64) -> f64 {
    let lev = leverage.max(1) as f64;
    (size * contract_size.max(1e-12) * entry) / lev
}

/// SGD sample weight for a shadow-resolved signal based on how it was saved.
fn shadow_resolve_weight(cfg: &crate::config::LearningConfig, sig: &Value) -> f64 {
    let shadow = sig.get("shadow_only").and_then(|v| v.as_bool()).unwrap_or(false);
    if !shadow {
        return 1.0;
    }
    match sig.get("reject_reason").and_then(|v| v.as_str()) {
        Some("ml_gate") => cfg.shadow_ml_reject_weight,
        Some("confluence_near_miss") => cfg.shadow_near_miss_weight,
        Some("sentiment_gate") => cfg.shadow_sentiment_weight,
        _ => 1.0,
    }
}

/// Classify a signal against the klines that followed it. Returns the outcome
/// label and whether it counts as a win for the model. `None` when the data is
/// insufficient to decide (caller leaves the signal pending).
fn resolve_signal_outcome(
    bars: &[crate::exchange::KlineBar],
    start_ts: i64,
    side_long: bool,
    sl: f64,
    tp: f64,
) -> Option<(&'static str, bool)> {
    let mut saw_bar = false;
    for bar in bars.iter().filter(|b| b.timestamp >= start_ts) {
        saw_bar = true;
        // Pessimistic ordering: if a single bar spans both levels we cannot know
        // which printed first, so we assume the stop was hit (counts as a loss).
        if side_long {
            if bar.low <= sl {
                return Some(("loss", false));
            }
            if bar.high >= tp {
                return Some(("win", true));
            }
        } else {
            if bar.high >= sl {
                return Some(("loss", false));
            }
            if bar.low <= tp {
                return Some(("win", true));
            }
        }
    }
    if saw_bar {
        // Window elapsed without touching either level — setup never delivered.
        Some(("expired", false))
    } else {
        None
    }
}

#[derive(Clone)]
pub struct ScannerService {
    inner: Arc<ScannerInner>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

struct ScannerInner {
    config: SharedAppConfig,
    db: Arc<Database>,
    risk: Arc<RwLock<RiskManager>>,
    exchange: MexcClient,
    /// BTC + ETH HTF bars for macro market context (ML features).
    macro_htf: RwLock<MacroHtfState>,
    paper: Mutex<PaperTrader>,
    live: Mutex<LiveTrader>,
    live_monitor: Mutex<LivePositionMonitor>,
    ml: Mutex<MlPipeline>,
    running: AtomicBool,
    ws_connected: AtomicBool,
    /// Unix timestamp of the last ticker batch received from the WS feed.
    /// 0 = never received (warm-up). Used to detect a silent WS stall.
    last_tick_at: AtomicI64,
    tracked_symbols: RwLock<Vec<String>>,
    states: RwLock<SymbolStates>,
    latest_signals: RwLock<Vec<Value>>,
    latest_scans: RwLock<Vec<Value>>,
    scan_log_gate: RwLock<HashMap<String, (i64, String)>>,
    ticker_map: RwLock<HashMap<String, TickerSnapshot>>,
    started_at: RwLock<Option<DateTime<Utc>>>,
    /// Throttle DB PnL reconciliation for UI snapshots (unix sec).
    last_risk_sync_at: AtomicI64,
    /// Throttle open-position monitoring work (unix sec).
    last_pos_monitor_at: AtomicI64,
    alerter: Arc<Alerter>,
    sentiment: Arc<SentimentService>,
    llm_regime: Arc<LlmRegimeService>,
    last_tune_at: AtomicI64,
    /// Serializes signal routing + execution so concurrent ticker candidates
    /// cannot race past max-position checks or hammer SQLite with parallel writes.
    signal_exec: Arc<Semaphore>,
    /// Cached open-position count — refreshed at most once per second on the ticker
    /// path so we can skip the ML pipeline when the book is full (-1 = unknown).
    cached_open_positions: AtomicI64,
    last_open_count_at: AtomicI64,
}

impl ScannerService {
    fn inner_cfg(&self) -> AppConfig {
        self.inner.config.read().unwrap().clone()
    }

    pub fn new(
        config: SharedAppConfig,
        db: Arc<Database>,
        risk: Arc<RwLock<RiskManager>>,
        secrets: Arc<RwLock<UserSecrets>>,
    ) -> crate::error::Result<Self> {
        let secrets_snap = {
            // Block-on-read only at startup to avoid async in new().
            let rt = tokio::runtime::Handle::try_current();
            if let Ok(handle) = rt {
                tokio::task::block_in_place(|| handle.block_on(async { secrets.read().await.clone() }))
            } else {
                UserSecrets::default()
            }
        };
        let exchange = MexcClient::new(config.clone())?;
        let live = LiveTrader::new(config.clone(), db.clone(), secrets_snap.clone());
        let live_client = crate::exchange::MexcPrivateClient::from_secrets(&config.read().unwrap().mexc, &secrets_snap);
        let alerter = Arc::new(Alerter::new(config.read().unwrap().alerts.clone()).with_secrets(secrets.clone()));
        let sentiment = Arc::new(SentimentService::new(config.clone(), db.clone()));
        let llm_regime = Arc::new(LlmRegimeService::new(config.clone()));
        let live_monitor =
            LivePositionMonitor::new(config.clone(), db.clone(), live_client).with_alerter(alerter.clone());
        let inner = Arc::new(ScannerInner {
            config: config.clone(),
            db: db.clone(),
            risk: risk.clone(),
            exchange,
            macro_htf: RwLock::new(MacroHtfState::default()),
            paper: Mutex::new(PaperTrader::new(config.clone(), db)),
            live: Mutex::new(live),
            live_monitor: Mutex::new(live_monitor),
            ml: Mutex::new(MlPipeline::new(config)),
            running: AtomicBool::new(false),
            ws_connected: AtomicBool::new(false),
            last_tick_at: AtomicI64::new(0),
            tracked_symbols: RwLock::new(Vec::new()),
            states: RwLock::new(HashMap::new()),
            latest_signals: RwLock::new(Vec::new()),
            latest_scans: RwLock::new(Vec::new()),
            scan_log_gate: RwLock::new(HashMap::new()),
            ticker_map: RwLock::new(HashMap::new()),
            started_at: RwLock::new(None),
            last_risk_sync_at: AtomicI64::new(0),
            last_pos_monitor_at: AtomicI64::new(0),
            alerter,
            sentiment,
            llm_regime,
            last_tune_at: AtomicI64::new(0),
            signal_exec: Arc::new(Semaphore::new(1)),
            cached_open_positions: AtomicI64::new(-1),
            last_open_count_at: AtomicI64::new(0),
        });
        Ok(Self {
            inner,
            tasks: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn active_strategies(&self) -> Vec<&str> {
        vec!["ai"]
    }

    pub fn get_trading_settings(&self) -> Value {
        let cfg = self.inner_cfg();
        json!({
            "trading_mode": cfg.trading.mode,
            "active_strategies": self.active_strategies(),
            "valid_modes": VALID_TRADING_MODES,
        })
    }

    /// Refresh exchange clients after settings save (MEXC REST URLs).
    pub async fn on_config_updated(&self, prev_mexc: crate::config::MexcConfig) {
        let mexc = self.inner.config.read().unwrap().mexc.clone();
        let mexc_changed = mexc.rest_base_url != prev_mexc.rest_base_url
            || mexc.ws_url != prev_mexc.ws_url;
        if mexc_changed {
            let secrets = self.inner.live.lock().await.secrets().clone();
            self.inner.live.lock().await.refresh_exchange_client();
            self.inner
                .live_monitor
                .lock()
                .await
                .refresh_exchange_client_from_secrets(&secrets);
            warn!(
                "MEXC endpoints changed — stop and start the scanner to reconnect the WebSocket feed"
            );
            let _ = self
                .inner
                .db
                .log_event(
                    "config_updated",
                    "MEXC endpoints changed — restart scanner for new WebSocket URL",
                    None,
                )
                .await;
        }
    }

    pub async fn get_watchlist_settings(&self) -> Value {
        let tracked = self.inner.tracked_symbols.read().await;
        json!({
            "mode": self.inner_cfg().watchlist.mode,
            "tracked_count": tracked.len(),
            "tracked_symbols": tracked.clone(),
        })
    }

    pub async fn get_status(&self) -> Value {
        let tracked = self.inner.tracked_symbols.read().await;
        let started = self.inner.started_at.read().await;
        let latest = self.inner.latest_signals.read().await;
        json!({
            "running": self.inner.running.load(Ordering::SeqCst),
            "ws_connected": self.inner.ws_connected.load(Ordering::SeqCst),
            "tracked_symbols": tracked.len(),
            "started_at": started.as_ref().map(|t| t.to_rfc3339()),
            "signals_buffered": latest.len(),
            "scans_buffered": self.inner.latest_scans.read().await.len(),
            "symbols_refreshed_at": Value::Null,
        })
    }

    pub async fn get_latest_scans(&self, limit: usize) -> Vec<Value> {
        let scans = self.inner.latest_scans.read().await;
        scans.iter().take(limit).cloned().collect()
    }

    /// Recent signals from the in-memory ring buffer (no DB hit).
    pub async fn get_latest_signals(&self, limit: usize) -> Vec<Value> {
        let latest = self.inner.latest_signals.read().await;
        latest.iter().take(limit).cloned().collect()
    }

    pub async fn get_tracked_symbols(&self) -> Value {
        let order = self.inner.tracked_symbols.read().await.clone();
        let states = self.inner.states.read().await;
        let tickers = self.inner.ticker_map.read().await;
        let lookback = self.inner_cfg().scanner.kline_lookback_bars;
        let interval = self.inner_cfg().scanner.kline_interval.clone();
        let refresh_sec = self.inner_cfg().scanner.kline_refresh_sec;
        let ws_on = self.inner.ws_connected.load(Ordering::SeqCst);

        let rows: Vec<Value> = order
            .iter()
            .enumerate()
            .map(|(rank, symbol)| {
                let state = states.get(symbol);
                let ticker = tickers.get(symbol);
                let klines_n = state.map(|s| s.klines.len()).unwrap_or(0);
                let ticks_n = state.map(|s| s.prices.len()).unwrap_or(0);
                let ws_live = ticker.is_some();
                let signal_ready = klines_n >= 15 && ticks_n >= 8 && ws_live;
                let last_scanned = state
                    .and_then(|s| s.last_scanned_at)
                    .map(|t| t.to_rfc3339());
                let last_signal = state
                    .and_then(|s| s.last_signal_at)
                    .map(|t| t.to_rfc3339());

                let mut monitoring = Vec::new();
                if ws_live {
                    monitoring.push("Live ticker: price, 24h volume, fair price".into());
                }
                monitoring.push(format!(
                    "Klines: {klines_n}/{lookback} × {interval} (every {refresh_sec}s)"
                ));
                if ticks_n > 0 {
                    monitoring.push(format!("Price ticks buffered: {ticks_n}"));
                }
                monitoring.push("AI pipeline: features, ML gate, sentiment".into());

                json!({
                    "rank": rank + 1,
                    "symbol": symbol,
                    "last_price": ticker.map(|t| t.last_price),
                    "change_24h_pct": ticker.map(|t| (t.rise_fall_rate * 100.0 * 100.0).round() / 100.0),
                    "klines_bars": klines_n,
                    "klines_target": lookback,
                    "price_ticks": ticks_n,
                    "ws_live": ws_live,
                    "signal_ready": signal_ready,
                    "last_scanned_at": last_scanned,
                    "last_signal_at": last_signal,
                    "monitoring": monitoring,
                })
            })
            .collect();
        json!({
            "mode": self.inner_cfg().watchlist.mode,
            "trading_mode": self.inner_cfg().trading.mode,
            "count": order.len(),
            "scanner_running": self.inner.running.load(Ordering::SeqCst),
            "ws_connected": ws_on,
            "kline_interval": interval,
            "refresh_every_sec": refresh_sec,
            "symbols": rows,
        })
    }

    /// Open positions enriched with the live mark price and unrealized PnL.
    pub async fn get_open_positions_live(&self) -> Vec<Value> {
        let mut positions = self.inner.db.get_open_positions().await.unwrap_or_default();
        let tickers = self.inner.ticker_map.read().await;
        // MEXC stores exchange-sourced position size as `holdVol` (number of
        // contracts), so the USDT PnL must be scaled by the contract size of the
        // symbol. Paper positions are sized directly in coins, so they use 1.0.
        // IMPORTANT: snapshot path must never block on live-trader mutex while an
        // order is being opened/closed. Use try_lock and fall back to 1.0 if busy.
        let has_live_positions = positions
            .iter()
            .any(|p| !p.get("paper").and_then(|v| v.as_bool()).unwrap_or(true));
        let contract_sizes: std::collections::HashMap<String, f64> = if has_live_positions {
            if let Ok(live) = self.inner.live.try_lock() {
                positions
                    .iter()
                    .filter_map(|p| p.get("symbol").and_then(|v| v.as_str()))
                    .map(|s| (s.to_string(), live.contract_size(s)))
                    .collect()
            } else {
                std::collections::HashMap::new()
            }
        } else {
            std::collections::HashMap::new()
        };
        for p in positions.iter_mut() {
            let symbol = p.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let entry = p.get("entry_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let side = p.get("side").and_then(|v| v.as_str()).unwrap_or("long").to_string();
            let paper = p.get("paper").and_then(|v| v.as_bool()).unwrap_or(true);
            let size = p
                .get("remaining_size")
                .and_then(|v| v.as_f64())
                .filter(|&s| s > 0.0)
                .or_else(|| p.get("size").and_then(|v| v.as_f64()))
                .unwrap_or(0.0);
            let leverage = p.get("leverage").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let contract_size = if paper {
                1.0
            } else {
                contract_sizes.get(&symbol).copied().unwrap_or(1.0)
            };
            // Coin-equivalent quantity used for absolute USDT PnL/margin.
            let qty = size * contract_size;
            let mark = tickers
                .get(&symbol)
                .map(|t| t.last_price)
                .filter(|&m| m > 0.0)
                .unwrap_or(entry);

            let entry_mode = p
                .get("entry_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("market")
                .to_string();
            let is_limit = matches!(entry_mode.as_str(), "limit" | "sniper");
            let exchange_linked = p
                .get("exchange_position_id")
                .and_then(|v| v.as_i64())
                .is_some();
            let order_status = if is_limit && !paper && !exchange_linked {
                "pending"
            } else if is_limit {
                "filled"
            } else {
                "open"
            };

            if let Some(obj) = p.as_object_mut() {
                obj.insert("mark_price".into(), json!(mark));
                obj.insert("contract_size".into(), json!(contract_size));
                obj.insert("order_status".into(), json!(order_status));
                if entry > 0.0 {
                    let move_pct = if side == "short" {
                        (entry - mark) / entry * 100.0
                    } else {
                        (mark - entry) / entry * 100.0
                    };
                    let upnl = if side == "short" {
                        (entry - mark) * qty
                    } else {
                        (mark - entry) * qty
                    };
                    let roi_pct = if leverage > 0.0 { move_pct * leverage } else { move_pct };
                    obj.insert("unrealized_pnl".into(), json!((upnl * 1_000_000.0).round() / 1_000_000.0));
                    obj.insert("unrealized_pnl_pct".into(), json!((move_pct * 100.0).round() / 100.0));
                    obj.insert("unrealized_roi_pct".into(), json!((roi_pct * 100.0).round() / 100.0));
                }
            }
        }
        positions
    }

    /// Enrich a single position with mark/unrealized PnL for charts (no phantom heal).
    pub async fn enrich_position_for_chart(&self, mut pos: Value) -> Value {
        let symbol = pos
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if symbol.is_empty() {
            return pos;
        }

        let entry = pos.get("entry_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let side = pos
            .get("side")
            .and_then(|v| v.as_str())
            .unwrap_or("long")
            .to_string();
        let paper = pos.get("paper").and_then(|v| v.as_bool()).unwrap_or(true);
        let size = pos
            .get("remaining_size")
            .and_then(|v| v.as_f64())
            .filter(|&s| s > 0.0)
            .or_else(|| pos.get("size").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        let leverage = pos.get("leverage").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let contract_size = if paper {
            1.0
        } else if let Ok(live) = self.inner.live.try_lock() {
            live.contract_size(&symbol)
        } else {
            // Keep chart responsive when live-trader mutex is busy.
            1.0
        };
        let qty = size * contract_size;
        let tickers = self.inner.ticker_map.read().await;
        let mark = tickers
            .get(&symbol)
            .map(|t| t.last_price)
            .filter(|&m| m > 0.0)
            .unwrap_or(entry);
        let is_open = pos.get("status").and_then(|v| v.as_str()) != Some("closed");

        if let Some(obj) = pos.as_object_mut() {
            obj.insert("mark_price".into(), json!(mark));
            obj.insert("contract_size".into(), json!(contract_size));
            if entry > 0.0 && is_open {
                let move_pct = if side == "short" {
                    (entry - mark) / entry * 100.0
                } else {
                    (mark - entry) / entry * 100.0
                };
                let upnl = if side == "short" {
                    (entry - mark) * qty
                } else {
                    (mark - entry) * qty
                };
                let roi_pct = if leverage > 0.0 {
                    move_pct * leverage
                } else {
                    move_pct
                };
                obj.insert(
                    "unrealized_pnl".into(),
                    json!((upnl * 1_000_000.0).round() / 1_000_000.0),
                );
                obj.insert(
                    "unrealized_pnl_pct".into(),
                    json!((move_pct * 100.0).round() / 100.0),
                );
                obj.insert(
                    "unrealized_roi_pct".into(),
                    json!((roi_pct * 100.0).round() / 100.0),
                );
            }
        }
        pos
    }

    /// Live mark / unrealized PnL for one position (open positions only).
    pub async fn get_position_live(&self, position_id: i64) -> Option<Value> {
        self.get_open_positions_live()
            .await
            .into_iter()
            .find(|p| p.get("id").and_then(|v| v.as_i64()) == Some(position_id))
    }

    pub async fn get_risk_metrics(&self) -> Value {
        let open = self.inner.db.get_open_positions().await.unwrap_or_default();
        let open_n = open.len() as i64;
        let stale_threshold = self.inner_cfg().risk.ws_stale_sec as i64;
        let last_tick = self.inner.last_tick_at.load(Ordering::Relaxed);
        let ws_stale = if last_tick == 0 {
            false // still warming up — not yet considered stale
        } else {
            Utc::now().timestamp() - last_tick > stale_threshold
        };
        // UI snapshots fire every 2 s — avoid a write lock + DB reconcile on each
        // tick; that contended with position monitoring and could freeze the app.
        let now = Utc::now().timestamp();
        let last_sync = self.inner.last_risk_sync_at.load(Ordering::Relaxed);
        if now.saturating_sub(last_sync) >= 30 {
            if let Ok(mut risk) = self.inner.risk.try_write() {
                self.inner.last_risk_sync_at.store(now, Ordering::Relaxed);
                let _ = risk.sync_pnl_totals_from_db().await;
                return risk.metrics_json(open_n, ws_stale);
            }
        }
        if let Ok(risk) = self.inner.risk.try_read() {
            return risk.metrics_json(open_n, ws_stale);
        }
        // Risk lock busy (signal execution) — return lightweight metrics so the
        // snapshot cache never blocks the UI thread.
        json!({
            "equity": 0,
            "peak_equity": 0,
            "daily_pnl": 0,
            "daily_pnl_pct": 0,
            "weekly_pnl": 0,
            "equity_source": "busy",
            "open_positions": open_n,
            "trading_paused": false,
            "kill_switch": false,
            "max_risk_per_trade_pct": self.inner_cfg().risk.max_risk_per_trade * 100.0,
            "ws_stale": ws_stale,
        })
    }

    pub async fn get_symbol_klines(&self, symbol: &str) -> Vec<crate::exchange::KlineBar> {
        self.inner
            .states
            .read()
            .await
            .get(symbol)
            .map(|s| s.klines.clone())
            .unwrap_or_default()
    }

    pub async fn get_learning_status(&self) -> Value {
        let trade_stats = self
            .inner
            .db
            .get_trade_stats(200, None)
            .await
            .unwrap_or_else(|_| json!({}));
        let model = self.inner.ml.lock().await.learning_status();
        let settings = self.inner_cfg();
        let ml = &settings.ml;
        let learning = &settings.learning;
        let promotion = self
            .inner
            .db
            .promotion_metrics()
            .await
            .unwrap_or_else(|_| json!({}));
        json!({
            "enabled": learning.enabled,
            "shadow_ml_rejects": learning.shadow_ml_rejects,
            "trade_stats": trade_stats,
            "model": model,
            "promotion": promotion,
            "message": format!(
                "Continuous learning — trade W/L {:.1}×/{:.1}×, ML rejects {:.2}×",
                ml.trade_win_weight,
                ml.trade_loss_weight,
                learning.shadow_ml_reject_weight,
            ),
        })
    }

    pub async fn reload_learning_params(&self) -> Value {
        json!({ "ok": true, "message": "Online model trains continuously; no manual reload needed" })
    }

    /// Native replacement for the old Python `/ml/train`: replay resolved trades
    /// from the DB into the online model.
    pub async fn train_online_from_db(&self) -> Value {
        let trained = self.inner.bootstrap_online_model().await;
        let stats = self.inner.ml.lock().await.online_stats();
        json!({
            "trained_samples": trained,
            "online_model": stats,
            "message": "Online model trained from resolved trade history",
        })
    }

    /// Shadow-resolve pending signals against price action and train on them.
    /// Lets the model learn from setups that never opened a position.
    pub async fn resolve_signals_from_price(&self, max_signals: u32) -> Value {
        let learned = self
            .inner
            .resolve_pending_signals_from_price(max_signals)
            .await;
        let stats = self.inner.ml.lock().await.online_stats();
        json!({
            "resolved_samples": learned,
            "online_model": stats,
            "message": "Pending signals resolved from price action and fed to the online model",
        })
    }

    pub async fn ml_learning_status(&self) -> Value {
        self.inner.ml.lock().await.learning_status()
    }

    pub async fn sentiment_status(&self) -> Value {
        self.inner.sentiment.status_json().await
    }

    pub async fn llm_regime_status(&self) -> Value {
        self.inner.llm_regime.status_json().await
    }

    pub async fn param_evolution_history(&self) -> Value {
        let runs = self
            .inner
            .db
            .get_param_evolution_history(25)
            .await
            .unwrap_or_default();
        json!({ "runs": runs })
    }

    pub async fn start(&self) -> crate::error::Result<Value> {
        if self.inner.running.load(Ordering::SeqCst) {
            return Ok(self.get_status().await);
        }

        // Flip running immediately and do all the slow network work (symbol
        // discovery, liquidity ranking, WebSocket connect) in the background so
        // the Start button responds instantly instead of blocking for seconds.
        self.inner.running.store(true, Ordering::SeqCst);
        *self.inner.started_at.write().await = Some(Utc::now());
        let _ = self.inner.db.log_event("scanner", "Scanner starting", None).await;

        // Paper learning: don't let a persisted ML auto-gate block all entries.
        let cfg = self.inner.config.read().unwrap().clone();
        if !cfg.execution.live_trading_enabled && cfg.execution.paper_relax_gates {
            self.inner.ml.lock().await.set_gate_auto_enabled(false);
            let _ = self.inner.db.set_ml_gate_auto_state(false).await;
        }

        let inner = self.inner.clone();
        let tasks_arc = self.tasks.clone();
        let bootstrap = tokio::spawn(async move {
            inner.bootstrap_streams(tasks_arc).await;
        });
        self.tasks.lock().await.push(bootstrap);

        info!("MEXC Trading Bot scanner start requested — discovering symbols in background");
        Ok(json!({
            "status": "starting",
            "running": true,
            "ws_connected": false,
            "tracked_symbols": self.inner.tracked_symbols.read().await.len(),
        }))
    }

    pub async fn stop(&self) -> Value {
        self.inner.running.store(false, Ordering::SeqCst);
        self.inner.ws_connected.store(false, Ordering::SeqCst);
        *self.inner.started_at.write().await = None;
        self.inner.exchange.stop_ticker_stream().await;

        let mut tasks = self.tasks.lock().await;
        for t in tasks.drain(..) {
            t.abort();
        }
        let _ = self.inner.db.log_event("scanner", "Scanner stopped", None).await;
        json!({ "status": "stopped", "running": false })
    }

    pub async fn activate_kill_switch(&self) -> Value {
        {
            let mut risk = self.inner.risk.write().await;
            let _ = risk.activate_kill_switch().await;
        }
        self.inner
            .alerter
            .fire("kill_switch", "Manual kill switch activated — all positions will be closed")
            .await;

        // Close every open position at market (paper closes locally, live submits
        // exchange close orders) so the kill switch actually flattens exposure.
        let open = self.inner.db.get_open_positions().await.unwrap_or_default();
        let mut closed = 0usize;
        let mut closed_live = 0usize;
        let mut errors: Vec<Value> = Vec::new();
        for pos in &open {
            let Some(id) = pos.get("id").and_then(|v| v.as_i64()) else {
                continue;
            };
            let is_live = !pos.get("paper").and_then(|v| v.as_bool()).unwrap_or(true);
            let result = self.close_position_manual(id).await;
            if result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                closed += 1;
                if is_live {
                    closed_live += 1;
                }
            } else if let Some(err) = result.get("error") {
                errors.push(json!({ "position_id": id, "error": err }));
            }
        }

        // Halt scanning so no new positions are opened while the kill switch is on.
        self.stop().await;

        let _ = self
            .inner
            .db
            .log_event(
                "kill_switch",
                &format!("Kill switch flattened {closed} position(s)"),
                Some(json!({ "closed": closed, "closed_live": closed_live, "errors": errors })),
            )
            .await;

        json!({
            "status": "kill_switch_active",
            "closed": closed,
            "closed_live": closed_live,
            "errors": errors,
            "running": false,
        })
    }

    pub async fn deactivate_kill_switch(&self) -> Value {
        let mut risk = self.inner.risk.write().await;
        let _ = risk.deactivate_kill_switch().await;
        json!({ "status": "kill_switch_inactive" })
    }

    pub async fn update_live_secrets(&self, secrets: UserSecrets) {
        self.inner.live.lock().await.update_secrets(secrets.clone());
        // Keep live_monitor client in sync so it can submit closes.
        let cfg = self.inner_cfg();
        let new_client = crate::exchange::MexcPrivateClient::from_secrets(
            &cfg.mexc,
            &secrets,
        );
        self.inner.live_monitor.lock().await.update_client(new_client);
    }

    pub async fn sync_exchange_positions(&self) -> Value {
        let result = self.inner.live.lock().await.sync_exchange_positions().await;
        {
            let mut risk = self.inner.risk.write().await;
            let _ = risk.reconcile_pnl_from_db().await;
        }
        result
    }

    /// Manually close an open position at the current mark price (paper) or via
    /// exchange order (live).
    pub async fn close_position_manual(&self, position_id: i64) -> Value {
        let Some(pos) = self
            .inner
            .db
            .get_position_by_id(position_id)
            .await
            .ok()
            .flatten()
        else {
            return json!({ "error": "Position not found" });
        };

        let status = pos.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if status != "open" && status != "partial" {
            return json!({
                "error": format!("Position is already {status}"),
                "position_id": position_id,
            });
        }

        let symbol = pos
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let side_str = pos.get("side").and_then(|v| v.as_str()).unwrap_or("long");
        let size = pos
            .get("remaining_size")
            .and_then(|v| v.as_f64())
            .filter(|&s| s > 0.0)
            .or_else(|| pos.get("size").and_then(|v| v.as_f64()))
            .unwrap_or(0.0);
        let entry = pos.get("entry_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let paper = pos.get("paper").and_then(|v| v.as_bool()).unwrap_or(true);

        if size <= 0.0 {
            return json!({ "error": "Position size is zero", "position_id": position_id });
        }

        let mark = self.resolve_mark_price(&symbol, entry).await;
        let mut reason = "manual_close";

        // Exchange positions are sized in contracts; convert to coins for PnL.
        // Paper closes have no real fees; live trades deduct round-trip taker fees.
        let (contract_size, fee_rate) = if paper {
            (1.0_f64, 0.0_f64)
        } else {
            let live = self.inner.live.lock().await;
            (live.contract_size(&symbol), live.fee_rate(&symbol))
        };

        if !paper {
            let live = self.inner.live.lock().await;
            if live.is_live() {
                use crate::models::PositionSide;
                let side = if side_str == "short" {
                    PositionSide::Short
                } else {
                    PositionSide::Long
                };

                // A position opened while dry-run was enabled is stored as a live
                // position (paper=false) but was never actually placed on MEXC.
                // Submitting a real close order for it fails on the exchange and
                // leaves the local position stuck open. Detect that case and close
                // it locally instead of trying to close a non-existent position.
                let exists_on_exchange = live.exchange_has_position(&symbol, side).await;
                if exists_on_exchange == Some(false) {
                    reason = "manual_close_phantom";
                    let _ = self
                        .inner
                        .db
                        .log_event(
                            "position_closed",
                            &format!("Closing phantom/dry-run position {symbol} locally (no live exchange position)"),
                            Some(json!({ "position_id": position_id, "symbol": symbol, "side": side_str })),
                        )
                        .await;
                } else {
                    let result = live
                        .close_position(&symbol, size, side, None, Some(mark))
                        .await;
                    let success = result.get("success").and_then(|v| v.as_bool()).unwrap_or(false)
                        || result.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);
                    if !success {
                        let err = result
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Exchange close failed");
                        return json!({
                            "error": err,
                            "position_id": position_id,
                            "exchange": result,
                        });
                    }
                }
            }
        }

        match self
            .inner
            .db
            .close_position(position_id, mark, contract_size, fee_rate, reason)
            .await
        {
            Ok(pnl) => {
                self.inner.note_position_closed();
                let mut risk = self.inner.risk.write().await;
                let _ = risk.update_pnl(pnl).await;
                drop(risk);
                let _ = self
                    .inner
                    .db
                    .log_event(
                        "position_closed",
                        &format!("Manually closed {symbol} ({reason})"),
                        Some(json!({
                            "position_id": position_id,
                            "symbol": symbol,
                            "side": side_str,
                            "exit_price": mark,
                            "pnl": pnl,
                            "paper": paper,
                        })),
                    )
                    .await;
                let details = json!({
                    "symbol": symbol,
                    "side": side_str.to_uppercase(),
                    "strategy": pos.get("strategy").and_then(|v| v.as_str()),
                    "exit_price": mark,
                    "pnl": pnl,
                    "reason": reason,
                });
                let msg = format!(
                    "{} {} manually closed @ {:.6} — PnL: {}{:.4} USDT",
                    side_str.to_uppercase(), symbol, mark,
                    if pnl >= 0.0 { "+" } else { "" }, pnl
                );
                self.inner.alerter.trade_event("position_closed", &msg, Some(&details)).await;
                info!("Manually closed position {position_id} {symbol} pnl={pnl:.4}");
                if !paper {
                    let exchange_pos_id = pos.get("exchange_position_id").and_then(|v| v.as_i64());
                    let live = self.inner.live.lock().await;
                    if live.is_live() {
                        cleanup_after_position_closed(live.client(), &symbol, exchange_pos_id)
                            .await;
                    }
                }
                json!({
                    "ok": true,
                    "position_id": position_id,
                    "symbol": symbol,
                    "exit_price": mark,
                    "pnl": pnl,
                    "reason": reason,
                    "paper": paper,
                })
            }
            Err(exc) => json!({ "error": exc.to_string(), "position_id": position_id }),
        }
    }

    async fn resolve_mark_price(&self, symbol: &str, fallback: f64) -> f64 {
        if let Some(ticker) = self.inner.ticker_map.read().await.get(symbol) {
            if ticker.last_price > 0.0 {
                return ticker.last_price;
            }
        }
        if let Ok(tickers) = self.inner.exchange.get_tickers().await {
            if let Some(t) = tickers.iter().find(|t| t.symbol == symbol) {
                if t.last_price > 0.0 {
                    return t.last_price;
                }
            }
        }
        fallback
    }
}

impl ScannerInner {
    fn cfg(&self) -> AppConfig {
        self.config.read().unwrap().clone()
    }

    /// Discover symbols, connect the ticker stream, and spawn the long-lived
    /// loops. Runs in the background so `start()` can return immediately.
    async fn bootstrap_streams(self: Arc<Self>, tasks: Arc<Mutex<Vec<JoinHandle<()>>>>) {
        // Symbol + contract discovery (single contract fetch, reused for both).
        if self.exchange.ping().await {
            match self.exchange.discover_contracts().await {
                Ok(contracts) => {
                    let symbols: Vec<String> =
                        contracts.iter().map(|c| c.symbol.clone()).collect();
                    self.live.lock().await.update_contracts(contracts.clone());
                    self.live_monitor.lock().await.update_contracts(contracts);
                    match self.exchange.rank_symbols(&symbols).await {
                        Ok(ranked) if !ranked.is_empty() => {
                            *self.tracked_symbols.write().await = ranked;
                        }
                        _ => {
                            *self.tracked_symbols.write().await = symbols;
                        }
                    }
                }
                Err(exc) => warn!("Contract discovery failed: {exc}"),
            }
        } else {
            warn!("MEXC REST unreachable — scanner degraded");
        }

        // Phase 4: Startup reconciliation — diff DB vs MEXC open positions.
        // Runs once per session start (not on every reconnect).
        {
            let live = self.live.lock().await;
            if live.has_credentials() {
                let client = live.private_client();
                let cfg = self.cfg();
                reconcile_on_boot(client, &self.db, &cfg).await;
            }
        }

        if !self.running.load(Ordering::SeqCst) {
            return;
        }

        let (ticker_tx, mut ticker_rx) = mpsc::channel(64);
        if let Err(exc) = self.exchange.start_ticker_stream(Some(ticker_tx)).await {
            warn!("Ticker stream failed to start: {exc}");
        } else {
            self.ws_connected.store(true, Ordering::SeqCst);
        }

        let ticker_inner = self.clone();
        let ticker_task = tokio::spawn(async move {
            while let Some(batch) = ticker_rx.recv().await {
                // Pass Arc<Self> so process_tickers can spawn signal-execution tasks
                // without blocking the receive loop.
                Arc::clone(&ticker_inner).process_tickers(batch).await;
            }
        });

        let kline_inner = self.clone();
        let kline_task = tokio::spawn(async move {
            kline_inner.kline_refresh_loop().await;
        });

        {
            let mut guard = tasks.lock().await;
            guard.push(ticker_task);
            guard.push(kline_task);
            if self.cfg().learning.enabled {
                let learn_inner = self.clone();
                let learn_task = tokio::spawn(async move {
                    learn_inner.learning_loop().await;
                });
                guard.push(learn_task);
                info!("Continuous learning loop started (every {LEARNING_INTERVAL_SEC}s)");
            }
            if self.cfg().sentiment.enabled {
                let sentiment = self.sentiment.clone();
                guard.push(tokio::spawn(async move {
                    sentiment.run_loop().await;
                }));
                info!("Sentiment poll loop started");
            }
            if self.cfg().ml.auto_retrain_enabled {
                let retrain_inner = self.clone();
                guard.push(tokio::spawn(async move {
                    retrain_inner.retrain_loop().await;
                }));
                info!(
                    "ONNX auto-retrain loop started (every {}h)",
                    self.cfg().ml.retrain_interval_hours
                );
            }
            if self.cfg().llm.enabled {
                let regime_inner = self.clone();
                guard.push(tokio::spawn(async move {
                    regime_inner.llm_regime_loop().await;
                }));
                info!(
                    "LLM regime loop started ({} @ {}, every {}s)",
                    self.cfg().llm.model,
                    self.cfg().llm.base_url,
                    self.cfg().llm.poll_interval_sec
                );
            }
            {
                let live = self.live.lock().await;
                if live.has_credentials() {
                    drop(live);
                    let heal_inner = self.clone();
                    guard.push(tokio::spawn(async move {
                        heal_inner.phantom_heal_loop().await;
                    }));
                    info!("Phantom position heal loop started (every 45s)");
                }
            }
        }

        let _ = self.db.log_event("scanner", "Scanner started", None).await;
        info!(
            "Scanner streams live — tracking {} symbols",
            self.tracked_symbols.read().await.len()
        );
    }

    async fn kline_refresh_loop(&self) {
        let mut cycle: u64 = 0;
        while self.running.load(Ordering::SeqCst) {
            let cfg = self.cfg();
            let interval = cfg.scanner.kline_interval.clone();
            let lookback = cfg.scanner.kline_lookback_bars;
            let htf_interval = cfg.scanner.htf_interval.clone();
            let htf_lookback = cfg.scanner.htf_lookback_bars;
            let refresh_sec = cfg.scanner.kline_refresh_sec;
            let fetch_funding =
                cfg.scanner.fetch_funding_rate && cycle % FUNDING_REFRESH_EVERY_N_CYCLES == 0;
            let symbols = self.tracked_symbols.read().await.clone();
            for symbol in symbols {
                if !self.running.load(Ordering::SeqCst) {
                    break;
                }
                // Primary (Min1) klines
                match self.exchange.get_klines(&symbol, &interval).await {
                    Ok(mut bars) => {
                        if bars.len() > lookback as usize {
                            bars = bars[bars.len() - lookback as usize..].to_vec();
                        }
                        let mut states = self.states.write().await;
                        let state = states
                            .entry(symbol.clone())
                            .or_insert_with(|| SymbolState::new(symbol.clone()));
                        state.update_klines(bars);
                    }
                    Err(exc) => debug!("Kline refresh {symbol} skipped: {exc}"),
                }
                // Higher-timeframe klines for structural context features
                match self.exchange.get_klines(&symbol, &htf_interval).await {
                    Ok(mut bars) => {
                        if bars.len() > htf_lookback as usize {
                            bars = bars[bars.len() - htf_lookback as usize..].to_vec();
                        }
                        let mut states = self.states.write().await;
                        if let Some(state) = states.get_mut(&symbol) {
                            state.update_htf_klines(bars);
                        }
                    }
                    Err(exc) => debug!("HTF kline refresh {symbol} skipped: {exc}"),
                }
                // Funding rate — low-frequency ML feature input, not a trading gate.
                if fetch_funding {
                    match self.exchange.get_funding_rate(&symbol).await {
                        Ok(rate) => {
                            let mut states = self.states.write().await;
                            if let Some(state) = states.get_mut(&symbol) {
                                state.funding_rate = rate;
                            }
                        }
                        Err(exc) => debug!("Funding rate refresh {symbol} skipped: {exc}"),
                    }
                }
            }
            self.refresh_macro_htf(&htf_interval, htf_lookback).await;
            cycle = cycle.wrapping_add(1);
            tokio::time::sleep(tokio::time::Duration::from_secs(refresh_sec))
                .await;
        }
    }

    /// Background loop: periodically retrain the ONNX GBM offline and hot-reload
    /// it. Sleeps `ml.retrain_interval_hours` between checks, re-reading config
    /// each cycle so toggling `auto_retrain_enabled` off takes effect promptly.
    async fn retrain_loop(&self) {
        let mut last_retrain_resolved: i64 = 0;
        while self.running.load(Ordering::SeqCst) {
            let cfg = self.cfg();
            if cfg.ml.auto_retrain_enabled {
                if let Some(resolved) = self.maybe_retrain_onnx(last_retrain_resolved).await {
                    last_retrain_resolved = resolved;
                }
            }
            let sleep_sec = cfg.ml.retrain_interval_hours.max(1) * 3600;
            for _ in 0..sleep_sec {
                if !self.running.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        }
    }

    /// Run `scripts/export_onnx.py` against the live DB if enough new resolved
    /// signals have accumulated since the last retrain, then hot-reload the
    /// resulting ONNX file into the live pipeline. Best-effort: any failure
    /// (missing Python, missing deps, too few samples) just logs a warning —
    /// the bot keeps trading on the online model / previous ONNX export.
    /// Returns the resolved-signal count at the time of a successful retrain.
    async fn maybe_retrain_onnx(&self, last_retrain_resolved: i64) -> Option<i64> {
        let counts = match self.db.get_signal_outcome_counts().await {
            Ok(c) => c,
            Err(exc) => {
                debug!("Retrain check skipped — couldn't read signal outcome counts: {exc}");
                return None;
            }
        };
        let resolved = counts.get("resolved").and_then(|v| v.as_i64()).unwrap_or(0);
        let cfg = self.cfg();
        if resolved - last_retrain_resolved < cfg.ml.retrain_min_new_samples as i64 {
            debug!(
                "Retrain skipped — only {} new resolved signal(s), need {}",
                (resolved - last_retrain_resolved).max(0),
                cfg.ml.retrain_min_new_samples
            );
            return None;
        }

        let python_bin = cfg.ml.python_bin.clone();
        let script_path = cfg.ml.export_script_path.clone();
        let db_path = cfg.storage.sqlite_path.clone();
        let out_path = crate::ml::onnx_model_path(&cfg.ml);

        info!("Starting offline ONNX retrain ({resolved} resolved signals available)...");
        let output = tokio::process::Command::new(&python_bin)
            .arg(&script_path)
            .arg("--db")
            .arg(&db_path)
            .arg("--out")
            .arg(&out_path)
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                info!(
                    "ONNX retrain succeeded: {}",
                    String::from_utf8_lossy(&out.stdout).trim()
                );
                self.ml.lock().await.reload_onnx();
                let _ = self
                    .db
                    .log_event(
                        "ml_retrain",
                        "ONNX model retrained offline and hot-reloaded",
                        Some(json!({ "resolved_signals": resolved })),
                    )
                    .await;
                Some(resolved)
            }
            Ok(out) => {
                warn!(
                    "ONNX retrain script exited with {}: {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                None
            }
            Err(exc) => {
                warn!("Failed to spawn ONNX retrain script ({python_bin} {script_path}): {exc}");
                None
            }
        }
    }

    /// Background loop: periodically classify the market regime via the local
    /// Ollama LLM. The cached result feeds ML feature slots 27–32. Offline or
    /// misbehaving Ollama degrades to a neutral regime — never blocks trading.
    /// Periodically close DB rows that are not on the exchange (failed rollbacks).
    /// Runs off the snapshot/ticker hot-path so UI snapshots never block on heal.
    async fn phantom_heal_loop(&self) {
        const INTERVAL_SEC: i64 = 45;
        while self.running.load(Ordering::SeqCst) {
            for _ in 0..INTERVAL_SEC {
                if !self.running.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            let live = self.live.lock().await;
            if !live.has_credentials() {
                continue;
            }
            let healed = live.heal_phantom_open_positions().await;
            drop(live);
            if healed > 0 {
                info!("Auto-healed {healed} phantom live position(s) not on MEXC");
                let mut risk = self.risk.write().await;
                let _ = risk.reconcile_pnl_from_db().await;
                drop(risk);
                self.cached_open_positions.store(-1, Ordering::Relaxed);
            }
        }
    }

    async fn llm_regime_loop(&self) {
        while self.running.load(Ordering::SeqCst) {
            let inputs = self.build_regime_inputs().await;
            self.llm_regime.refresh(&inputs).await;
            let interval = self.cfg().llm.poll_interval_sec.max(30);
            for _ in 0..interval {
                if !self.running.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
        }
    }

    /// Assemble the market snapshot the LLM classifies: BTC/ETH HTF moves and
    /// BTC volatility from the macro kline cache, plus news sentiment state.
    async fn build_regime_inputs(&self) -> RegimeInputs {
        let lookback = (self.cfg().scanner.htf_lookback_bars as usize).max(2);
        let (btc_move, eth_move, btc_atr) = {
            let macro_htf = self.macro_htf.read().await;
            let btc_move = htf_move_pct(&macro_htf.btc_klines, lookback);
            let eth_move = htf_move_pct(&macro_htf.eth_klines, lookback);
            let hlc: Vec<(f64, f64, f64)> = macro_htf
                .btc_klines
                .iter()
                .rev()
                .take(15)
                .rev()
                .map(|b| (b.high, b.low, b.close))
                .collect();
            (btc_move, eth_move, crate::signals::indicators::atr_pct(&hlc))
        };
        let snap = self.sentiment.snapshot().await;
        RegimeInputs {
            btc_move_pct: btc_move,
            eth_move_pct: eth_move,
            btc_atr_pct: btc_atr,
            global_sentiment: snap.global_score,
            fear_greed: snap.fear_greed,
        }
    }

    /// Background loop: resolve closed-trade outcomes and train the online model.
    async fn learning_loop(&self) {
        if self.ml.lock().await.online_sample_count() == 0 {
            self.bootstrap_online_model().await;
        }
        let paper_relax = !self.cfg().execution.live_trading_enabled
            && self.cfg().execution.paper_relax_gates;
        if paper_relax {
            self.ml.lock().await.set_gate_auto_enabled(false);
        } else if let Ok(enabled) = self.db.get_ml_gate_auto_state().await {
            self.ml.lock().await.set_gate_auto_enabled(enabled);
        }
        while self.running.load(Ordering::SeqCst) {
            for _ in 0..LEARNING_INTERVAL_SEC {
                if !self.running.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }
            if let Err(exc) = self.resolve_outcomes_and_learn().await {
                warn!("Learning loop error: {exc}");
            }
            let _ = self.resolve_pending_signals_from_price(SIGNAL_RESOLVE_BATCH).await;

            if !paper_relax {
                if let Some(toggled) = self.ml.lock().await.evaluate_gate_auto() {
                    let _ = self.db.set_ml_gate_auto_state(toggled).await;
                    let _ = self
                        .db
                        .log_event(
                            "ml_gate_auto",
                            &format!("ML gate auto-{}", if toggled { "enabled" } else { "disabled" }),
                            None,
                        )
                        .await;
                }
            }

            self.maybe_run_auto_tune().await;
        }
    }

    async fn maybe_run_auto_tune(&self) {
        let cfg = self.cfg();
        if !cfg.learning.auto_tune_enabled {
            return;
        }
        let interval_sec = cfg.learning.auto_tune_interval_hours.max(1) * 3600;
        let now = Utc::now().timestamp();
        let last = self.last_tune_at.load(Ordering::Relaxed);
        if last > 0 && now - last < interval_sec as i64 {
            return;
        }
        self.last_tune_at.store(now, Ordering::Relaxed);

        let signals = self
            .db
            .get_resolved_signals_with_features(2000)
            .await
            .unwrap_or_default();
        if signals.len() < 50 {
            return;
        }
        let champion = ParamTuner::load_champion(&self.db, &cfg)
            .await
            .map(|t| t.champion)
            .unwrap_or_else(|_| ParamTuner::default_champion(&cfg));
        let result = ParamTuner::tune(&signals, &champion);
        let improved = result.get("improved").and_then(|v| v.as_bool()).unwrap_or(false);
        if !improved {
            return;
        }
        let challenger = result.get("challenger").cloned().unwrap_or(json!({}));
        let oos = result.get("oos_metrics").cloned().unwrap_or(json!({}));
        let champion_oos = ParamTuner::tune(&signals, &champion)
            .get("oos_metrics")
            .cloned()
            .unwrap_or(json!({}));
        let promote = ParamTuner::should_promote(&champion_oos, &oos);
        let apply = cfg.learning.auto_tune_apply == "apply";
        if promote && apply {
            let _ = self.db.set_strategy_overlay(&challenger).await;
        }
        let _ = self
            .db
            .insert_param_evolution(&champion, &challenger, &oos, promote && apply)
            .await;
        let _ = self
            .db
            .log_event(
                "param_tune",
                if promote && apply {
                    "Parameter challenger promoted to overlay"
                } else if promote {
                    "Parameter tune suggests promotion (auto_tune_apply=suggest)"
                } else {
                    "Parameter tune completed — no promotion"
                },
                Some(result),
            )
            .await;
    }

    async fn ml_feature_context(&self, symbol: &str) -> MlFeatureContext {
        let lookback = self.cfg().scanner.htf_lookback_bars as usize;
        let btc_move = {
            let macro_htf = self.macro_htf.read().await;
            htf_move_pct(&macro_htf.btc_klines, lookback.max(2))
        };
        let global = self.sentiment.global_score().await;
        let symbol_sentiment = self.sentiment.symbol_score(symbol).await.unwrap_or(0.0);
        let (symbol_htf_move, funding_rate) = {
            let states = self.states.read().await;
            match states.get(symbol) {
                Some(state) => (htf_move_pct(&state.htf_klines, lookback.max(2)), state.funding_rate),
                None => (0.0, 0.0),
            }
        };
        MlFeatureContext {
            btc_htf_move_pct: btc_move,
            global_sentiment: global,
            symbol_htf_move_pct: symbol_htf_move,
            funding_rate,
            symbol_sentiment,
            regime: self.llm_regime.regime().await,
        }
    }

    async fn sentiment_allows_signal(&self, signal: &PumpSignal) -> bool {
        let cfg = self.cfg().sentiment.clone();
        let side = if signal.price_change_pct >= 0.0 {
            Side::Long
        } else {
            Side::Short
        };
        let sym_score = self.sentiment.symbol_score(&signal.symbol).await;
        let snap = self.sentiment.snapshot().await;
        sentiment_allows(
            side,
            &signal.symbol,
            snap.global_score,
            sym_score,
            snap.fear_greed,
            &cfg,
        )
        .allows
    }

    async fn route_enhanced_signal(
        &self,
        signal: PumpSignal,
        klines: &[crate::exchange::KlineBar],
    ) -> Option<PumpSignal> {
        let ctx = self.ml_feature_context(&signal.symbol).await;
        let mut ml = self.ml.lock().await;
        let mut enhanced = match ml.enhance_signal_outcome(signal, Some(klines), &ctx) {
            EnhanceOutcome::Tradable(e) => e,
            EnhanceOutcome::MlRejected(r) => {
                drop(ml);
                self.save_shadow_signal(r, "ml_gate").await;
                return None;
            }
        };
        drop(ml);

        let cfg = self.cfg();
        let paper_relax =
            !cfg.execution.live_trading_enabled && cfg.execution.paper_relax_gates;

        // Phase 5: unified decision authority. Fuses ML win prob, expected
        // value, LLM regime alignment, and sentiment into go/no-go + sizing.
        if cfg.decision.enabled {
            if let Some(rejected) = self.apply_decision(&mut enhanced, &ctx, &cfg, paper_relax).await {
                self.save_shadow_signal(rejected, "decision_gate").await;
                return None;
            }
        }

        if !paper_relax && !self.sentiment_allows_signal(&enhanced).await {
            self.save_shadow_signal(enhanced, "sentiment_gate").await;
            return None;
        }
        Some(enhanced)
    }

    /// Run the decision engine against an ML-enhanced signal, annotate it with
    /// EV / reward-risk / reasoning, and (when approved) apply the conviction
    /// size & leverage multipliers on top of the ML Kelly base. Returns
    /// `Some(signal)` when the trade is rejected and should be shadowed;
    /// `None` when it's approved (or when paper-relax keeps it for data).
    async fn apply_decision(
        &self,
        signal: &mut PumpSignal,
        ctx: &MlFeatureContext,
        cfg: &AppConfig,
        paper_relax: bool,
    ) -> Option<PumpSignal> {
        let side_long = signal.price_change_pct >= 0.0;
        let take_profit = signal
            .projected_take_profits
            .first()
            .copied()
            .unwrap_or(signal.last_price);
        let inputs = crate::ai::DecisionInputs {
            win_prob: (signal.setup_probability_pct / 100.0).clamp(0.0, 1.0),
            side_long,
            entry: signal.last_price,
            stop_loss: signal.projected_stop_loss,
            take_profit,
            regime: ctx.regime.clone(),
            global_sentiment: ctx.global_sentiment,
            symbol_sentiment: ctx.symbol_sentiment,
        };
        let decision = crate::ai::DecisionEngine::decide(&cfg.decision, &inputs);

        let round2 = |x: f64| (x * 100.0).round() / 100.0;
        signal.expected_value_r = round2(decision.expected_value_r);
        signal.reward_risk = round2(decision.reward_risk);
        signal.decision_reason = decision.reason.clone();
        signal.message.push_str(&format!(" | {}", decision.reason));

        if decision.approved {
            signal.suggested_risk_pct *= decision.size_scale;
            signal.suggested_leverage =
                ((signal.suggested_leverage as f64) * decision.leverage_scale).round().max(1.0) as u32;
            return None;
        }

        // Rejected. In paper-relax we keep collecting data (ML Kelly sizing
        // left intact, decision reasoning recorded); otherwise shadow it.
        if paper_relax {
            None
        } else {
            Some(signal.clone())
        }
    }

    /// Shadow-resolve pending signals (executed or not) against real price action
    /// so the model learns from *every* setup, not only the few that opened a
    /// position. For each pending signal whose hold window has fully elapsed, we
    /// fetch the klines covering that window and label it win / loss / expired.
    async fn resolve_pending_signals_from_price(&self, max_signals: u32) -> u32 {
        let max_hold = self.cfg().trading.max_hold_sec.max(60) as i64;
        let now = Utc::now().timestamp();
        let pending = self.db.get_pending_signals(300).await.unwrap_or_default();
        // 60 one-minute bars before the signal are enough to recompute the full
        // technical feature set (EMA-26 / MACD need ~26).
        let feature_lookback_sec: i64 = 60 * 60;
        let mut processed = 0u32;
        let mut learned = 0u32;
        let mut wins = 0u32;
        let mut backfilled = 0u32;

        for sig in &pending {
            if processed >= max_signals {
                break;
            }
            let sig_id = sig.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            if sig_id == 0 {
                continue;
            }

            let gen_str = sig.get("generated_at").and_then(|v| v.as_str()).unwrap_or("");
            let Ok(gen_dt) = DateTime::parse_from_rfc3339(gen_str) else {
                continue;
            };
            let start = gen_dt.timestamp();
            // Require the full hold window (+1 min buffer) to have elapsed so the
            // outcome is final and not still in progress.
            if now < start + max_hold + 60 {
                continue;
            }

            // If this signal actually opened a position, let the trade-based
            // resolver own its outcome to avoid double counting.
            if let Ok(Some(_)) = self.db.get_position_by_signal_id(sig_id).await {
                continue;
            }

            let symbol = sig.get("symbol").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let entry = sig.get("last_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let sl = sig.get("projected_stop_loss").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tp = sig
                .get("projected_take_profits")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let side_long = sig.get("price_change_pct").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0;
            if entry <= 0.0 || sl <= 0.0 || tp <= 0.0 {
                continue;
            }

            let mut features: Vec<f64> = sig
                .get("ml_features")
                .and_then(|f| f.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_f64()).collect())
                .unwrap_or_default();

            processed += 1;
            let end = start + max_hold;
            // One fetch covers both the pre-signal bars (for feature backfill)
            // and the post-signal bars (for outcome resolution).
            let bars = match self
                .exchange
                .get_klines_range(&symbol, "Min1", start - feature_lookback_sec, end)
                .await
            {
                Ok(b) => b,
                Err(exc) => {
                    debug!("Signal resolve klines {symbol} skipped: {exc}");
                    continue;
                }
            };

            // Backfill features for historically featureless signals by replaying
            // the technical indicators on the bars up to the signal moment.
            if !features.iter().any(|&v| v != 0.0) {
                let pre: Vec<crate::exchange::KlineBar> =
                    bars.iter().filter(|b| b.timestamp <= start).cloned().collect();
                if pre.len() >= 20 {
                    let fv = TechnicalFeatureBuilder::feature_vector(&pre, None);
                    if fv.iter().any(|&v| v != 0.0) {
                        features = normalize_feature_vector(Some(&fv), FEATURE_DIM);
                        backfilled += 1;
                    }
                }
            }
            TechnicalFeatureBuilder::enrich_context_from_payload(&mut features, sig);
            if features.iter().any(|&v| v != 0.0) {
                let _ = self.db.set_signal_features(sig_id, &features).await;
            }
            if !features.iter().any(|&v| v != 0.0) {
                continue;
            }

            let Some((label, won)) = resolve_signal_outcome(&bars, start, side_long, sl, tp) else {
                continue;
            };
            let weight = shadow_resolve_weight(&self.cfg().learning, sig);
            if self.db.update_signal_outcome(sig_id, label).await.is_ok() {
                if label == "expired" {
                    self.ml.lock().await.record_outcome_soft(&features, 0.45, 0.3 * weight);
                } else {
                    self.ml.lock().await.record_outcome_weighted(&features, won, weight);
                }
                learned += 1;
                if won {
                    wins += 1;
                }
                let is_shadow = sig.get("shadow_only").and_then(|v| v.as_bool()).unwrap_or(false);
                if is_shadow {
                    let _ = self
                        .db
                        .log_event(
                            "shadow_signal_resolved",
                            &format!(
                                "Shadow resolved: {} {label} (weight {weight:.2})",
                                sig.get("symbol").and_then(|v| v.as_str()).unwrap_or("?")
                            ),
                            Some(json!({
                                "signal_id": sig_id,
                                "symbol": symbol,
                                "outcome": label,
                                "weight": weight,
                                "reject_reason": sig.get("reject_reason"),
                                "won": won,
                            })),
                        )
                        .await;
                }
            }
        }

        if learned > 0 {
            let stats = self.ml.lock().await.online_stats();
            info!(
                "Online model learned from {learned} shadow-resolved signal(s) ({wins} win, {backfilled} backfilled) — {} total samples",
                stats.get("samples").and_then(|v| v.as_u64()).unwrap_or(0)
            );
            let _ = self
                .db
                .log_event(
                    "model_learn",
                    &format!(
                        "Online model updated from {learned} shadow-resolved signal(s) ({backfilled} feature-backfilled)"
                    ),
                    Some(stats),
                )
                .await;
        }
        learned
    }

    /// Mark closed positions' signals as win/loss, feed the online model, and
    /// apply circuit-breaker logic for each resolved trade.
    async fn resolve_outcomes_and_learn(&self) -> crate::error::Result<()> {
        // Lift the circuit-breaker pause if its cooldown has elapsed.
        {
            let mut risk = self.risk.write().await;
            let _ = risk.lift_circuit_breaker_if_expired().await;
        }

        let pending = self.db.get_unresolved_closed_positions(200).await?;
        if pending.is_empty() {
            return Ok(());
        }
        let mut learned = 0u32;
        for row in &pending {
            let signal_id = row.get("signal_id").and_then(|v| v.as_i64()).unwrap_or(0);
            if signal_id == 0 {
                continue;
            }
            let pnl = row.get("realized_pnl").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let won = pnl > 0.0;
            let outcome = if won { "win" } else { "loss" };
            self.db.update_signal_outcome(signal_id, outcome).await?;

            // Notify the risk manager so circuit breakers fire when needed.
            let symbol = row
                .get("payload")
                .and_then(|p| p.get("symbol"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            {
                let mut risk = self.risk.write().await;
                let before_ks = risk.metrics_json(0, false)["kill_switch"]
                    .as_bool()
                    .unwrap_or(false);
                let _ = risk.record_trade_outcome(&symbol, won).await;
                let after = risk.metrics_json(0, false);
                // Fire alert if kill switch was just auto-activated.
                if !before_ks && after["kill_switch"].as_bool().unwrap_or(false) {
                    self.alerter
                        .fire(
                            "kill_switch",
                            &format!("Kill switch auto-activated by max-drawdown halt on {symbol}"),
                        )
                        .await;
                }
                // Fire alert if circuit breaker just tripped.
                if after["circuit_breaker_active"].as_bool().unwrap_or(false) {
                    let remaining = after["circuit_breaker_remaining_sec"].as_i64().unwrap_or(0);
                    self.alerter
                        .fire(
                            "circuit_breaker",
                            &format!(
                                "Loss streak circuit breaker tripped on {symbol} — paused for {remaining}s"
                            ),
                        )
                        .await;
                }
            }

            // Telegram trade-close notification.
            {
                let close_reason = row
                    .get("exit_reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("closed");
                let event_key = match close_reason {
                    r if r.contains("tp") || r.contains("take_profit") => "tp_hit",
                    r if r.contains("stop") || r.contains("sl") || r.contains("cut") => "cut_loss",
                    _ => "position_closed",
                };
                let exit_price = row.get("exit_price").and_then(json_f64).unwrap_or(0.0);
                let side = row
                    .get("side")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_uppercase())
                    .unwrap_or_else(|| "?".into());
                let entry_price = row.get("entry_price").and_then(json_f64).unwrap_or(0.0);
                let details = json!({
                    "symbol": symbol,
                    "side": side,
                    "strategy": row.get("strategy").and_then(|v| v.as_str()),
                    "entry_price": entry_price,
                    "exit_price": exit_price,
                    "pnl": pnl,
                    "reason": close_reason,
                });
                let exit_label = if exit_price > 0.0 {
                    format!("{exit_price:.6}")
                } else {
                    "market".into()
                };
                let msg = if pnl >= 0.0 {
                    format!("{side} {symbol} closed @ {exit_label} — PnL: +{pnl:.4} USDT")
                } else {
                    format!("{side} {symbol} closed @ {exit_label} — PnL: {pnl:.4} USDT")
                };
                self.alerter.trade_event(event_key, &msg, Some(&details)).await;
            }

            let features: Vec<f64> = row
                .get("payload")
                .and_then(|p| p.get("ml_features"))
                .and_then(|f| f.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_f64()).collect())
                .unwrap_or_default();
            if features.iter().any(|&v| v != 0.0) {
                let entry_price = row.get("entry_price").and_then(json_f64).unwrap_or(0.0);
                let side = row.get("side").and_then(|v| v.as_str()).unwrap_or("long");
                let size = row.get("size").and_then(json_f64).unwrap_or(0.0);
                let sl = row
                    .get("payload")
                    .and_then(|p| p.get("projected_stop_loss"))
                    .and_then(json_f64)
                    .unwrap_or(0.0);
                let side_long = side == "long";
                let r_mult = compute_r_multiple(pnl, entry_price, sl, size, side_long);
                let soft = soft_label_from_r(r_mult);
                let weight = self.cfg().ml.trade_outcome_weight(won);
                let mut ml = self.ml.lock().await;
                ml.record_outcome_soft(&features, soft, weight);
                ml.record_r_outcome(r_mult, won);
                drop(ml);
                let _ = self
                    .db
                    .set_signal_outcome_meta(signal_id, r_mult, soft)
                    .await;
                learned += 1;
            }
        }
        if learned > 0 {
            let stats = self.ml.lock().await.online_stats();
            info!(
                "Online model learned from {} new trade(s) — {} total samples",
                learned,
                stats.get("samples").and_then(|v| v.as_u64()).unwrap_or(0)
            );
            let _ = self
                .db
                .log_event(
                    "model_learn",
                    &format!("Online model updated from {learned} resolved trade(s)"),
                    Some(stats),
                )
                .await;
        }
        {
            let mut risk = self.risk.write().await;
            let _ = risk.reconcile_pnl_from_db().await;
        }
        Ok(())
    }

    /// Replay resolved signals (with stored features) into the online model. Safe
    /// to call repeatedly; weights converge toward the historical evidence.
    ///
    /// Weighting strategy applied during bootstrap (mirrors the live loop):
    ///   - Real trade wins/losses → `ml.trade_win_weight` / `ml.trade_loss_weight`
    ///   - Shadow win / loss      → shadow_resolve_weight
    ///   - Shadow expired         → soft label 0.45, weight 0.3
    async fn bootstrap_online_model(&self) -> u32 {
        let rows = self
            .db
            .get_resolved_signals_with_features(1000)
            .await
            .unwrap_or_default();
        let threshold_pct = self.cfg().ml.supervised_threshold * 100.0;
        let hard_gate = self.cfg().ml.hard_ml_gate;
        let mut ml = self.ml.lock().await;
        let mut trained = 0u32;
        let mut skipped_pregame = 0u32;
        for sig in &rows {
            let features: Vec<f64> = sig
                .get("ml_features")
                .and_then(|f| f.as_array())
                .map(|arr| arr.iter().filter_map(|x| x.as_f64()).collect())
                .unwrap_or_default();
            if !features.iter().any(|&v| v != 0.0) {
                continue;
            }

            let outcome = sig.get("outcome").and_then(|v| v.as_str()).unwrap_or("");

            // When hard_ml_gate is active, skip pre-gate-era tradable signals that would
            // never have fired today. Shadow ML rejects are always included — they
            // train the model on setups the gate blocked intentionally.
            if hard_gate {
                let is_shadow = sig.get("shadow_only").and_then(|v| v.as_bool()).unwrap_or(false);
                if !is_shadow {
                    let setup_prob = sig.get("setup_probability_pct").and_then(|v| v.as_f64()).unwrap_or(-1.0);
                    if setup_prob >= 0.0 && setup_prob < threshold_pct {
                        skipped_pregame += 1;
                        continue;
                    }
                }
            }

            // Assign weights mirroring the live training loop.
            let is_real_trade = sig.get("from_trade").and_then(|v| v.as_bool()).unwrap_or(false);
            let weight = if is_real_trade {
                self.cfg().ml.trade_outcome_weight(outcome == "win")
            } else {
                shadow_resolve_weight(&self.cfg().learning, sig)
            };
            if outcome == "expired" {
                ml.record_outcome_soft(&features, 0.45, 0.3 * weight);
            } else if is_real_trade {
                ml.record_outcome_weighted(&features, outcome == "win", weight);
            } else {
                ml.record_outcome_weighted(&features, outcome == "win", weight);
            }
            trained += 1;
        }
        if trained > 0 {
            info!(
                "Bootstrapped online model from {trained} resolved signal(s) ({skipped_pregame} pre-gate skipped)"
            );
        }
        trained
    }

    /// Save a signal for shadow-only training (no execution).
    async fn save_shadow_signal(&self, signal: crate::signals::PumpSignal, reject_reason: &str) {
        let learning = &self.cfg().learning;
        if !learning.enabled {
            return;
        }
        if reject_reason == "ml_gate" && !learning.shadow_ml_rejects {
            return;
        }

        if let Ok(pending) = self.db.count_pending_shadow_signals().await {
            if pending >= learning.shadow_max_pending as i64 {
                debug!(
                    "Shadow queue full ({pending}) — skipping shadow save for {}",
                    signal.symbol
                );
                return;
            }
        }

        if let Ok(recent) = self
            .db
            .count_recent_shadow_signals(&signal.symbol, 3600)
            .await
        {
            if recent >= learning.shadow_max_per_symbol_hour as i64 {
                debug!(
                    "Shadow rate limit for {} ({recent}/hr) — skipping",
                    signal.symbol
                );
                return;
            }
        }

        // Dedupe: one shadow per symbol+side within the dedupe window.
        let side_long = signal.price_change_pct > 0.0;
        let cooldown = SHADOW_DEDUPE_SEC;
        if let Ok(dup) = self
            .db
            .count_recent_shadow_signals_side(&signal.symbol, side_long, cooldown)
            .await
        {
            if dup > 0 {
                debug!(
                    "Shadow dedupe for {} {} (cooldown {cooldown}s) — skipping",
                    signal.symbol,
                    if side_long { "long" } else { "short" }
                );
                return;
            }
        }

        match self.db.insert_shadow_signal(&signal, reject_reason).await {
            Ok(id) => {
                let _ = self
                    .db
                    .log_event(
                        "shadow_signal_saved",
                        &format!(
                            "Shadow signal saved: {} score={:.1} reason={reject_reason}",
                            signal.symbol, signal.composite_score
                        ),
                        Some(json!({
                            "signal_id": id,
                            "symbol": signal.symbol,
                            "reject_reason": reject_reason,
                            "setup_probability_pct": signal.setup_probability_pct,
                        })),
                    )
                    .await;
                debug!("Shadow signal {id} saved for {} ({reject_reason})", signal.symbol);
            }
            Err(exc) => warn!("Failed to save shadow signal for {}: {exc}", signal.symbol),
        }
    }

    async fn refresh_macro_htf(&self, interval: &str, lookback: u32) {
        let mut btc_klines = Vec::new();
        match self.exchange.get_klines("BTC_USDT", interval).await {
            Ok(mut bars) => {
                if bars.len() > lookback as usize {
                    bars = bars[bars.len() - lookback as usize..].to_vec();
                }
                btc_klines = bars;
            }
            Err(exc) => debug!("Macro HTF refresh BTC_USDT skipped: {exc}"),
        }
        let mut eth_klines = Vec::new();
        match self.exchange.get_klines("ETH_USDT", interval).await {
            Ok(mut bars) => {
                if bars.len() > lookback as usize {
                    bars = bars[bars.len() - lookback as usize..].to_vec();
                }
                eth_klines = bars;
            }
            Err(exc) => debug!("Macro HTF refresh ETH_USDT skipped: {exc}"),
        }
        *self.macro_htf.write().await = MacroHtfState {
            btc_klines,
            eth_klines,
        };
    }

    /// Open-position count for capacity gating (DB hit at most once per second).
    async fn current_open_positions(&self) -> i64 {
        let now = Utc::now().timestamp();
        let last = self.last_open_count_at.load(Ordering::Relaxed);
        if now.saturating_sub(last) < 1 {
            let cached = self.cached_open_positions.load(Ordering::Relaxed);
            if cached >= 0 {
                return cached;
            }
        }
        let n = self.db.count_open_positions().await.unwrap_or(0);
        self.cached_open_positions.store(n, Ordering::Relaxed);
        self.last_open_count_at.store(now, Ordering::Relaxed);
        n
    }

    async fn at_position_capacity(&self) -> bool {
        let n = self.current_open_positions().await;
        n >= self.cfg().risk.max_concurrent_positions as i64
    }

    fn note_position_opened(&self) {
        let cached = self.cached_open_positions.load(Ordering::Relaxed);
        if cached >= 0 {
            self.cached_open_positions
                .store(cached + 1, Ordering::Relaxed);
        }
    }

    fn note_position_closed(&self) {
        let cached = self.cached_open_positions.load(Ordering::Relaxed);
        if cached > 0 {
            self.cached_open_positions
                .store(cached - 1, Ordering::Relaxed);
        }
    }

    /// Lightweight scan entry when the position book is full — skips ML/DB signal work.
    async fn record_capacity_reject(&self, signal: &PumpSignal, ticker: &TickerSnapshot) {
        let max = self.cfg().risk.max_concurrent_positions;
        let side = if signal.price_change_pct >= 0.0 {
            "long"
        } else {
            "short"
        };
        self.maybe_record_scan(
            &signal.symbol,
            "rejected",
            &format!("Max positions ({max}) reached"),
            Some(signal.composite_score),
            Some(signal.confluence_count),
            Some(side),
            ticker,
        )
        .await;
    }

    /// Process a batch of WebSocket ticker updates.
    ///
    /// Takes `Arc<Self>` so that signal-execution tasks can be spawned without
    /// blocking the ticker-receive loop. This is the primary fix for the freeze:
    /// previously this function was awaited inline, meaning any slow REST call
    /// (order placement, SL/TP setup, leverage change) would stall all ticker
    /// processing until it completed.
    async fn process_tickers(self: Arc<Self>, tickers: Vec<TickerSnapshot>) {
        // Stamp the last time we received live ticker data for WS-staleness detection.
        self.last_tick_at.store(Utc::now().timestamp(), Ordering::Relaxed);

        let tracked: Vec<String> = self.tracked_symbols.read().await.clone();
        let cfg = self.cfg();

        // Batch-update ticker_map with all tickers in a single write lock rather
        // than acquiring and releasing it once per ticker in the batch.
        {
            let mut tmap = self.ticker_map.write().await;
            for ticker in &tickers {
                tmap.insert(ticker.symbol.clone(), ticker.clone());
            }
        }

        let at_capacity = self.at_position_capacity().await;

        for ticker in tickers {
            if !tracked.contains(&ticker.symbol) {
                continue;
            }

            let symbol = ticker.symbol.clone();
            // Update state and evaluate the AI candidate generator under one
            // lock scope; stamp the cooldown as soon as a candidate is taken so
            // subsequent ticks don't re-emit while routing is in flight.
            let candidate = {
                let mut states = self.states.write().await;
                let state = states
                    .entry(symbol.clone())
                    .or_insert_with(|| SymbolState::new(symbol.clone()));
                state.update_ticker(&ticker);
                state.last_scanned_at = Some(Utc::now());
                match crate::signals::AiCandidateEngine::evaluate(&cfg, state) {
                    Some(signal) => {
                        state.last_signal_at = Some(Utc::now());
                        Some((signal, state.klines.clone()))
                    }
                    None => None,
                }
            };

            if let Some((signal, klines)) = candidate {
                if at_capacity {
                    self.record_capacity_reject(&signal, &ticker).await;
                    continue;
                }
                // Serialize signal execution so concurrent candidates cannot race
                // past max-position checks (SQLite read-then-write) or block the UI
                // snapshot path with dozens of parallel DB writes.
                let inner = Arc::clone(&self);
                let ticker_clone = ticker.clone();
                let exec_sem = Arc::clone(&self.signal_exec);
                tokio::spawn(async move {
                    let _permit = exec_sem.acquire().await.ok();
                    inner
                        .process_ai_candidate(signal, klines, &ticker_clone)
                        .await;
                });
            }
        }

        // Exit monitoring with 10 open positions can mean 10+ DB round-trips per
        // second — never run it on the ticker hot-path; spawn and use try_write.
        let now = Utc::now().timestamp();
        let last = self.last_pos_monitor_at.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= 1 {
            self.last_pos_monitor_at.store(now, Ordering::Relaxed);
            let inner = Arc::clone(&self);
            tokio::spawn(async move {
                let map = inner.monitor_ticker_map().await;
                if map.is_empty() {
                    return;
                }
                if let Ok(mut risk) = inner.risk.try_write() {
                    let _ = inner.paper.lock().await.monitor_positions(&map, &mut risk).await;
                    let _ = inner
                        .live_monitor
                        .lock()
                        .await
                        .monitor(&map, &mut risk)
                        .await;
                }
                // Positions may have closed during monitoring — force a fresh count
                // on the next capacity check so freed slots are available promptly.
                inner
                    .cached_open_positions
                    .store(-1, Ordering::Relaxed);
            });
        }
    }

    /// Build a minimal ticker map for open-position monitoring only.
    /// Avoids cloning the full tracked-symbol ticker map on every WS batch.
    async fn monitor_ticker_map(&self) -> HashMap<String, TickerSnapshot> {
        let positions = self.db.get_open_positions().await.unwrap_or_default();
        if positions.is_empty() {
            return HashMap::new();
        }
        let tickers = self.ticker_map.read().await;
        let mut map = HashMap::with_capacity(positions.len());
        for pos in positions {
            let Some(symbol) = pos.get("symbol").and_then(|v| v.as_str()) else {
                continue;
            };
            if let Some(ticker) = tickers.get(symbol) {
                map.insert(symbol.to_string(), ticker.clone());
            }
        }
        map
    }

    /// Route a fresh AI candidate through the ML/sentiment pipeline and emit
    /// it for execution when tradable. Rejected candidates become shadow
    /// signals inside `route_enhanced_signal` so the model still learns.
    async fn process_ai_candidate(
        &self,
        signal: PumpSignal,
        klines: Vec<crate::exchange::KlineBar>,
        ticker: &TickerSnapshot,
    ) {
        if self.at_position_capacity().await {
            self.record_capacity_reject(&signal, ticker).await;
            return;
        }

        let side = if signal.price_change_pct >= 0.0 { "long" } else { "short" };
        self.maybe_record_scan(
            &signal.symbol,
            "candidate",
            &signal.message,
            Some(signal.composite_score),
            Some(signal.confluence_count),
            Some(side),
            ticker,
        )
        .await;

        if let Some(enhanced) = self.route_enhanced_signal(signal, &klines).await {
            self.emit_signal(enhanced).await;
        }
    }

    async fn maybe_record_scan(
        &self,
        symbol: &str,
        action: &str,
        message: &str,
        score: Option<f64>,
        confluence_count: Option<u32>,
        side: Option<&str>,
        ticker: &TickerSnapshot,
    ) {
        let now = Utc::now().timestamp();
        let gate_key = format!("{action}|{message}");
        let should_log = {
            let gate = self.scan_log_gate.read().await;
            match gate.get(symbol) {
                Some((last_ts, last_key)) => {
                    action == "signal"
                        || *last_key != gate_key
                        || now - *last_ts >= 45
                }
                None => true,
            }
        };
        if !should_log {
            return;
        }
        self.scan_log_gate
            .write()
            .await
            .insert(symbol.to_string(), (now, gate_key));
        self.push_scan_event(symbol, action, message, score, confluence_count, side, ticker)
            .await;
    }

    async fn push_scan_event(
        &self,
        symbol: &str,
        action: &str,
        message: &str,
        score: Option<f64>,
        confluence_count: Option<u32>,
        side: Option<&str>,
        ticker: &TickerSnapshot,
    ) {
        let event = json!({
            "symbol": symbol,
            "action": action,
            "message": message,
            "composite_score": score,
            "confluence_count": confluence_count,
            "side": side,
            "last_price": ticker.last_price,
            "change_24h_pct": (ticker.rise_fall_rate * 100.0 * 100.0).round() / 100.0,
            "scanned_at": Utc::now().to_rfc3339(),
        });
        let mut scans = self.latest_scans.write().await;
        scans.insert(0, event);
        scans.truncate(200);
    }

    async fn emit_signal(&self, mut signal: PumpSignal) {
        if self.at_position_capacity().await {
            debug!(
                "Skipping signal for {} — max positions ({}) reached",
                signal.symbol,
                self.cfg().risk.max_concurrent_positions
            );
            return;
        }

        let id = match self.db.insert_signal(&signal).await {
            Ok(id) => id,
            Err(exc) => {
                warn!("Failed to save signal: {exc}");
                return;
            }
        };
        signal.signal_id = Some(id);
        let payload = signal.to_payload();
        {
            let mut latest = self.latest_signals.write().await;
            latest.insert(0, payload.clone());
            latest.truncate(100);
        }
        if let Some(ticker) = self.ticker_map.read().await.get(&signal.symbol).cloned() {
            self.push_scan_event(
                &signal.symbol,
                "signal",
                &signal.message,
                Some(signal.composite_score),
                Some(signal.confluence_count),
                Some(if signal.price_change_pct >= 0.0 { "long" } else { "short" }),
                &ticker,
            )
            .await;
        }
        let event_msg = format!(
            "Signal: {} score={:.1}",
            signal.symbol, signal.composite_score
        );
        let _ = self
            .db
            .log_event("signal", &event_msg, Some(payload))
            .await;

        // Gate execution when the WS feed is stale — prices may be stale too.
        let stale_threshold = self.cfg().risk.ws_stale_sec as i64;
        let last_tick = self.last_tick_at.load(Ordering::Relaxed);
        let ws_stale = last_tick > 0 && Utc::now().timestamp() - last_tick > stale_threshold;
        if ws_stale {
            let age = Utc::now().timestamp() - last_tick;
            warn!(
                "Skipping signal execution for {} — WS feed stale (last tick {age}s ago)",
                signal.symbol,
            );
            self.alerter
                .fire(
                    "ws_stale",
                    &format!("WebSocket feed stale — last tick {age}s ago. Signal for {} blocked.", signal.symbol),
                )
                .await;
            if let Some(id) = signal.signal_id {
                let _ = self.db.set_signal_reject_reason(id, "ws_stale").await;
            }
            return;
        }

        // Execute signal: paper path is fast (no REST calls), live path splits
        // risk.prepare / exchange REST calls / risk.commit to minimize lock
        // contention. Neither path blocks the ticker loop because emit_signal
        // is always called from a spawned background task (see process_tickers).
        let is_live = self.live.lock().await.is_live();
        let mode = if is_live { "live" } else { "paper" };

        let open_result = if !is_live {
            // Paper: only risk.write() — no live-trader mutex (contract lookup is best-effort).
            let mut risk = self.risk.write().await;
            match risk.try_open_from_signal(&signal, true).await {
                Ok(Some(id)) => {
                    let contract_max = if let Ok(live) = self.live.try_lock() {
                        live.max_leverage_for_symbol(&signal.symbol)
                    } else {
                        signal.suggested_leverage as i32
                    };
                    let corrected =
                        signal.suggested_leverage.min(contract_max as u32).max(1) as i32;
                    let _ = self.db.update_position_leverage(id, corrected).await;
                    Ok(Some(id))
                }
                Ok(None) => Ok(None),
                Err(exc) => Err(exc),
            }
        } else {
            // Live: split locks so risk.write() is never held during REST calls.
            // Step 1 — prepare (brief risk.write(), DB ops only).
            let mut prepared = {
                let mut risk = self.risk.write().await;
                match risk.prepare_open_from_signal(&signal).await {
                    Ok(Some(p)) => p,
                    Ok(None) => {
                        if let Some(id) = signal.signal_id {
                            let _ = self.db.set_signal_reject_reason(id, "trade_blocked").await;
                        }
                        return;
                    }
                    Err(exc) => {
                        warn!("Risk prepare failed for {}: {exc}", signal.symbol);
                        return;
                    }
                }
            }; // risk.write() released here

            // Step 2 — submit order to exchange (live.lock(), REST calls).
            // risk lock is NOT held during this step.
            let (success, limit_order_id) = {
                let live = self.live.lock().await;
                match live.execute_live_order(&signal, &mut prepared).await {
                    Ok(v) => v,
                    Err(exc) => {
                        warn!("Live order execution failed for {}: {exc}", signal.symbol);
                        return;
                    }
                }
            }; // live.lock() released here

            if !success {
                if let Some(id) = signal.signal_id {
                    let _ = self.db.set_signal_reject_reason(id, "trade_blocked").await;
                }
                return;
            }

            // Step 3 — commit position to DB (brief risk.write(), fast).
            let commit_result = {
                let mut risk = self.risk.write().await;
                risk.commit_open_from_signal(&signal, false, &prepared).await
            };

            // Step 4 — schedule limit-order TTL cancel now that we have the pos_id.
            if let (Ok(Some(pos_id)), Some(order_id)) = (&commit_result, limit_order_id) {
                let cancel_client = self.live.lock().await.client().clone();
                let cancel_symbol = signal.symbol.clone();
                let limit_ttl = self.cfg().execution.limit_ttl_sec;
                let db_clone = self.db.clone();
                let pending_pos_id = *pos_id;
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(limit_ttl)).await;
                    match cancel_client.cancel_order(&cancel_symbol, &order_id).await {
                        Ok(_) => {
                            let _ = db_clone.log_event(
                                "limit_order_cancelled",
                                &format!("Limit TTL expired for {} — cancelled {}", cancel_symbol, order_id),
                                None,
                            ).await;
                            if let Ok(Some(p)) = db_clone.get_position_by_id(pending_pos_id).await {
                                let filled = p
                                    .get("exchange_position_id")
                                    .and_then(|v| v.as_i64())
                                    .is_some();
                                if !filled {
                                    let entry = p
                                        .get("entry_price")
                                        .and_then(|v| v.as_f64())
                                        .unwrap_or(0.0);
                                    let _ = db_clone
                                        .close_position_synced(
                                            pending_pos_id,
                                            "limit_ttl_expired",
                                            1.0,
                                            0.0,
                                            entry,
                                        )
                                        .await;
                                }
                            }
                        }
                        Err(e) => {
                            debug!("Limit cancel for {}: {e}", cancel_symbol);
                        }
                    }
                });
            }

            commit_result
        };

        let contract_size = if let Ok(live) = self.live.try_lock() {
            live.contract_size(&signal.symbol)
        } else {
            1.0
        };

        match open_result {
            Ok(Some(pos_id)) => {
                self.note_position_opened();
                info!("{mode} position opened id={pos_id} {}", signal.symbol);
                let side_str = if signal.price_change_pct >= 0.0 {
                    "LONG"
                } else {
                    "SHORT"
                };
                let (size, entry, leverage) = self
                    .db
                    .get_position_by_id(pos_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|p| {
                        (
                            json_f64(p.get("size").unwrap_or(&Value::Null)).unwrap_or(0.0),
                            json_f64(p.get("entry_price").unwrap_or(&Value::Null))
                                .unwrap_or(signal.last_price),
                            p.get("leverage")
                                .and_then(|v| v.as_i64())
                                .unwrap_or(signal.suggested_leverage as i64),
                        )
                    })
                    .unwrap_or((0.0, signal.last_price, signal.suggested_leverage as i64));
                let margin = margin_usdt(size, contract_size, entry, leverage);
                let details = json!({
                    "symbol": signal.symbol,
                    "side": side_str,
                    "strategy": signal.strategy,
                    "entry_price": entry,
                    "size": size,
                    "leverage": leverage,
                    "margin_usdt": margin,
                    "mode": mode,
                });
                let msg = format!(
                    "Opened {side_str} {} @ {entry:.6} · margin {margin:.2} USDT",
                    signal.symbol
                );
                self.alerter
                    .trade_event("position_opened", &msg, Some(&details))
                    .await;
            }
            Ok(None) => {
                if let Some(id) = signal.signal_id {
                    let _ = self.db.set_signal_reject_reason(id, "trade_blocked").await;
                }
            }
            Err(exc) => warn!("Open from signal failed: {exc}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::KlineBar;

    fn bar(ts: i64, high: f64, low: f64) -> KlineBar {
        KlineBar {
            symbol: "X_USDT".into(),
            timestamp: ts,
            open: (high + low) / 2.0,
            high,
            low,
            close: (high + low) / 2.0,
            volume: 0.0,
            amount: 0.0,
        }
    }

    #[test]
    fn long_take_profit_first_is_win() {
        let bars = vec![bar(100, 101.0, 99.5), bar(160, 105.0, 100.5)];
        // entry ~100, tp 104, sl 98
        assert_eq!(
            resolve_signal_outcome(&bars, 100, true, 98.0, 104.0),
            Some(("win", true))
        );
    }

    #[test]
    fn long_stop_loss_first_is_loss() {
        let bars = vec![bar(100, 101.0, 97.0)];
        assert_eq!(
            resolve_signal_outcome(&bars, 100, true, 98.0, 104.0),
            Some(("loss", false))
        );
    }

    #[test]
    fn short_take_profit_first_is_win() {
        let bars = vec![bar(100, 100.5, 95.0)];
        // short: tp below entry (96), sl above (102)
        assert_eq!(
            resolve_signal_outcome(&bars, 100, false, 102.0, 96.0),
            Some(("win", true))
        );
    }

    #[test]
    fn no_touch_within_window_is_expired() {
        let bars = vec![bar(100, 101.0, 99.0), bar(160, 101.5, 99.5)];
        assert_eq!(
            resolve_signal_outcome(&bars, 100, true, 90.0, 110.0),
            Some(("expired", false))
        );
    }

    #[test]
    fn bars_before_signal_are_ignored() {
        // The only price spike happens before the signal timestamp.
        let bars = vec![bar(40, 110.0, 80.0), bar(160, 101.0, 99.0)];
        assert_eq!(
            resolve_signal_outcome(&bars, 100, true, 90.0, 109.0),
            Some(("expired", false))
        );
    }

    #[test]
    fn empty_bars_stay_pending() {
        assert_eq!(resolve_signal_outcome(&[], 100, true, 98.0, 104.0), None);
    }

    #[test]
    fn shadow_resolve_weight_by_reason() {
        use crate::config::LearningConfig;
        let cfg = LearningConfig::default();
        let ml = json!({ "shadow_only": true, "reject_reason": "ml_gate" });
        let near = json!({ "shadow_only": true, "reject_reason": "confluence_near_miss" });
        let blocked = json!({ "shadow_only": true, "reject_reason": "trade_blocked" });
        assert!((shadow_resolve_weight(&cfg, &ml) - 0.5).abs() < f64::EPSILON);
        assert!((shadow_resolve_weight(&cfg, &near) - 0.3).abs() < f64::EPSILON);
        assert!((shadow_resolve_weight(&cfg, &blocked) - 1.0).abs() < f64::EPSILON);
    }
}
