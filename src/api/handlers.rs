//! FastAPI parity handlers for MEXC Trading Bot.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::user_settings::{
    apply_user_settings, save_app_config, settings_file_path, settings_schema, user_settings_values,
};
use crate::AppState;

async fn cfg(state: &AppState) -> crate::config::AppConfig {
    state.config.read().await.clone()
}

async fn cfg_arc(state: &AppState) -> Arc<crate::config::AppConfig> {
    Arc::new(state.config.read().await.clone())
}

fn execution_health(secrets: &crate::utils::UserSecrets, cfg: &crate::config::AppConfig) -> Value {
    let live_trading = secrets.live_trading && cfg.execution.live_trading_enabled;
    let dry_run = cfg.execution.dry_run;
    let exchange_orders_enabled =
        live_trading && !dry_run && secrets.has_credentials();
    json!({
        "paper_trading": secrets.paper_trading,
        "live_trading": live_trading,
        "live_trading_enabled": cfg.execution.live_trading_enabled,
        "dry_run": dry_run,
        "exchange_orders_enabled": exchange_orders_enabled,
        "has_api_credentials": secrets.has_credentials(),
    })
}

pub(crate) fn stub(feature: &str) -> Value {
    json!({
        "error": format!("{feature} not yet implemented in Rust"),
        "rust_migration": true,
    })
}

pub async fn app_logo() -> Result<Response<Body>, StatusCode> {
    let path = crate::utils::app_icon_path();
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?)
}

// --- Health & scanner ---

pub async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    let status = scanner.get_status().await;
    let secrets = state.secrets.read().await;
    let cfg = state.config.read().await;
    let exchanges = json!({ "mexc": status.get("ws_connected").unwrap_or(&json!(false)) });
    let mut body = json!({
        "status": "ok",
        "name": "MEXC Trading Bot",
        "scanner_running": status.get("running"),
        "ws_connected": status.get("ws_connected"),
        "tracked_symbols": status.get("tracked_symbols"),
        "scanner": status,
        "exchanges": exchanges,
        "user_data_dir": cfg.storage.sqlite_path,
        "trading_mode": cfg.trading.mode,
        "scalp_enabled": cfg.scalp.enabled,
        "watchlist": scanner.get_watchlist_settings().await,
    });
    if let (Some(b), Some(m)) = (body.as_object_mut(), crate::version::build_metadata().as_object()) {
        b.extend(m.clone());
    }
    if let Some(exec) = body.as_object_mut() {
        if let Some(h) = execution_health(&secrets, &cfg).as_object() {
            exec.extend(h.clone());
        }
    }
    Json(body)
}

pub async fn scanner_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.get_status().await)
}

pub async fn scanner_tracked(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.get_tracked_symbols().await)
}

pub async fn scanner_start(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    match scanner.start().await {
        Ok(v) => Json(v),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

pub async fn scanner_stop(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.stop().await)
}

// --- Risk & wallet ---

pub async fn risk(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.get_risk_metrics().await)
}

#[derive(Debug, Deserialize)]
pub struct PaperQuery {
    pub paper: Option<String>,
}

