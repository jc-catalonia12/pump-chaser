//! Live position monitor — polls open live positions on every WS ticker batch
//! and closes them at MEXC when stop-loss or take-profit levels are hit.
//!
//! This is the Rust port of `pump_chaser/execution/live_monitor.py`.
//!
//! **Design**
//! - SL check: price crosses the stored `stop_loss` field (adjusted after each
//!   TP hit to entry — "free ride" after TP1).
//! - TP check: iterates `take_profit_levels` JSON array and partially closes
//!   each level when triggered.  After TP1 the SL is moved to entry.
//! - Trailing stop: when `trailing_stop_pct` is set (> 0) and the position has
//!   moved more than `trailing_activation_pct` in-profit, the SL is ratcheted
//!   up (long) or down (short) on every tick.
//! - Failures are written to the audit log.  Plan orders on MEXC are *also*
//!   retried once if they failed at placement time.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::SharedAppConfig;
use crate::db::Database;
use crate::execution::cleanup_after_position_closed;
use crate::execution::trailing::{TrailingTrack, update_trailing};
use crate::exchange::{ContractInfo, MexcPrivateClient, TickerSnapshot};
use crate::models::PositionSide;
use crate::risk::RiskManager;
use crate::utils::{Alerter, UserSecrets};

pub struct LivePositionMonitor {
    config: SharedAppConfig,
    db: Arc<Database>,
    client: MexcPrivateClient,
    alerter: Option<Arc<Alerter>>,
    /// Per-position adaptive trailing state. Key = position id.
    trailing_stops: HashMap<i64, TrailingTrack>,
    /// Contract metadata (size, fee rate) keyed by symbol — kept in sync with LiveTrader.
    contracts: HashMap<String, ContractInfo>,
}

impl LivePositionMonitor {
    pub fn new(config: SharedAppConfig, db: Arc<Database>, client: MexcPrivateClient) -> Self {
        Self {
            config,
            db,
            client,
            alerter: None,
            trailing_stops: HashMap::new(),
            contracts: HashMap::new(),
        }
    }

    pub fn with_alerter(mut self, alerter: Arc<Alerter>) -> Self {
        self.alerter = Some(alerter);
        self
    }

    /// Update the stored client when credentials change.
    pub fn update_client(&mut self, client: MexcPrivateClient) {
        self.client = client;
    }

    /// Rebuild REST client after MEXC endpoint change (same API keys).
    pub fn refresh_exchange_client_from_secrets(&mut self, secrets: &UserSecrets) {
        let mexc = self.config.read().unwrap().mexc.clone();
        self.client = MexcPrivateClient::from_secrets(&mexc, secrets);
    }

    /// Keep contract metadata in sync with LiveTrader after symbol discovery.
    pub fn update_contracts(&mut self, contracts: Vec<ContractInfo>) {
        self.contracts = contracts.into_iter().map(|c| (c.symbol.clone(), c)).collect();
    }

    fn contract_size(&self, symbol: &str) -> f64 {
        self.contracts
            .get(symbol)
            .map(|c| c.contract_size)
            .filter(|&s| s > 0.0)
            .unwrap_or(1.0)
    }

