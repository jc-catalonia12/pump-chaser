//! Paper trading exit engine — stop-loss, take-profit (partial), trailing stop,
//! and time-based exit for paper positions.
//!
//! Previously only SL + time-exit were handled.  This revision adds:
//!   - Per-TP-level partial closes (mirroring `paper_trader.py`).
//!   - Trailing-stop ratchet (activated after `trailing_activation_pct` move).
//!   - "Free ride" SL move to entry after the first TP is hit.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};

use crate::config::SharedAppConfig;
use crate::db::Database;
use crate::exchange::TickerSnapshot;
use crate::execution::paper_fill::paper_chunk_pnl;
use crate::execution::trailing::{TrailingTrack, update_trailing};
use crate::risk::RiskManager;

pub struct PaperTrader {
    config: SharedAppConfig,
    db: Arc<Database>,
    /// In-memory adaptive trailing stop per position id.
    trailing_stops: HashMap<i64, TrailingTrack>,
}

impl PaperTrader {
    pub fn new(config: SharedAppConfig, db: Arc<Database>) -> Self {
        Self {
            config,
            db,
            trailing_stops: HashMap::new(),
        }
    }

    pub async fn monitor_positions(
        &mut self,
        tickers: &HashMap<String, TickerSnapshot>,
        risk: &mut RiskManager,
    ) -> Vec<Value> {
        let mut events = Vec::new();
        let cfg = self.config.read().unwrap().clone();
        let fee_rate = cfg.execution.paper_fee_rate;
        let slippage = cfg.execution.paper_slippage_pct;
        let positions = self.db.get_open_positions().await.unwrap_or_default();
        for pos in positions {
            let Some(paper) = pos.get("paper").and_then(|v| v.as_bool()) else {
                continue;
            };
            if !paper {
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
            let opened = pos.get("opened_at").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let remaining = pos
                .get("remaining_size")
                .and_then(|v| v.as_f64())
                .filter(|&s| s > 0.0)
                .unwrap_or_else(|| pos.get("size").and_then(|v| v.as_f64()).unwrap_or(0.0));

            // ── Time exit ─────────────────────────────────────────────────
            if let Some(reason) = self.check_time_exit(&opened) {
                let exit = paper_chunk_pnl(entry, price, remaining, &side, slippage, fee_rate).0;
                if let Ok(pnl) = self.db.close_position(id, exit, 1.0, fee_rate, &reason).await {
                    let _ = risk.update_pnl(pnl).await;
                    let _ = risk.record_trade_outcome(&symbol, pnl > 0.0).await;
                    let _ = self
                        .db
                        .log_event("position_closed", &format!("Closed {symbol} ({reason})"), Some(json!({"pnl": pnl})))
                        .await;
                    events.push(json!({"position_id": id, "symbol": symbol, "reason": reason, "pnl": pnl}));
                    self.trailing_stops.remove(&id);
                }
                continue;
            }

            // ── Adaptive trailing stop ratchet ─────────────────────────────
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

            // ── Take-profit levels (partial closes) ───────────────────────
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

                let frac = tp.get("close_fraction").and_then(|v| v.as_f64()).unwrap_or(0.5);
                let total_size = pos.get("size").and_then(|v| v.as_f64()).unwrap_or(remaining);
                let is_last_tp = level as usize == tp_levels.len();
                let close_vol = if is_last_tp {
                    remaining
                } else {
                    (total_size * frac).min(remaining).max(0.0)
                };

                // Mark this TP as hit in the stored JSON.
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

                // After first TP: move SL to entry ("free ride").
                if level == 1 && entry > 0.0 {
                    sl = entry;
                    self.trailing_stops
                        .insert(id, TrailingTrack { stop: entry, peak_favorable: price });
                    let _ = self.db.update_position_sl_tp(id, Some(entry), Some(&new_levels_str)).await;
                } else {
                    let _ = self.db.update_position_sl_tp(id, None, Some(&new_levels_str)).await;
                }

                let (exit_px, pnl) = paper_chunk_pnl(entry, price, close_vol, &side, slippage, fee_rate);

                if is_last_tp || (remaining - close_vol) < 0.01 {
                    if let Ok(pnl) =
                        self.db
                            .close_position(id, exit_px, 1.0, fee_rate, "take_profit")
                            .await
                    {
                    let _ = risk.update_pnl(pnl).await;
                    let _ = risk.record_trade_outcome(&symbol, pnl > 0.0).await;
                    let _ = self.db.log_event(
                        "position_closed",
                        &format!("Paper TP{level} {symbol} ({side}) @ {exit_px:.6}"),
                        Some(json!({"pnl": pnl, "reason": "take_profit"})),
                    ).await;
                    events.push(json!({"position_id": id, "symbol": symbol, "reason": "take_profit", "level": level, "pnl": pnl}));
                    self.trailing_stops.remove(&id);
                    closed_full = true;
                    }
                } else {
                    let new_remaining = (remaining - close_vol).max(0.0);
                    let _ = self.db.partial_close_position(id, new_remaining, exit_px, pnl).await;
                    let _ = risk.update_pnl(pnl).await;
                let _ = self.db.log_event(
                        "position_partial_tp",
                        &format!("Paper partial TP{level} {symbol} ({side}) @ {price:.6} closed {close_vol:.2}"),
                        Some(json!({"pnl": pnl, "level": level, "symbol": symbol})),
                    ).await;
                    events.push(json!({"position_id": id, "symbol": symbol, "reason": "take_profit_partial", "level": level, "pnl": pnl}));
                }
                // Handle one TP per tick.
                break;
            }

            if closed_full {
                continue;
            }

            // ── Stop loss ─────────────────────────────────────────────────
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
            if let Ok(pnl) = self
                .db
                .close_position(
                    id,
                    paper_chunk_pnl(entry, price, remaining, &side, slippage, fee_rate).0,
                    1.0,
                    fee_rate,
                    reason,
                )
                .await
            {
                let _ = risk.update_pnl(pnl).await;
                let _ = risk.record_trade_outcome(&symbol, false).await;
                let event_type = if reason.contains("stop") || reason == "trailing_stop" {
                    "cut_loss"
                } else {
                    "position_closed"
                };
                let _ = self
                    .db
                    .log_event(
                        event_type,
                        &format!("Paper closed {symbol} ({reason})"),
                        Some(json!({"pnl": pnl, "reason": reason, "symbol": symbol})),
                    )
                    .await;
                events.push(json!({"position_id": id, "symbol": symbol, "reason": reason, "pnl": pnl}));
                self.trailing_stops.remove(&id);
            }
        }
        events
    }

    fn check_time_exit(&self, opened_at: &str) -> Option<String> {
        let max_hold = self.config.read().unwrap().trading.max_hold_sec;
        if max_hold == 0 {
            return None;
        }
        let opened = chrono::DateTime::parse_from_rfc3339(opened_at)
            .map(|d| d.with_timezone(&Utc))
            .ok()?;
        let age = Utc::now().signed_duration_since(opened).num_seconds().max(0) as u64;
        if age >= max_hold {
            Some("time_exit".into())
        } else {
            None
        }
    }
}