pub async fn pnl_daily(State(state): State<Arc<AppState>>, Query(q): Query<PaperQuery>) -> Json<Value> {
    let paper_filter = q.paper.as_deref().map(|p| {
        let key = p.trim().to_lowercase();
        if matches!(key.as_str(), "paper" | "1" | "true") {
            Some(true)
        } else if matches!(key.as_str(), "live" | "0" | "false") {
            Some(false)
        } else {
            None
        }
    }).flatten();

    let days = state.db.get_daily_pnl_history(paper_filter).await.unwrap_or_default();
    let total_pnl: f64 = days.iter().filter_map(|d| d.get("pnl").and_then(|v| v.as_f64())).sum();
    let total_trades: i64 = days.iter().filter_map(|d| d.get("trades").and_then(|v| v.as_i64())).sum();
    let best = days.iter().max_by(|a, b| {
        a.get("pnl").and_then(|v| v.as_f64()).unwrap_or(0.0)
            .partial_cmp(&b.get("pnl").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let worst = days.iter().min_by(|a, b| {
        a.get("pnl").and_then(|v| v.as_f64()).unwrap_or(0.0)
            .partial_cmp(&b.get("pnl").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let started = state.db.get_trading_started_at().await.ok().flatten();
    Json(json!({
        "days": days,
        "summary": {
            "trading_since": started,
            "first_pnl_day": days.first().and_then(|d| d.get("day").cloned()),
            "last_pnl_day": days.last().and_then(|d| d.get("day").cloned()),
            "days_with_trades": days.len(),
            "total_trades": total_trades,
            "total_pnl": (total_pnl * 10000.0).round() / 10000.0,
            "best_day": best,
            "worst_day": worst,
            "avg_daily_pnl": if days.is_empty() { 0.0 } else { (total_pnl / days.len() as f64 * 10000.0).round() / 10000.0 },
        },
        "filter": paper_filter,
    }))
}

pub async fn reanchor_wallet(State(state): State<Arc<AppState>>) -> Json<Value> {
    let secrets = state.secrets.read().await;
    if !secrets.has_credentials() {
        return Json(json!({ "error": "MEXC API credentials not configured" }));
    }
    let live = crate::execution::LiveTrader::new(
        cfg_arc(&state).await,
        state.db.clone(),
        secrets.clone(),
    );
    match live.get_wallet_balance().await {
        Ok(balance) => {
            let wallet_equity = balance.anchor_equity();
            let mut risk = state.risk.write().await;
            match risk.sync_from_live_wallet(wallet_equity, true).await {
                Ok(reanchored) => Json(json!({
                    "ok": true,
                    "reanchored": reanchored,
                    "wallet_balance": wallet_equity,
                    "wallet_equity": balance.equity,
                    "wallet_available": balance.available,
                    "equity": risk.metrics(0).equity,
                })),
                Err(exc) => Json(json!({ "error": exc.to_string() })),
            }
        }
        Err(exc) => Json(json!({ "error": exc.to_string() })),
    }
}

pub async fn wallet(State(state): State<Arc<AppState>>) -> Json<Value> {
    let secrets = state.secrets.read().await;
    let risk = state.risk.read().await;
    let open = state.db.count_open_positions().await.unwrap_or(0);
    let m = risk.metrics(open);

    if secrets.has_credentials() {
        let live = crate::execution::LiveTrader::new(
            cfg_arc(&state).await,
            state.db.clone(),
            secrets.clone(),
        );
        if let Ok(balance) = live.get_wallet_balance().await {
            let wallet_equity = balance.anchor_equity();
            return Json(json!({
                "currency": "USDT",
                "equity": wallet_equity,
                "available": balance.available,
                "source": "live",
                "paper_trading": secrets.paper_trading,
                "live_trading": live.is_live(),
            }));
        }
    }

    Json(json!({
        "currency": "USDT",
        "equity": m.equity,
        "available": m.equity,
        "source": m.equity_source,
        "paper_trading": secrets.paper_trading,
        "live_trading": false,
    }))
}

// --- Signals ---

#[derive(Debug, Deserialize)]
pub struct SignalsQuery {
    #[serde(default = "default_signals_page_size")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_signals_page_size() -> i64 {
    25
}

pub async fn signals(State(state): State<Arc<AppState>>, Query(q): Query<SignalsQuery>) -> Json<Value> {
    let limit = q.limit.clamp(1, 100);
    let offset = q.offset.max(0);
    let total = state.db.count_signals().await.unwrap_or(0);
    let signals = state
        .db
        .get_signals_paged(limit, offset)
        .await
        .unwrap_or_default();
    let total_pages = if total == 0 {
        1
    } else {
        (total + limit - 1) / limit
    };
    let page = if limit > 0 { offset / limit + 1 } else { 1 };
    Json(json!({
        "signals": signals,
        "total": total,
        "limit": limit,
        "offset": offset,
        "page": page,
        "total_pages": total_pages,
    }))
}

#[derive(Debug, Deserialize)]
pub struct SignalHistoryQuery {
    pub symbol: String,
    #[serde(default = "default_hist_limit")]
    pub limit: i64,
    #[serde(default = "default_true_str")]
    pub resolve: String,
}

fn default_hist_limit() -> i64 {
    20
}

fn default_true_str() -> String {
    "true".into()
}

pub async fn signals_history(State(state): State<Arc<AppState>>, Query(q): Query<SignalHistoryQuery>) -> Json<Value> {
    let history = state
        .db
        .get_signals_for_symbol(&q.symbol, q.limit)
        .await
        .unwrap_or_default();
    Json(json!({ "symbol": q.symbol, "history": history }))
}

pub async fn signals_stats(State(state): State<Arc<AppState>>) -> Json<Value> {
    let strategies = state.db.get_strategy_outcome_stats().await.unwrap_or_default();
    let pending = state.db.get_pending_signals(500).await.unwrap_or_default();
    Json(json!({ "strategies": strategies, "pending": pending.len() }))
}

pub async fn signals_chart(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SignalChartQuery>,
) -> Json<Value> {
    let symbol = q.symbol.trim();
    if symbol.is_empty() {
        return Json(json!({ "error": "symbol required" }));
    }

    let mut signal: Option<Value> = None;
    if let Some(ref at) = q.generated_at {
        let history = state
            .db
            .get_signals_for_symbol(symbol, 50)
            .await
            .unwrap_or_default();
        signal = history.into_iter().find(|s| {
            s.get("generated_at").and_then(|v| v.as_str()) == Some(at.as_str())
                || s.get("created_at").and_then(|v| v.as_str()) == Some(at.as_str())
        });
    }
    if signal.is_none() {
        let recent = state.db.get_recent_signals(100).await.unwrap_or_default();
        signal = recent.into_iter().find(|s| s.get("symbol").and_then(|v| v.as_str()) == Some(symbol));
    }
    let Some(signal) = signal else {
        return Json(json!({ "error": "Signal not found" }));
    };

    let cfg = cfg(&state).await;
    let interval = cfg.scanner.kline_interval.clone();
    let bars_n = q.bars.clamp(30, 500) as usize;
    let scanner = state.scanner.read().await;
    let cached = scanner.get_symbol_klines(symbol).await;
    let exchange = match crate::exchange::MexcClient::new(Arc::new(cfg.clone())) {
        Ok(c) => c,
        Err(e) => return Json(json!({ "error": e.to_string() })),
    };

    let signal_id = signal.get("id").and_then(|v| v.as_i64());
    let position = if let Some(sid) = signal_id {
        state.db.get_position_by_signal_id(sid).await.ok().flatten()
    } else {
        None
    };

    // Anchor klines from before the signal so entry/exit markers stay visible.
    let lookback_sec = 30 * 60_i64;
    let start_ts = crate::charts::parse_signal_start_ts(&signal).unwrap_or(0) - lookback_sec;
    let kline_bars = if start_ts > 0 {
        crate::charts::load_chart_bars_from_time(
            &exchange,
            &cached,
            symbol,
            &interval,
            start_ts,
            bars_n,
        )
        .await
    } else {
        crate::charts::load_chart_bars(&exchange, &cached, symbol, &interval, bars_n).await
    };
    let zones = crate::charts::build_chart_zones(&kline_bars, &cfg);

    let mut trade = crate::charts::build_trade_overlay(Some(&signal), position.as_ref());

    // Resolved signal without a closed position: infer exit marker from price action.
    let pos_closed = position
        .as_ref()
        .and_then(|p| p.get("status").and_then(|v| v.as_str()))
        == Some("closed");
    if !pos_closed {
        let outcome = signal
            .get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        if matches!(outcome, "win" | "loss" | "expired") {
            if let Some(start) = crate::charts::parse_signal_start_ts(&signal) {
                let entry = signal.get("last_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let sl = signal
                    .get("projected_stop_loss")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let tp = signal
                    .get("projected_take_profits")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let side_long = signal
                    .get("price_change_pct")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0)
                    > 0.0;
                let max_hold = cfg.confluence.max_hold_sec as i64;
                if entry > 0.0 && sl > 0.0 && tp > 0.0 {
                    if let Some(exit) = crate::charts::resolve_signal_exit_from_bars(
                        &kline_bars,
                        start,
                        side_long,
                        sl,
                        tp,
                        outcome,
                        max_hold,
                    ) {
                        if let Some(obj) = trade.as_object_mut() {
                            for (k, v) in exit.as_object().unwrap_or(&serde_json::Map::new()) {
                                obj.insert(k.clone(), v.clone());
                            }
                            if let Some(ts) = exit.get("exit_timestamp").and_then(|v| v.as_i64()) {
                                let closed_at = chrono::DateTime::from_timestamp(ts, 0)
                                    .map(|dt| dt.to_rfc3339())
                                    .unwrap_or_default();
                                obj.insert("closed_at".into(), json!(closed_at));
                            }
                            obj.insert("outcome".into(), json!(outcome));
                        }
                    }
                }
            }
        }
    }

    Json(json!({
        "signal": signal,
        "trade": trade,
        "bars": crate::charts::bars_to_chart_payload(&kline_bars),
        "zones": zones,
        "interval": interval,
        "data_source": crate::charts::DATA_SOURCE,
        "tv_symbol": crate::charts::mexc_to_tradingview_symbol(symbol),
    }))
}

#[derive(Debug, Deserialize)]
pub struct SignalChartQuery {
    pub symbol: String,
    pub generated_at: Option<String>,
    #[serde(default = "default_chart_bars")]
    pub bars: i64,
}

fn default_chart_bars() -> i64 {
    120
}

#[derive(Debug, Deserialize)]
pub struct PositionChartQuery {
    pub interval: Option<String>,
    #[serde(default = "default_chart_bars")]
    pub bars: i64,
}

// --- Positions ---

#[derive(Deserialize)]
pub struct PositionsHistoryQuery {
    #[serde(default = "default_history_limit")]
    pub limit: i64,
    /// `all` (default), `live`, or `paper`
    pub paper: Option<String>,
}

fn default_history_limit() -> i64 {
    100
}

fn parse_positions_paper_filter(raw: Option<&str>) -> Option<bool> {
    match raw.map(|s| s.trim().to_lowercase()).as_deref() {
        Some("live") | Some("false") | Some("0") => Some(false),
        Some("paper") | Some("true") | Some("1") => Some(true),
        _ => None,
    }
}

pub async fn positions(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    let positions = scanner.get_open_positions_live().await;
    let count = positions.len();
    Json(json!({ "positions": positions, "count": count }))
}

pub async fn positions_history(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PositionsHistoryQuery>,
) -> Json<Value> {
    let limit = q.limit.clamp(1, 500);
    let paper = parse_positions_paper_filter(q.paper.as_deref());
    let positions = state
        .db
        .get_closed_positions(limit, None, paper)
        .await
        .unwrap_or_default();
    let total = state
        .db
        .count_closed_positions(None, paper)
        .await
        .unwrap_or(0);
    let filter = match paper {
        Some(true) => "paper",
        Some(false) => "live",
        None => "all",
    };
    Json(json!({
        "positions": positions,
        "count": positions.len(),
        "total": total,
        "limit": limit,
        "filter": filter,
    }))
}

pub async fn positions_sync(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.sync_exchange_positions().await)
}

pub async fn positions_close(
    State(state): State<Arc<AppState>>,
    Path(position_id): Path<i64>,
) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.close_position_manual(position_id).await)
}

pub async fn position_chart(
    State(state): State<Arc<AppState>>,
    Path(position_id): Path<i64>,
    Query(q): Query<PositionChartQuery>,
) -> Json<Value> {
    let Some(db_pos) = state
        .db
        .get_position_by_id(position_id)
        .await
        .ok()
        .flatten()
    else {
        return Json(json!({ "error": "Position not found" }));
    };

    let symbol = db_pos
        .get("symbol")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let cfg = cfg(&state).await;
    let interval = q
        .interval
        .clone()
        .unwrap_or_else(|| cfg.scanner.kline_interval.clone());
    let bars_n = q.bars.clamp(30, 500) as usize;

    let scanner = state.scanner.read().await;
    // Prefer live-enriched position (mark, unrealized PnL); fall back to DB row.
    let position = scanner
        .get_position_live(position_id)
        .await
        .unwrap_or(db_pos);

    let signal = if let Some(sig_id) = position.get("signal_id").and_then(|v| v.as_i64()) {
        state.db.get_signal_by_id(sig_id).await.ok().flatten()
    } else {
        None
    };

    let cached = scanner.get_symbol_klines(&symbol).await;
    let exchange = match crate::exchange::MexcClient::new(Arc::new(cfg.clone())) {
        Ok(c) => c,
        Err(e) => return Json(json!({ "error": e.to_string() })),
    };
    let kline_bars =
        crate::charts::load_chart_bars(&exchange, &cached, &symbol, &interval, bars_n).await;
    let zones = crate::charts::build_chart_zones(&kline_bars, &cfg);
    let trade = crate::charts::build_trade_overlay(signal.as_ref(), Some(&position));

    let is_open = position
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s == "open" || s == "partial")
        .unwrap_or(true);

    Json(json!({
        "position": position,
        "signal": signal,
        "trade": trade,
        "overlay_options": { "hide_zones": is_open },
        "bars": crate::charts::bars_to_chart_payload(&kline_bars),
        "zones": zones,
        "interval": interval,
        "data_source": crate::charts::DATA_SOURCE,
        "tv_symbol": crate::charts::mexc_to_tradingview_symbol(&symbol),
    }))
}

// --- Activity & learning ---

#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

pub async fn activity(State(state): State<Arc<AppState>>, Query(q): Query<LimitQuery>) -> Json<Value> {
    let events = state.db.get_trade_activity(q.limit).await.unwrap_or_default();
    let unread_count = state.db.count_unread_activity().await.unwrap_or(0);
    Json(json!({ "events": events, "unread_count": unread_count }))
}

#[derive(Debug, Deserialize)]
pub struct ActivitySeenBody {
    #[serde(default)]
    pub ids: Vec<i64>,
    #[serde(default)]
    pub all: bool,
}

pub async fn activity_seen(State(state): State<Arc<AppState>>, Json(body): Json<ActivitySeenBody>) -> Json<Value> {
    let ids = if body.ids.is_empty() { None } else { Some(body.ids.as_slice()) };
    let updated = state
        .db
        .mark_activity_seen(ids, body.all)
        .await
        .unwrap_or(0);
    let unread_count = state.db.count_unread_activity().await.unwrap_or(0);
    Json(json!({
        "updated": updated,
        "unread_count": unread_count,
    }))
}

pub async fn learning_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.get_learning_status().await)
}