    fn fee_rate(&self, symbol: &str) -> f64 {
        self.contracts
            .get(symbol)
            .map(|c| c.taker_fee_rate)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0006)
    }

    /// Cancel all open plan + stop orders for a symbol on MEXC.
    async fn cleanup_symbol_orders(&self, symbol: &str, exchange_pos_id: Option<i64>) {
        cleanup_after_position_closed(&self.client, symbol, exchange_pos_id).await;
    }

    async fn position_on_exchange(&self, symbol: &str, side: &str) -> Option<bool> {
        if !self.client.has_credentials() {
            return None;
        }
        let raw = self.client.get_open_positions().await.ok()?;
        let want_long = side == "long";
        let found = raw.iter().any(|p| {
            if p.get("symbol").and_then(|v| v.as_str()) != Some(symbol) {
                return false;
            }
            let hold = p.get("holdVol").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if hold <= 0.0 {
                return false;
            }
            let is_long = p.get("positionType").and_then(|v| v.as_i64()) == Some(1);
            is_long == want_long
        });
        Some(found)
    }

    /// Called on every WS ticker batch.  Returns a list of close events.
    pub async fn monitor(
        &mut self,
        tickers: &HashMap<String, TickerSnapshot>,
        risk: &mut RiskManager,
    ) -> Vec<Value> {
        if !self.client.has_credentials() {
            return vec![];
        }

        let cfg = self.config.read().unwrap().clone();
        let positions = self.db.get_open_positions().await.unwrap_or_default();
        let mut events = Vec::new();

        for pos in positions {
            // Only handle live (non-paper) positions.
            if pos.get("paper").and_then(|v| v.as_bool()).unwrap_or(true) {
                continue;
            }

            let id = pos.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let symbol = match pos.get("symbol").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let Some(ticker) = tickers.get(&symbol) else {
                continue;
            };
            let price = ticker.last_price;
            let side = pos.get("side").and_then(|v| v.as_str()).unwrap_or("long").to_string();
            let entry = pos.get("entry_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let mut sl = pos.get("stop_loss").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let remaining = pos
                .get("remaining_size")
                .and_then(|v| v.as_f64())
                .filter(|&s| s > 0.0)
                .unwrap_or_else(|| pos.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0));
            let _leverage = pos.get("leverage").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let exchange_pos_id = pos.get("exchange_position_id").and_then(|v| v.as_i64());
            let cs = self.contract_size(&symbol);
            let fee_rate = self.fee_rate(&symbol);

            let strategy = pos
                .get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or("ai")
                .to_string();

            // ── Adaptive trailing stop (widens on large pumps/dumps) ───────
            if entry > 0.0 {
                let prev_sl = sl;
                let track = self
                    .trailing_stops
                    .entry(id)
                    .or_insert_with(|| TrailingTrack::seed(sl));
                if track.stop <= 0.0 && sl > 0.0 {
                    track.stop = sl;
                }
                sl = update_trailing(&side, entry, price, track, &cfg.risk);
                if (sl - prev_sl).abs() > 1e-10 {
                    let _ = self.db.update_position_sl_tp(id, Some(sl), None).await;
                }
            }

            // ── Take-profit levels ────────────────────────────────────────
            let tp_levels: Vec<Value> = pos
                .get("take_profit_levels")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let mut closed_full = false;
            for tp in &tp_levels {
                let tp_price = tp.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let tp_hit = tp.get("hit").and_then(|v| v.as_bool()).unwrap_or(false);
                let level = tp.get("level").and_then(|v| v.as_i64()).unwrap_or(0);
                if tp_price <= 0.0 || tp_hit {
                    continue;
                }
                let triggered = if side == "long" {
                    price >= tp_price
                } else {
                    price <= tp_price
                };
                if !triggered {
                    continue;
                }

                // Calculate close volume for this TP level.
                let frac = tp.get("close_fraction").and_then(|v| v.as_f64()).unwrap_or(0.5);
                let total_size = pos.get("size").and_then(|v| v.as_f64()).unwrap_or(remaining);
                let close_vol = (total_size * frac).max(1.0);
                let is_last_tp = level as usize == tp_levels.len();
                let close_vol = if is_last_tp { remaining } else { close_vol };

                info!(
                    "TP{level} hit for {symbol} {side} @ {price:.6} (target {tp_price:.6})"
                );

                let result = self
                    .close_on_exchange(&symbol, close_vol, &side, exchange_pos_id, price)
                    .await;

                if result.get("success").and_then(|v| v.as_bool()) != Some(false) {
                    // Mark TP hit in DB levels.
                    let new_levels: Vec<Value> = tp_levels
                        .iter()
                        .map(|t| {
                            if t.get("level").and_then(|v| v.as_i64()) == Some(level) {
                                let mut t2 = t.clone();
                                if let Value::Object(ref mut m) = t2 {
                                    m.insert("hit".into(), json!(true));
                                }
                                t2
                            } else {
                                t.clone()
                            }
                        })
                        .collect();
                    let new_levels_str = serde_json::to_string(&new_levels).unwrap_or_default();
                    // Move SL to entry after first TP ("free ride").
                    if level == 1 && entry > 0.0 {
                        sl = entry;
                        self.trailing_stops
                            .insert(id, TrailingTrack { stop: entry, peak_favorable: price });
                        let _ = self.db.update_position_sl_tp(id, Some(entry), Some(&new_levels_str)).await;
                    } else {
                        let _ = self.db.update_position_sl_tp(id, None, Some(&new_levels_str)).await;
                    }

                    if is_last_tp {
                        // Fully closed via last TP — cancel any remaining SL plan orders.
                        let side_enum = if side == "long" { PositionSide::Long } else { PositionSide::Short };
                        let pnl = compute_pnl(entry, price, remaining, cs, fee_rate, &side_enum);
                        let _ = self.db.close_position(id, price, cs, fee_rate, "take_profit").await;
                        let _ = risk.update_pnl(pnl).await;
                        let _ = risk.record_trade_outcome(&symbol, pnl > 0.0).await;
                        let _ = self.db.log_event(
                            "position_closed",
                            &format!("Closed {symbol} ({side}) TP{level} @ {price:.6}"),
                            Some(json!({"position_id": id, "pnl": pnl, "reason": "take_profit"})),
                        ).await;
                        events.push(json!({"position_id": id, "symbol": symbol, "reason": "take_profit", "level": level, "pnl": pnl}));
                        self.trailing_stops.remove(&id);
                        self.cleanup_symbol_orders(&symbol, exchange_pos_id).await;
                        closed_full = true;
                        break;
                    } else {
                        // Partial close: update remaining_size in DB.
                        let new_remaining = (remaining - close_vol).max(0.0);
                        let side_enum = if side == "long" { PositionSide::Long } else { PositionSide::Short };
                        let partial_pnl = compute_pnl(entry, price, close_vol, cs, fee_rate, &side_enum);
                        let _ = self.db.partial_close_position(id, new_remaining, price, partial_pnl).await;
                        let _ = risk.update_pnl(partial_pnl).await;
                        let _ = self.db.log_event(
                            "position_partial_tp",
                            &format!("Partial TP{level} {symbol} {side} @ {price:.6} closed {close_vol:.2}"),
                            Some(json!({"position_id": id, "pnl": partial_pnl, "level": level})),
                        ).await;
                        if let Some(alerter) = &self.alerter {
                            let tp_pct = if entry > 0.0 {
                                ((tp_price - entry).abs() / entry) * 100.0
                            } else {
                                0.0
                            };
                            let details = json!({
                                "symbol": symbol,
                                "side": side.to_uppercase(),
                                "strategy": strategy,
                                "entry_price": entry,
                                "exit_price": price,
                                "pnl": partial_pnl,
                                "tp_pct": tp_pct,
                                "reason": format!("take_profit_l{level}"),
                            });
                            let msg = format!(
                                "TP{level} {} {} @ {price:.6} — PnL: {}{partial_pnl:.4} USDT",
                                side.to_uppercase(),
                                symbol,
                                if partial_pnl >= 0.0 { "+" } else { "" },
                            );
                            alerter.trade_event("tp_hit", &msg, Some(&details)).await;
                        }
                        events.push(json!({"position_id": id, "symbol": symbol, "reason": "take_profit_partial", "level": level, "pnl": partial_pnl}));
                        // Only handle one TP per tick (price may have blown past
                        // multiple levels — the next tick handles the next one).
                        break;
                    }
                } else {
                    warn!("TP{level} market close failed for {symbol}: {:?}", result.get("error"));
                    let _ = self.db.log_event(
                        "live_close_error",
                        &format!("TP{level} close failed for {symbol}"),
                        Some(result),
                    ).await;
                }
            }

            if closed_full {
                continue;
            }

            // ── Stop-loss ─────────────────────────────────────────────────
            if sl <= 0.0 {
                continue;
            }
            let hit_sl = if side == "long" {
                price <= sl
            } else {
                price >= sl
            };
            if !hit_sl {
                continue;
            }

            let reason = if (side == "long" && sl >= entry) || (side == "short" && sl <= entry) {
                "trailing_stop"
            } else {
                "stop_loss"
            };
            info!("SL hit for {symbol} {side} @ {price:.6} (sl={sl:.6}, reason={reason})");

            let result = self
                .close_on_exchange(&symbol, remaining, &side, exchange_pos_id, price)
                .await;

            if result.get("success").and_then(|v| v.as_bool()) != Some(false) {
                let side_enum = if side == "long" { PositionSide::Long } else { PositionSide::Short };
                let pnl = compute_pnl(entry, price, remaining, cs, fee_rate, &side_enum);
                let _ = self.db.close_position(id, price, cs, fee_rate, reason).await;
                let _ = risk.update_pnl(pnl).await;
                let _ = risk.record_trade_outcome(&symbol, false).await;
                let _ = self.db.log_event(
                    "position_closed",
                    &format!("Closed {symbol} ({reason}) @ {price:.6}"),
                    Some(json!({"position_id": id, "pnl": pnl, "reason": reason})),
                ).await;
                events.push(json!({"position_id": id, "symbol": symbol, "reason": reason, "pnl": pnl}));
                self.trailing_stops.remove(&id);
                self.cleanup_symbol_orders(&symbol, exchange_pos_id).await;
            } else {
                warn!("SL market close failed for {symbol}: {:?}", result.get("error"));
                let _ = self.db.log_event(
                    "live_close_error",
                    &format!("SL close failed for {symbol} ({reason})"),
                    Some(result),
                ).await;
                if self.position_on_exchange(&symbol, &side).await == Some(false) {
                    info!("Position {symbol} already closed on exchange — cleaning up orders");
                    self.cleanup_symbol_orders(&symbol, exchange_pos_id).await;
                }
            }
        }

        events
    }

    /// Submit a market close order for the full or partial position size.
    /// Returns the raw MEXC response as JSON (includes `success` field).
    async fn close_on_exchange(
        &self,
        symbol: &str,
        vol: f64,
        side: &str,
        exchange_pos_id: Option<i64>,
        mark_price: f64,
    ) -> Value {
        let close_side: i64 = if side == "long" { 4 } else { 2 };
        let mut payload = json!({
            "symbol": symbol,
            "vol": vol,
            "side": close_side,
            "type": 5,        // market
            "openType": 2,    // cross
        });
        if mark_price > 0.0 {
            payload["price"] = json!(mark_price);
        }
        if let Some(pid) = exchange_pos_id {
            payload["positionId"] = json!(pid);
        }

        if !self.client.has_credentials() {
            return json!({ "success": false, "error": "no credentials" });
        }

        match self.client.submit_order(payload).await {
            Ok(result) => result,
            Err(exc) => json!({ "success": false, "error": exc.to_string() }),
        }
    }
}

/// Net realized PnL after round-trip taker fees.
///
/// `size` is in contracts; `contract_size` converts to coin quantity.
/// For live positions MEXC charges taker fees on open + close notional.
/// Pass `fee_rate = 0.0` for paper trades.
fn compute_pnl(
    entry: f64,
    exit: f64,
    size: f64,
    contract_size: f64,
    fee_rate: f64,
    side: &PositionSide,
) -> f64 {
    let qty = size * contract_size.max(1e-12);
    let gross = match side {
        PositionSide::Long  => (exit - entry) * qty,
        PositionSide::Short => (entry - exit) * qty,
    };
    // Round-trip taker fees: open notional + close notional × fee_rate.
    let fees = (entry * qty + exit * qty) * fee_rate;
    gross - fees
}
