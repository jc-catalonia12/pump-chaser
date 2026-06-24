//! Scanner orchestration with background kline + ticker loops.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::config::AppConfig;
use crate::db::Database;
use crate::exchange::{MexcClient, TickerSnapshot};
use crate::execution::{reconcile_on_boot, LivePositionMonitor, LiveTrader, PaperTrader};
use crate::ml::features::{normalize_feature_vector, TechnicalFeatureBuilder, FEATURE_DIM};
use crate::ml::{EnhanceOutcome, MlPipeline};
use crate::risk::RiskManager;
use crate::signals::confluence::{ConfluenceEngine, ScanDiagnosis};
use crate::signals::{PumpSignal, SymbolState, SymbolStates};
use crate::utils::{Alerter, UserSecrets};

const VALID_TRADING_MODES: &[&str] = &["confluence", "pump", "scalp", "both", "all"];

/// How often the learning loop resolves closed trades and trains the model.
const LEARNING_INTERVAL_SEC: u64 = 30;

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

pub struct ScannerService {
    inner: Arc<ScannerInner>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

struct ScannerInner {
    config: Arc<AppConfig>,
    db: Arc<Database>,
    risk: Arc<RwLock<RiskManager>>,
    exchange: MexcClient,
    confluence: ConfluenceEngine,
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
    alerter: Arc<Alerter>,
}

impl ScannerService {
    pub fn new(
        config: Arc<AppConfig>,
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
        let live_client = crate::exchange::MexcPrivateClient::from_secrets(&config.mexc, &secrets_snap);
        let alerter = Arc::new(Alerter::new(config.alerts.clone()).with_secrets(secrets.clone()));
        let live_monitor =
            LivePositionMonitor::new(config.clone(), db.clone(), live_client).with_alerter(alerter.clone());
        let inner = Arc::new(ScannerInner {
            config: config.clone(),
            db: db.clone(),
            risk: risk.clone(),
            exchange,
            confluence: ConfluenceEngine::new(config.clone()),
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
            alerter,
        });
        Ok(Self {
            inner,
            tasks: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn active_strategies(&self) -> Vec<&str> {
        let mode = self.inner.config.trading.mode.as_str();
        let mut active = Vec::new();
        if matches!(mode, "confluence" | "all") && self.inner.config.confluence.enabled {
            active.push("confluence");
        }
        if matches!(mode, "pump" | "both" | "all") {
            active.push("pump");
        }
        if matches!(mode, "scalp" | "both" | "all") && self.inner.config.scalp.enabled {
            active.push("scalp");
        }
        active
    }

    pub fn get_trading_settings(&self) -> Value {
        json!({
            "trading_mode": self.inner.config.trading.mode,
            "active_strategies": self.active_strategies(),
            "scalp_enabled": self.inner.config.scalp.enabled,
            "scalp_active": self.active_strategies().contains(&"scalp"),
            "confluence_enabled": self.inner.config.confluence.enabled,
            "valid_modes": VALID_TRADING_MODES,
        })
    }

    pub async fn get_watchlist_settings(&self) -> Value {
        let tracked = self.inner.tracked_symbols.read().await;
        json!({
            "mode": self.inner.config.watchlist.mode,
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

    pub async fn get_tracked_symbols(&self) -> Value {
        let order = self.inner.tracked_symbols.read().await.clone();
        let states = self.inner.states.read().await;
        let tickers = self.inner.ticker_map.read().await;
        let lookback = self.inner.config.scanner.kline_lookback_bars;
        let interval = self.inner.config.scanner.kline_interval.clone();
        let refresh_sec = self.inner.config.scanner.kline_refresh_sec;
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
                    .and_then(|s| s.last_confluence_at)
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
                monitoring.push("Confluence: volume, zone, structure, liquidity".into());

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
            "mode": self.inner.config.watchlist.mode,
            "trading_mode": self.inner.config.trading.mode,
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
        let contract_sizes: std::collections::HashMap<String, f64> = {
            let live = self.inner.live.lock().await;
            positions
                .iter()
                .filter_map(|p| p.get("symbol").and_then(|v| v.as_str()))
                .map(|s| (s.to_string(), live.contract_size(s)))
                .collect()
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

            if let Some(obj) = p.as_object_mut() {
                obj.insert("mark_price".into(), json!(mark));
                obj.insert("contract_size".into(), json!(contract_size));
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
        let scalp = open
            .iter()
            .filter(|p| p.get("strategy").and_then(|v| v.as_str()) == Some("scalp"))
            .count() as i64;
        let confluence = open
            .iter()
            .filter(|p| p.get("strategy").and_then(|v| v.as_str()) == Some("confluence"))
            .count() as i64;
        let risk = self.inner.risk.read().await;
        let stale_threshold = self.inner.config.risk.ws_stale_sec as i64;
        let last_tick = self.inner.last_tick_at.load(Ordering::Relaxed);
        let ws_stale = if last_tick == 0 {
            false // still warming up — not yet considered stale
        } else {
            Utc::now().timestamp() - last_tick > stale_threshold
        };
        risk.metrics_json(open_n, scalp, confluence, ws_stale)
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
        let conf_stats = self
            .inner
            .db
            .get_trade_stats(200, Some("confluence"))
            .await
            .unwrap_or_else(|_| json!({}));
        let model = self.inner.ml.lock().await.learning_status();
        let ml = &self.inner.config.ml;
        let learning = &self.inner.config.learning;
        json!({
            "enabled": learning.enabled,
            "shadow_ml_rejects": learning.shadow_ml_rejects,
            "shadow_near_miss": learning.shadow_near_miss,
            "confluence_trade_stats": conf_stats,
            "model": model,
            "message": format!(
                "Continuous learning — trade W/L {:.1}×/{:.1}×, ML rejects {:.2}×, near-miss {:.2}×",
                ml.trade_win_weight,
                ml.trade_loss_weight,
                learning.shadow_ml_reject_weight,
                learning.shadow_near_miss_weight,
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
        let new_client = crate::exchange::MexcPrivateClient::from_secrets(
            &self.inner.config.mexc,
            &secrets,
        );
        self.inner.live_monitor.lock().await.update_client(new_client);
    }

    pub async fn sync_exchange_positions(&self) -> Value {
        self.inner.live.lock().await.sync_exchange_positions().await
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
                let mut risk = self.inner.risk.write().await;
                let _ = risk.update_pnl(pnl).await;
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
                // Cancel any dangling SL/TP plan orders left on MEXC.
                if !paper {
                    let live = self.inner.live.lock().await;
                    if live.is_live() {
                        let _ = live.client().cancel_all_plan_orders(&symbol).await;
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
                reconcile_on_boot(client, &self.db, &self.config).await;
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
                ticker_inner.process_tickers(batch).await;
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
            if self.config.learning.enabled {
                let learn_inner = self.clone();
                let learn_task = tokio::spawn(async move {
                    learn_inner.learning_loop().await;
                });
                guard.push(learn_task);
                info!("Continuous learning loop started (every {LEARNING_INTERVAL_SEC}s)");
            }
        }

        let _ = self.db.log_event("scanner", "Scanner started", None).await;
        info!(
            "Scanner streams live — tracking {} symbols",
            self.tracked_symbols.read().await.len()
        );
    }

    async fn kline_refresh_loop(&self) {
        let interval = self.config.scanner.kline_interval.clone();
        let lookback = self.config.scanner.kline_lookback_bars;
        while self.running.load(Ordering::SeqCst) {
            let symbols = self.tracked_symbols.read().await.clone();
            for symbol in symbols {
                if !self.running.load(Ordering::SeqCst) {
                    break;
                }
                match self.exchange.get_klines(&symbol, &interval).await {
                    Ok(mut bars) => {
                        if bars.len() > lookback as usize {
                            bars = bars[bars.len() - lookback as usize..].to_vec();
                        }
                        let mut states = self.states.write().await;
                        let state = states
                            .entry(symbol.clone())
                            .or_insert_with(|| SymbolState::new(symbol));
                        state.update_klines(bars);
                    }
                    Err(exc) => debug!("Kline refresh {symbol} skipped: {exc}"),
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(
                self.config.scanner.kline_refresh_sec,
            ))
            .await;
        }
    }

    /// Background loop: resolve closed-trade outcomes and train the online model.
    async fn learning_loop(&self) {
        // Cold start only: replay resolved history into a brand-new (empty) model.
        // A persisted model already carries that learning, so we don't double-count
        // on restart — new trades flow in through the loop below.
        if self.ml.lock().await.online_sample_count() == 0 {
            self.bootstrap_online_model().await;
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
            // Also learn from signals that never became trades by replaying them
            // against the price action that followed. Multiplies training data.
            if self.config.learning.enabled {
                let _ = self.resolve_pending_signals_from_price(SIGNAL_RESOLVE_BATCH).await;
            }
        }
    }

    /// Shadow-resolve pending signals (executed or not) against real price action
    /// so the model learns from *every* setup, not only the few that opened a
    /// position. For each pending signal whose hold window has fully elapsed, we
    /// fetch the klines covering that window and label it win / loss / expired.
    async fn resolve_pending_signals_from_price(&self, max_signals: u32) -> u32 {
        let max_hold = self.config.confluence.max_hold_sec.max(60) as i64;
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
                        let _ = self.db.set_signal_features(sig_id, &features).await;
                        backfilled += 1;
                    }
                }
            }
            if !features.iter().any(|&v| v != 0.0) {
                continue;
            }

            let Some((label, won)) = resolve_signal_outcome(&bars, start, side_long, sl, tp) else {
                continue;
            };
            let weight = shadow_resolve_weight(&self.config.learning, sig);
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
                let before_ks = risk.metrics_json(0, 0, 0, false)["kill_switch"]
                    .as_bool()
                    .unwrap_or(false);
                let _ = risk.record_trade_outcome(&symbol, won).await;
                let after = risk.metrics_json(0, 0, 0, false);
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
                let weight = self.config.ml.trade_outcome_weight(won);
                self.ml
                    .lock()
                    .await
                    .record_outcome_weighted(&features, won, weight);
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
        let threshold_pct = self.config.ml.supervised_threshold * 100.0;
        let hard_gate = self.config.ml.hard_ml_gate;
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
                self.config
                    .ml
                    .trade_outcome_weight(outcome == "win")
            } else {
                shadow_resolve_weight(&self.config.learning, sig)
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
        let learning = &self.config.learning;
        if !learning.enabled {
            return;
        }
        match reject_reason {
            "ml_gate" if !learning.shadow_ml_rejects => return,
            "confluence_near_miss" if !learning.shadow_near_miss => return,
            _ => {}
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

        // Dedupe: one shadow per symbol+side within confluence cooldown window.
        let side_long = signal.price_change_pct > 0.0;
        let cooldown = self.config.confluence.alert_cooldown_sec as i64;
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

    async fn process_tickers(&self, tickers: Vec<TickerSnapshot>) {
        // Stamp the last time we received live ticker data for WS-staleness detection.
        self.last_tick_at.store(Utc::now().timestamp(), Ordering::Relaxed);

        let tracked: Vec<String> = self.tracked_symbols.read().await.clone();
        let mode = self.config.trading.mode.clone();
        let conf_on = matches!(mode.as_str(), "confluence" | "all") && self.config.confluence.enabled;

        for ticker in tickers {
            self.ticker_map
                .write()
                .await
                .insert(ticker.symbol.clone(), ticker.clone());
            if !tracked.contains(&ticker.symbol) {
                continue;
            }

            let symbol = ticker.symbol.clone();
            let mut states = self.states.write().await;
            let state = states
                .entry(symbol.clone())
                .or_insert_with(|| SymbolState::new(symbol.clone()));
            state.update_ticker(&ticker);
            state.last_scanned_at = Some(Utc::now());

            if !conf_on {
                drop(states);
                continue;
            }

            if self.confluence.in_cooldown(state) {
                drop(states);
                self.maybe_record_scan(
                    &symbol,
                    "cooldown",
                    "On alert cooldown — skipping re-analysis",
                    None,
                    None,
                    None,
                    &ticker,
                )
                .await;
                continue;
            }

            if state.klines.len() < 15 || state.prices.len() < 8 {
                let klines_n = state.klines.len();
                let ticks_n = state.prices.len();
                drop(states);
                self.maybe_record_scan(
                    &symbol,
                    "warming",
                    &format!("Collecting data — klines {klines_n}/15, ticks {ticks_n}/8"),
                    None,
                    None,
                    None,
                    &ticker,
                )
                .await;
                continue;
            }

            let evaluated = self.confluence.evaluate(state, None, false);
            let near_miss = if evaluated.is_none() && self.config.learning.shadow_near_miss {
                self.confluence.evaluate_near_miss(
                    state,
                    None,
                    false,
                    self.config.learning.near_miss_margin,
                )
            } else {
                None
            };
            let klines = state.klines.clone();
            drop(states);

            if let Some(signal) = evaluated {
                let mut ml = self.ml.lock().await;
                match ml.enhance_signal_outcome(signal, Some(&klines)) {
                    EnhanceOutcome::Tradable(enhanced) => {
                        drop(ml);
                        self.emit_signal(enhanced).await;
                        let mut states = self.states.write().await;
                        if let Some(s) = states.get_mut(&symbol) {
                            s.last_confluence_at = Some(Utc::now());
                        }
                    }
                    EnhanceOutcome::MlRejected(rejected) => {
                        drop(ml);
                        self.save_shadow_signal(rejected, "ml_gate").await;
                    }
                }
                continue;
            }

            if let Some(near) = near_miss {
                let mut ml = self.ml.lock().await;
                let enriched = ml.attach_features(near, Some(&klines));
                drop(ml);
                self.save_shadow_signal(enriched, "confluence_near_miss")
                    .await;
                continue;
            }

            let diagnosis = {
                let states = self.states.read().await;
                states
                    .get(&symbol)
                    .map(|s| self.confluence.diagnose(s, None, false))
                    .unwrap_or(ScanDiagnosis {
                        action: "skipped".into(),
                        message: "State unavailable".into(),
                        composite_score: None,
                        confluence_count: None,
                        side: None,
                    })
            };
            self.maybe_record_scan(
                &symbol,
                &diagnosis.action,
                &diagnosis.message,
                diagnosis.composite_score,
                diagnosis.confluence_count,
                diagnosis.side.as_deref(),
                &ticker,
            )
            .await;
        }

        let map = self.ticker_map.read().await.clone();
        let mut risk = self.risk.write().await;
        let _ = self.paper.lock().await.monitor_positions(&map, &mut risk).await;
        let _ = self.live_monitor.lock().await.monitor(&map, &mut risk).await;
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
        let _ = self
            .db
            .log_event(
                "signal",
                &format!(
                    "Confluence signal: {} score={:.1}",
                    signal.symbol, signal.composite_score
                ),
                Some(payload),
            )
            .await;

        // Gate execution when the WS feed is stale — prices may be stale too.
        let stale_threshold = self.config.risk.ws_stale_sec as i64;
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

        let mut risk = self.risk.write().await;
        let live = self.live.lock().await;
        match live.open_from_signal(&signal, &mut risk).await {
            Ok(Some(pos_id)) => {
                let mode = if live.is_live() { "live" } else { "paper" };
                info!("{mode} position opened id={pos_id} {}", signal.symbol);
                let side_str = if signal.price_change_pct >= 0.0 {
                    "LONG"
                } else {
                    "SHORT"
                };
                let contract_size = live.contract_size(&signal.symbol);
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