pub async fn learning_reload(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.reload_learning_params().await)
}

pub async fn audit(State(state): State<Arc<AppState>>, Query(q): Query<LimitQuery>) -> Json<Value> {
    let events = state.db.get_audit_log(q.limit).await.unwrap_or_default();
    Json(json!({ "events": events }))
}

#[derive(Debug, Deserialize)]
pub struct OptimizationQuery {
    pub symbol: Option<String>,
    #[serde(default = "default_opt_limit")]
    pub limit: i64,
}

fn default_opt_limit() -> i64 {
    10
}

pub async fn optimization(State(state): State<Arc<AppState>>, Query(q): Query<OptimizationQuery>) -> Json<Value> {
    let runs = state
        .db
        .get_optimization_runs(q.symbol.as_deref(), q.limit)
        .await
        .unwrap_or_default();
    Json(json!({ "runs": runs }))
}

// --- Trading control ---

pub async fn trading_start(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    let _ = scanner.deactivate_kill_switch().await;
    let start_result = scanner.start().await;
    drop(scanner);

    // Sync exchange positions in the background — it's a network round-trip and
    // must not delay the Start button's response.
    let scanner_arc = state.scanner.clone();
    tokio::spawn(async move {
        let s = scanner_arc.read().await;
        let _ = s.sync_exchange_positions().await;
    });

    match start_result {
        Ok(result) => {
            let steps = vec![
                "Trading resumed (kill switch off)".to_string(),
                format!(
                    "Scanner {}",
                    result.get("status").and_then(|v| v.as_str()).unwrap_or("started")
                ),
                "Discovering symbols & syncing MEXC in background…".to_string(),
            ];
            Json(json!({
                "status": "ready",
                "steps": steps,
                "equity": state.risk.read().await.metrics(0).equity,
                "scanner_running": result.get("running"),
                "tracked_symbols": result.get("tracked_symbols"),
            }))
        }
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

pub async fn trading_stop(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.stop().await)
}

pub async fn kill_switch_activate(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.activate_kill_switch().await)
}

pub async fn kill_switch_deactivate(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.deactivate_kill_switch().await)
}

// --- Settings ---

pub async fn user_settings_get(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state.config.read().await;
    Json(json!({
        "config_path": settings_file_path().display().to_string(),
        "values": user_settings_values(&cfg),
        "sections": settings_schema(),
        "note": "Changes are saved to settings.yaml. Stop and start the scanner for strategy thresholds to fully apply.",
    }))
}

pub async fn user_settings_put(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let patch = body.get("values").cloned().unwrap_or(body);
    let mut updated = {
        let cfg = state.config.read().await;
        cfg.clone()
    };
    if let Err(exc) = apply_user_settings(&mut updated, &patch) {
        return Json(json!({ "error": exc.to_string() }));
    }
    if let Err(exc) = save_app_config(&updated) {
        return Json(json!({ "error": exc.to_string() }));
    }
    {
        let mut cfg = state.config.write().await;
        *cfg = updated.clone();
    }
    Json(json!({
        "ok": true,
        "config_path": settings_file_path().display().to_string(),
        "values": user_settings_values(&updated),
        "live_trading_enabled": updated.execution.live_trading_enabled,
        "scanner_restart_recommended": true,
    }))
}

pub async fn trading_settings_get(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.get_trading_settings())
}

pub async fn trading_settings_put(Json(body): Json<Value>) -> Json<Value> {
    Json(json!({
        "error": "Runtime trading settings update not persisted yet",
        "requested": body,
        "rust_migration": true,
    }))
}

pub async fn watchlist_settings_get(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.get_watchlist_settings().await)
}

pub async fn watchlist_settings_put(Json(body): Json<Value>) -> Json<Value> {
    Json(json!({
        "error": "Watchlist settings update not persisted yet",
        "requested": body,
        "rust_migration": true,
    }))
}

// --- ML ---

pub async fn ml_stack(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state.config.read().await;
    #[cfg(feature = "onnx")]
    let onnx_path = crate::ml::onnx::resolve_model_path(&cfg.ml);
    #[cfg(feature = "onnx")]
    let onnx_loaded = onnx_path.exists();
    #[cfg(not(feature = "onnx"))]
    let onnx_loaded = false;

    let model = {
        let scanner = state.scanner.read().await;
        scanner.ml_learning_status().await
    };

    Json(json!({
        "config": cfg.ml,
        "backtest_engine": cfg.backtest.engine,
        "mexc_client": cfg.exchanges.mexc_client,
        "installed": { "xgboost": false, "onnx": onnx_loaded },
        "model": model,
        "runtime": {
            "inference": "native_online + onnx_fallback",
            "hard_ml_gate": cfg.ml.hard_ml_gate,
            "onnx_loaded": onnx_loaded,
        },
    }))
}

pub async fn ml_outcome_trends(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state.config.read().await;
    Json(json!({
        "daily": [],
        "ml_buckets": [],
        "training_runs": [],
        "supervised_threshold": cfg.ml.supervised_threshold,
        "rust_migration": true,
    }))
}

pub async fn ml_train(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.train_online_from_db().await)
}

/// Resolve pending signals (including ones that never opened a position) against
/// the price action that followed, and train the online model on the outcomes.
pub async fn ml_resolve_signals(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.resolve_signals_from_price(500).await)
}

pub async fn ml_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.ml_learning_status().await)
}

/// Combined model statistics + learning history for the Training screen.
pub async fn ml_history(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = cfg(&state).await;
    #[cfg(feature = "onnx")]
    let onnx_loaded = crate::ml::onnx::resolve_model_path(&cfg.ml).exists();
    #[cfg(not(feature = "onnx"))]
    let onnx_loaded = false;

    let model = {
        let scanner = state.scanner.read().await;
        scanner.ml_learning_status().await
    };
    let history = state
        .db
        .get_model_learn_history(1000)
        .await
        .unwrap_or_default();
    let signal_outcomes = state
        .db
        .get_signal_outcome_counts()
        .await
        .unwrap_or_else(|_| json!({}));

    let threshold_pct = cfg.ml.supervised_threshold * 100.0;
    let postgate_stats = state
        .db
        .get_postgatewin_stats(threshold_pct)
        .await
        .unwrap_or_else(|_| json!({}));
    let side_stats = state
        .db
        .get_side_outcome_stats()
        .await
        .unwrap_or_else(|_| json!({}));
    let rolling_7d = state
        .db
        .get_rolling_win_rate(7)
        .await
        .unwrap_or_default();
    let shadow_stats = state
        .db
        .get_shadow_signal_stats()
        .await
        .unwrap_or_else(|_| json!({}));
    let shadow_recent = state
        .db
        .get_recent_shadow_signals(50)
        .await
        .unwrap_or_default();

    Json(json!({
        "model": model,
        "history": history,
        "signal_outcomes": signal_outcomes,
        "postgate_stats": postgate_stats,
        "side_stats": side_stats,
        "rolling_7d": rolling_7d,
        "shadow_stats": shadow_stats,
        "shadow_recent": shadow_recent,
        "onnx": {
            "loaded": onnx_loaded,
            "path": cfg.ml.onnx_model_path,
            "type": "gradient_boosting",
            "feature_dim": crate::ml::features::FEATURE_DIM,
        },
        "config": {
            "enabled": cfg.ml.enabled,
            "supervised_enabled": cfg.ml.supervised_enabled,
            "supervised_threshold": cfg.ml.supervised_threshold,
            "min_training_samples": cfg.ml.min_training_samples,
            "hard_ml_gate": cfg.ml.hard_ml_gate,
            "trade_win_weight": cfg.ml.trade_win_weight,
            "trade_loss_weight": cfg.ml.trade_loss_weight,
        },
        "learning": {
            "shadow_ml_reject_weight": cfg.learning.shadow_ml_reject_weight,
            "shadow_near_miss_weight": cfg.learning.shadow_near_miss_weight,
        },
        "feature_columns": crate::ml::features::FEATURE_COLUMNS,
    }))
}

pub async fn ml_shadow_stats(State(state): State<Arc<AppState>>) -> Json<Value> {
    let stats = state
        .db
        .get_shadow_signal_stats()
        .await
        .unwrap_or_else(|_| json!({}));
    let recent = state
        .db
        .get_recent_shadow_signals(100)
        .await
        .unwrap_or_default();
    Json(json!({ "stats": stats, "recent": recent }))
}

// --- User & build ---

pub async fn user_profile(State(state): State<Arc<AppState>>) -> Json<Value> {
    let secrets = state.secrets.read().await;
    let cfg = state.config.read().await;
    Json(json!({
        "sqlite_path": cfg.storage.sqlite_path,
        "credentials": secrets.to_public(),
        "paper_trading": secrets.paper_trading,
        "live_trading": secrets.live_trading && cfg.execution.live_trading_enabled,
        "live_trading_enabled": cfg.execution.live_trading_enabled,
        "dry_run": cfg.execution.dry_run,
        "exchange_orders_enabled": secrets.live_trading
            && cfg.execution.live_trading_enabled
            && !cfg.execution.dry_run
            && secrets.has_credentials(),
        "platform": "rust",
    }))
}

pub async fn user_credentials_put(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let mut secrets = state.secrets.write().await;
    *secrets = crate::utils::merge_secrets_update(secrets.clone(), &body);

    let mut dry_run_disabled = false;
    if body.get("execution_mode").and_then(|v| v.as_str()) == Some("live") {
        let mut cfg = state.config.write().await;
        if cfg.execution.dry_run {
            cfg.execution.dry_run = false;
            if let Err(exc) = save_app_config(&cfg) {
                return Json(json!({ "error": exc.to_string() }));
            }
            dry_run_disabled = true;
        }
    }

    if let Err(exc) = crate::utils::save_secrets(&secrets) {
        return Json(json!({ "error": exc.to_string() }));
    }
    let scanner = state.scanner.read().await;
    scanner.update_live_secrets(secrets.clone()).await;
    let cfg = state.config.read().await;
    Json(json!({
        "ok": true,
        "credentials": secrets.to_public(),
        "paper_trading": secrets.paper_trading,
        "live_trading": secrets.live_trading && cfg.execution.live_trading_enabled,
        "dry_run": cfg.execution.dry_run,
        "exchange_orders_enabled": secrets.live_trading
            && cfg.execution.live_trading_enabled
            && !cfg.execution.dry_run
            && secrets.has_credentials(),
        "dry_run_disabled": dry_run_disabled,
    }))
}

pub async fn user_credentials_delete(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut secrets = state.secrets.write().await;
    secrets.clear_credentials();
    if let Err(exc) = crate::utils::save_secrets(&secrets) {
        return Json(json!({ "error": exc.to_string() }));
    }
    let scanner = state.scanner.read().await;
    scanner.update_live_secrets(secrets.clone()).await;
    Json(json!({
        "ok": true,
        "credentials": secrets.to_public(),
        "paper_trading": secrets.paper_trading,
        "live_trading": false,
    }))
}

pub async fn build_installer_info() -> Json<Value> {
    Json(json!({ "status": "unavailable", "message": "Installer build is Python-only", "rust_migration": true }))
}

pub async fn build_installer_post(Json(_body): Json<Value>) -> Json<Value> {
    Json(stub("Installer build"))
}

pub async fn build_installer_status() -> Json<Value> {
    Json(json!({ "status": "idle" }))
}

pub async fn build_installer_download(Path(filename): Path<String>) -> Json<Value> {
    Json(json!({ "error": format!("Artifact not found: {filename}") }))
}

pub async fn build_installer_delete(Path(filename): Path<String>) -> Json<Value> {
    Json(json!({ "error": format!("Cannot delete: {filename}") }))
}

// --- Training & backtest ---

pub async fn training_info(Query(_q): Query<Value>) -> Json<Value> {
    Json(json!({
        "message": "Training is native + continuous. The online model trains automatically on every resolved trade. Use POST /ml/train to replay resolved history.",
        "native_learning": true,
    }))
}

pub async fn training_status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    let model = scanner.ml_learning_status().await;
    Json(json!({ "status": "continuous", "model": model }))
}

pub async fn training_run(State(state): State<Arc<AppState>>, Json(_body): Json<Value>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.train_online_from_db().await)
}

pub async fn training_run_sync(State(state): State<Arc<AppState>>, Json(_body): Json<Value>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    Json(scanner.train_online_from_db().await)
}

pub async fn backtest(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let cfg = cfg(&state).await;
    let default_threshold = cfg.ml.supervised_threshold * 100.0;
    let ml_threshold = body
        .get("ml_threshold")
        .and_then(|v| v.as_f64())
        .unwrap_or(default_threshold);
    let fee_pct = body.get("fee_pct").and_then(|v| v.as_f64()).unwrap_or(0.001);
    let risk_pct = body.get("risk_pct").and_then(|v| v.as_f64()).unwrap_or(cfg.risk.max_risk_per_trade);
    let limit = body.get("limit").and_then(|v| v.as_i64()).unwrap_or(2000);

    let signals = match state.db.get_resolved_signals_with_features(limit).await {
        Ok(s) => s,
        Err(e) => return Json(json!({ "error": e.to_string() })),
    };
    Json(crate::backtest::Backtester::new().run_json(&signals, ml_threshold, fee_pct, risk_pct))
}

pub async fn walk_forward(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let train_frac = body.get("train_frac").and_then(|v| v.as_f64()).unwrap_or(0.8);
    let cfg = cfg(&state).await;
    let onnx_path = cfg.ml.onnx_model_path.clone();
    let limit = body.get("limit").and_then(|v| v.as_i64()).unwrap_or(2000);

    let signals = match state.db.get_resolved_signals_with_features(limit).await {
        Ok(s) => s,
        Err(e) => return Json(json!({ "error": e.to_string() })),
    };
    Json(crate::backtest::StrategyLearner::new().walk_forward_json(
        &signals,
        train_frac,
        onnx_path.as_deref(),
    ))
}

pub async fn historical_fetch(State(state): State<Arc<AppState>>, Json(body): Json<Value>) -> Json<Value> {
    let symbol = body.get("symbol").and_then(|v| v.as_str()).unwrap_or("BTC_USDT");
    let interval = body
        .get("interval")
        .and_then(|v| v.as_str())
        .unwrap_or("Min1");
    let lookback = body.get("lookback_bars").and_then(|v| v.as_u64()).unwrap_or(500) as u32;
    let client = match crate::exchange::MexcClient::new(cfg_arc(&state).await) {
        Ok(c) => c,
        Err(e) => return Json(json!({ "error": e.to_string() })),
    };
    match client.get_klines(symbol, interval).await {
        Ok(bars) => Json(json!({
            "symbol": symbol,
            "interval": interval,
            "bars_cached": bars.len().min(lookback as usize),
        })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

// --- Exchanges & live ---

pub async fn exchanges_health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let client = match crate::exchange::MexcClient::new(cfg_arc(&state).await) {
        Ok(c) => c,
        Err(e) => return Json(json!({ "exchanges": { "mexc": false, "error": e.to_string() } })),
    };
    let ok = client.ping().await;
    Json(json!({ "exchanges": { "mexc": ok } }))
}

pub async fn live_snapshot(State(state): State<Arc<AppState>>) -> Json<Value> {
    let scanner = state.scanner.read().await;
    let risk = scanner.get_risk_metrics().await;
    let secrets = state.secrets.read().await;
    let cfg = state.config.read().await;
    let risk_guard = state.risk.read().await;
    let open = state.db.count_open_positions().await.unwrap_or(0);
    let m = risk_guard.metrics(open);
    let mut wallet = json!({
        "currency": "USDT",
        "equity": m.equity,
        "available": m.equity,
        "source": m.equity_source,
        "paper_trading": secrets.paper_trading,
        "live_trading": secrets.live_trading && cfg.execution.live_trading_enabled,
    });
    if secrets.has_credentials() {
        let live = crate::execution::LiveTrader::new(
            cfg_arc(&state).await,
            state.db.clone(),
            secrets.clone(),
        );
        if let Ok(balance) = live.get_wallet_balance().await {
            let wallet_equity = balance.anchor_equity();
            wallet["equity"] = json!(wallet_equity);
            wallet["available"] = json!(balance.available);
            wallet["source"] = json!("live");
        }
    }
    let positions = scanner.get_open_positions_live().await;
    let activity = state.db.get_trade_activity(15).await.unwrap_or_default();
    let activity_unread = state.db.count_unread_activity().await.unwrap_or(0);
    let signals = state.db.get_recent_signals(30).await.unwrap_or_default();
    let scan_events = scanner.get_latest_scans(40).await;
    let status = scanner.get_status().await;
    let mut health = json!({
        "scanner_running": status.get("running"),
        "ws_connected": status.get("ws_connected"),
        "tracked_symbols": status.get("tracked_symbols"),
        "scans_buffered": status.get("scans_buffered"),
        "signals_buffered": status.get("signals_buffered"),
        "trading_mode": cfg.trading.mode,
    });
    if let (Some(h), Some(e)) = (health.as_object_mut(), execution_health(&secrets, &cfg).as_object()) {
        h.extend(e.clone());
    }
    if let (Some(h), Some(m)) = (health.as_object_mut(), crate::version::build_metadata().as_object()) {
        h.extend(m.clone());
    }
    Json(json!({
        "type": "snapshot",
        "ts": chrono::Utc::now().to_rfc3339(),
        "health": health,
        "risk": risk,
        "wallet": wallet,
        "positions": positions,
        "activity": activity,
        "activity_unread": activity_unread,
        "signals": signals,
        "scan_events": scan_events,
    }))
}

// ---------------------------------------------------------------------------
// Telegram notification settings
// ---------------------------------------------------------------------------

async fn refresh_telegram_bot_info(secrets: &mut crate::utils::UserSecrets) {
    if secrets.telegram_bot_token.is_empty() || !secrets.telegram_bot_username.is_empty() {
        return;
    }
    match crate::utils::Alerter::fetch_bot_info(&secrets.telegram_bot_token).await {
        Ok((username, name)) => {
            secrets.telegram_bot_username = username;
            secrets.telegram_bot_name = name;
        }
        Err(e) => {
            tracing::warn!("Telegram getMe failed: {e}");
        }
    }
}

/// GET /user/telegram — return current Telegram connection state (no secrets exposed).
pub async fn user_telegram_get(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut secrets = state.secrets.write().await;
    let before = secrets.telegram_bot_username.clone();
    refresh_telegram_bot_info(&mut secrets).await;
    if secrets.telegram_bot_username != before {
        let _ = crate::utils::save_secrets(&secrets);
    }
    Json(secrets.telegram_public())
}

/// PUT /user/telegram — save or update Telegram credentials and event filters.
///
/// Accepted body fields (all optional):
/// ```json
/// {
///   "telegram_bot_token": "123456:ABC...",
///   "telegram_chat_id": "-100...",
///   "telegram_enabled": true,
///   "telegram_events": ["position_opened","position_closed","tp_hit","cut_loss"],
///   "clear_telegram": false
/// }
/// ```
pub async fn user_telegram_put(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let token_updated = body
        .get("telegram_bot_token")
        .and_then(|v| v.as_str())
        .is_some_and(|t| !t.is_empty() && !t.starts_with("********"));
    let mut secrets = state.secrets.write().await;
    *secrets = crate::utils::merge_secrets_update(secrets.clone(), &body);
    if token_updated {
        secrets.telegram_bot_username.clear();
        secrets.telegram_bot_name.clear();
    }
    if secrets.has_telegram() {
        refresh_telegram_bot_info(&mut secrets).await;
    }
    if let Err(exc) = crate::utils::save_secrets(&secrets) {
        return Json(json!({ "error": exc.to_string() }));
    }
    Json(json!({ "ok": true, "telegram": secrets.telegram_public() }))
}

/// POST /user/telegram/test — send a test message to verify the connection.
pub async fn user_telegram_test(State(state): State<Arc<AppState>>) -> Json<Value> {
    let secrets = state.secrets.read().await;
    if !secrets.has_telegram() {
        return Json(json!({ "ok": false, "error": "No Telegram credentials saved" }));
    }
    let token = secrets.telegram_bot_token.clone();
    let chat_id = secrets.telegram_chat_id.clone();
    drop(secrets);
    match crate::utils::Alerter::test_telegram(&token, &chat_id).await {
        Ok(()) => Json(json!({ "ok": true, "message": "Test message sent successfully" })),
        Err(e) => Json(json!({ "ok": false, "error": e })),
    }
}
