use serde_json::{json, Value};
use sqlx::Row;

use crate::db::Database;
use crate::error::Result;

impl Database {
    pub async fn count_open_positions(&self) -> Result<i64> {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM positions WHERE status IN ('open', 'partial')")
                .fetch_one(self.pool())
                .await?;
        Ok(row.0)
    }

    pub async fn count_open_positions_by_strategy(&self, strategy: &str) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM positions WHERE status IN ('open', 'partial') AND strategy = ?",
        )
        .bind(strategy)
        .fetch_one(self.pool())
        .await?;
        Ok(row.0)
    }

    pub async fn has_open_position_on_symbol(&self, symbol: &str) -> Result<bool> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM positions WHERE status IN ('open', 'partial') AND symbol = ?",
        )
        .bind(symbol)
        .fetch_one(self.pool())
        .await?;
        Ok(row.0 > 0)
    }

    pub async fn get_open_positions(&self) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT id, symbol, side, entry_price, size, remaining_size, stop_loss, status, \
             opened_at, realized_pnl, paper, strategy, leverage, signal_id, \
             exchange_position_id, source, entry_mode, limit_price \
             FROM positions WHERE status IN ('open', 'partial') ORDER BY opened_at DESC",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(position_row).collect())
    }

    pub async fn get_open_position_by_exchange_id(&self, exchange_id: i64) -> Result<Option<Value>> {
        let row = sqlx::query(
            "SELECT id, symbol, side, entry_price, size, remaining_size, stop_loss, status, \
             opened_at, realized_pnl, paper, strategy, leverage, signal_id, \
             exchange_position_id, source, entry_mode, limit_price \
             FROM positions WHERE exchange_position_id = ? AND status IN ('open', 'partial') LIMIT 1",
        )
        .bind(exchange_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(position_row))
    }

    pub async fn get_open_position_by_symbol_side(
        &self,
        symbol: &str,
        side: &str,
        source: Option<&str>,
    ) -> Result<Option<Value>> {
        let row = match source {
            Some(src) => {
                sqlx::query(
                    "SELECT id, symbol, side, entry_price, size, remaining_size, stop_loss, status, \
                     opened_at, realized_pnl, paper, strategy, leverage, signal_id, \
                     exchange_position_id, source, entry_mode, limit_price \
                     FROM positions WHERE symbol = ? AND side = ? AND source = ? \
                     AND status IN ('open', 'partial') ORDER BY id DESC LIMIT 1",
                )
                .bind(symbol)
                .bind(side)
                .bind(src)
                .fetch_optional(self.pool())
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, symbol, side, entry_price, size, remaining_size, stop_loss, status, \
                     opened_at, realized_pnl, paper, strategy, leverage, signal_id, \
                     exchange_position_id, source, entry_mode, limit_price \
                     FROM positions WHERE symbol = ? AND side = ? \
                     AND status IN ('open', 'partial') ORDER BY id DESC LIMIT 1",
                )
                .bind(symbol)
                .bind(side)
                .fetch_optional(self.pool())
                .await?
            }
        };
        Ok(row.as_ref().map(position_row))
    }

    pub async fn get_position_by_id(&self, position_id: i64) -> Result<Option<Value>> {
        let row = sqlx::query(
            "SELECT id, symbol, side, entry_price, size, remaining_size, stop_loss, status, \
             opened_at, closed_at, realized_pnl, paper, strategy, leverage, signal_id, \
             exchange_position_id, source, exit_price, exit_reason, take_profit_levels, \
             entry_mode, limit_price \
             FROM positions WHERE id = ?",
        )
        .bind(position_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(position_row))
    }

    pub async fn get_position_by_signal_id(&self, signal_id: i64) -> Result<Option<Value>> {
        let row = sqlx::query(
            "SELECT id, symbol, side, entry_price, size, remaining_size, stop_loss, status, \
             opened_at, closed_at, realized_pnl, exit_price, exit_reason, paper, strategy, leverage, signal_id \
             FROM positions WHERE signal_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(signal_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(position_row))
    }

    pub async fn get_closed_positions(
        &self,
        limit: i64,
        strategy: Option<&str>,
        paper: Option<bool>,
    ) -> Result<Vec<Value>> {
        let cols = "SELECT id, symbol, side, entry_price, size, remaining_size, stop_loss, status, \
             opened_at, closed_at, realized_pnl, paper, strategy, leverage, signal_id, \
             exit_price, exit_reason \
             FROM positions WHERE status = 'closed'";

        let rows = match (strategy, paper) {
            (Some(s), Some(p)) => {
                sqlx::query(&format!("{cols} AND strategy = ? AND paper = ? ORDER BY closed_at DESC LIMIT ?"))
                    .bind(s)
                    .bind(if p { 1 } else { 0 })
                    .bind(limit)
                    .fetch_all(self.pool())
                    .await?
            }
            (Some(s), None) => {
                sqlx::query(&format!("{cols} AND strategy = ? ORDER BY closed_at DESC LIMIT ?"))
                    .bind(s)
                    .bind(limit)
                    .fetch_all(self.pool())
                    .await?
            }
            (None, Some(p)) => {
                sqlx::query(&format!("{cols} AND paper = ? ORDER BY closed_at DESC LIMIT ?"))
                    .bind(if p { 1 } else { 0 })
                    .bind(limit)
                    .fetch_all(self.pool())
                    .await?
            }
            (None, None) => {
                sqlx::query(&format!("{cols} ORDER BY closed_at DESC LIMIT ?"))
                    .bind(limit)
                    .fetch_all(self.pool())
                    .await?
            }
        };
        Ok(rows.iter().map(position_row).collect())
    }

    pub async fn count_closed_positions(
        &self,
        strategy: Option<&str>,
        paper: Option<bool>,
    ) -> Result<i64> {
        let base = "SELECT COUNT(*) AS n FROM positions WHERE status = 'closed'";
        let row = match (strategy, paper) {
            (Some(s), Some(p)) => {
                sqlx::query(&format!("{base} AND strategy = ? AND paper = ?"))
                    .bind(s)
                    .bind(if p { 1 } else { 0 })
                    .fetch_one(self.pool())
                    .await?
            }
            (Some(s), None) => {
                sqlx::query(&format!("{base} AND strategy = ?"))
                    .bind(s)
                    .fetch_one(self.pool())
                    .await?
            }
            (None, Some(p)) => {
                sqlx::query(&format!("{base} AND paper = ?"))
                    .bind(if p { 1 } else { 0 })
                    .fetch_one(self.pool())
                    .await?
            }
            (None, None) => sqlx::query(base).fetch_one(self.pool()).await?,
        };
        Ok(row.try_get::<i64, _>("n").unwrap_or(0))
    }

    pub async fn get_trade_stats(&self, limit: i64, strategy: Option<&str>) -> Result<Value> {
        let closed = self.get_closed_positions(limit, strategy, None).await?;
        if closed.is_empty() {
            return Ok(json!({
                "total": 0,
                "total_trades": 0,
                "wins": 0,
                "losses": 0,
                "win_rate": 0.0,
                "total_pnl": 0.0,
                "avg_pnl": 0.0,
                "profit_factor": 0.0,
            }));
        }
        let mut wins = 0i64;
        let mut total_pnl = 0.0_f64;
        let mut gross_profit = 0.0_f64;
        let mut gross_loss = 0.0_f64;
        for p in &closed {
            let pnl = p.get("realized_pnl").and_then(|v| v.as_f64()).unwrap_or(0.0);
            total_pnl += pnl;
            if pnl > 0.0 {
                wins += 1;
                gross_profit += pnl;
            } else if pnl < 0.0 {
                gross_loss += pnl.abs();
            }
        }
        let total = closed.len() as i64;
        let profit_factor = if gross_loss > 0.0 {
            (gross_profit / gross_loss * 10000.0).round() / 10000.0
        } else if gross_profit > 0.0 {
            99.0
        } else {
            0.0
        };
        Ok(json!({
            "total": total,
            "total_trades": total,
            "wins": wins,
            "losses": total - wins,
            "win_rate": if total > 0 { (wins as f64 / total as f64 * 10000.0).round() / 10000.0 } else { 0.0 },
            "total_pnl": (total_pnl * 10000.0).round() / 10000.0,
            "avg_pnl": (total_pnl / total as f64 * 10000.0).round() / 10000.0,
            "profit_factor": profit_factor,
        }))
    }

    pub async fn insert_position(
        &self,
        symbol: &str,
        side: &str,
        entry_price: f64,
        size: f64,
        stop_loss: f64,
        paper: bool,
        strategy: &str,
        leverage: i64,
        signal_id: Option<i64>,
        entry_mode: &str,
        limit_price: Option<f64>,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO positions (symbol, side, entry_price, size, remaining_size, stop_loss, status, opened_at, paper, strategy, leverage, signal_id, entry_mode, limit_price) \
             VALUES (?, ?, ?, ?, ?, ?, 'open', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(symbol)
        .bind(side)
        .bind(entry_price)
        .bind(size)
        .bind(size)
        .bind(stop_loss)
        .bind(&now)
        .bind(if paper { 1 } else { 0 })
        .bind(strategy)
        .bind(leverage)
        .bind(signal_id)
        .bind(entry_mode)
        .bind(limit_price)
        .execute(self.pool())
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn update_position_after_exchange(
        &self,
        id: i64,
        size: f64,
        entry_price: f64,
        exchange_position_id: Option<i64>,
        leverage: Option<i32>,
    ) -> Result<()> {
        match (exchange_position_id, leverage) {
            (Some(eid), Some(lev)) => {
                sqlx::query(
                    "UPDATE positions SET size = ?, remaining_size = ?, entry_price = ?, \
                     exchange_position_id = ?, leverage = ? WHERE id = ?",
                )
                .bind(size)
                .bind(size)
                .bind(entry_price)
                .bind(eid)
                .bind(lev)
                .bind(id)
                .execute(self.pool())
                .await?;
            }
            (Some(eid), None) => {
                sqlx::query(
                    "UPDATE positions SET size = ?, remaining_size = ?, entry_price = ?, \
                     exchange_position_id = ? WHERE id = ?",
                )
                .bind(size)
                .bind(size)
                .bind(entry_price)
                .bind(eid)
                .bind(id)
                .execute(self.pool())
                .await?;
            }
            (None, Some(lev)) => {
                sqlx::query(
                    "UPDATE positions SET size = ?, remaining_size = ?, entry_price = ?, leverage = ? WHERE id = ?",
                )
                .bind(size)
                .bind(size)
                .bind(entry_price)
                .bind(lev)
                .bind(id)
                .execute(self.pool())
                .await?;
            }
            (None, None) => {
                sqlx::query(
                    "UPDATE positions SET size = ?, remaining_size = ?, entry_price = ? WHERE id = ?",
                )
                .bind(size)
                .bind(size)
                .bind(entry_price)
                .bind(id)
                .execute(self.pool())
                .await?;
            }
        }
        Ok(())
    }

    pub async fn set_exchange_position_id(&self, id: i64, exchange_position_id: i64) -> Result<()> {
        sqlx::query("UPDATE positions SET exchange_position_id = ? WHERE id = ?")
            .bind(exchange_position_id)
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn insert_exchange_position(
        &self,
        symbol: &str,
        side: &str,
        entry_price: f64,
        size: f64,
        stop_loss: f64,
        leverage: i64,
        exchange_position_id: i64,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO positions (symbol, side, entry_price, size, remaining_size, stop_loss, status, \
             opened_at, paper, strategy, leverage, exchange_position_id, source) \
             VALUES (?, ?, ?, ?, ?, ?, 'open', ?, 0, 'exchange', ?, ?, 'exchange')",
        )
        .bind(symbol)
        .bind(side)
        .bind(entry_price)
        .bind(size)
        .bind(size)
        .bind(stop_loss)
        .bind(&now)
        .bind(leverage)
        .bind(exchange_position_id)
        .execute(self.pool())
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// Close a position discovered missing on the exchange (no exit price available).
    pub async fn close_position_synced(&self, id: i64, reason: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE positions SET status = 'closed', remaining_size = 0, closed_at = ?, exit_reason = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(reason)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Close a position and record realized PnL (fees included).
    ///
    /// - `contract_size` converts stored contract count → coin qty (1.0 for paper).
    /// - `fee_rate` is the taker fee rate applied to both open and close notional
    ///   (0.0 for paper, typically 0.0006 for MEXC live).
    pub async fn close_position(
        &self,
        id: i64,
        exit_price: f64,
        contract_size: f64,
        fee_rate: f64,
        reason: &str,
    ) -> Result<f64> {
        let row = sqlx::query(
            "SELECT side, entry_price, remaining_size FROM positions WHERE id = ?",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await?;
        let side: String = row.try_get("side")?;
        let entry: f64 = row.try_get("entry_price")?;
        let size: f64 = row.try_get("remaining_size")?;
        let cs = if contract_size > 0.0 { contract_size } else { 1.0 };
        let qty = size * cs;
        let gross = if side == "long" {
            (exit_price - entry) * qty
        } else {
            (entry - exit_price) * qty
        };
        // Deduct round-trip taker fees (open + close notional × fee_rate).
        let fees = (entry * qty + exit_price * qty) * fee_rate;
        let pnl = gross - fees;
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "UPDATE positions SET status = 'closed', remaining_size = 0, closed_at = ?, realized_pnl = ?, exit_price = ?, exit_reason = ? WHERE id = ?",
        )
        .bind(&now)
        .bind(pnl)
        .bind(exit_price)
        .bind(reason)
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(pnl)
    }

    /// Update stop-loss and take-profit levels (JSON string) for a position.
    /// Stores the actual SL price and a JSON array of TP levels.
    pub async fn update_position_sl_tp(
        &self,
        id: i64,
        stop_loss: Option<f64>,
        take_profit_levels: Option<&str>,
    ) -> Result<()> {
        match (stop_loss, take_profit_levels) {
            (Some(sl), Some(tp)) => {
                sqlx::query(
                    "UPDATE positions SET stop_loss = ?, take_profit_levels = ? WHERE id = ?",
                )
                .bind(sl)
                .bind(tp)
                .bind(id)
                .execute(self.pool())
                .await?;
            }
            (Some(sl), None) => {
                sqlx::query("UPDATE positions SET stop_loss = ? WHERE id = ?")
                    .bind(sl)
                    .bind(id)
                    .execute(self.pool())
                    .await?;
            }
            (None, Some(tp)) => {
                sqlx::query("UPDATE positions SET take_profit_levels = ? WHERE id = ?")
                    .bind(tp)
                    .bind(id)
                    .execute(self.pool())
                    .await?;
            }
            (None, None) => {}
        }
        Ok(())
    }

    /// Partially close a position — reduce `remaining_size` and mark 'partial'.
    /// Used by the live monitor when a TP level closes only a fraction of the position.
    pub async fn partial_close_position(&self, id: i64, new_remaining: f64, exit_price: f64) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        if new_remaining <= 0.0 {
            sqlx::query(
                "UPDATE positions SET status = 'closed', remaining_size = 0, closed_at = ?, exit_price = ? WHERE id = ?",
            )
            .bind(&now)
            .bind(exit_price)
            .bind(id)
            .execute(self.pool())
            .await?;
        } else {
            sqlx::query(
                "UPDATE positions SET status = 'partial', remaining_size = ?, exit_price = ? WHERE id = ?",
            )
            .bind(new_remaining)
            .bind(exit_price)
            .bind(id)
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }

    /// Update leverage for an existing position (used after exchange confirmation).
    pub async fn update_position_leverage(&self, id: i64, leverage: i32) -> Result<()> {
        sqlx::query("UPDATE positions SET leverage = ? WHERE id = ?")
            .bind(leverage)
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

fn position_row(row: &sqlx::sqlite::SqliteRow) -> Value {
    let entry: f64 = row.try_get("entry_price").unwrap_or(0.0);
    let exit_price: Option<f64> = row.try_get::<Option<f64>, _>("exit_price").ok().flatten();
    let side: String = row.try_get::<String, _>("side").unwrap_or_default();
    let realized_pnl: f64 = row.try_get::<f64, _>("realized_pnl").unwrap_or(0.0);

    // Parse stored take-profit levels JSON, silently default to empty array.
    let take_profit_levels: Value = row
        .try_get::<Option<String>, _>("take_profit_levels")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!([]));

    // Price move % from entry to exit (sign adjusted for direction).
    let realized_pnl_pct = match exit_price {
        Some(px) if entry > 0.0 => {
            let move_pct = if side == "short" {
                (entry - px) / entry * 100.0
            } else {
                (px - entry) / entry * 100.0
            };
            Some((move_pct * 100.0).round() / 100.0)
        }
        _ => None,
    };

    json!({
        "id": row.try_get::<i64, _>("id").unwrap_or(0),
        "symbol": row.try_get::<String, _>("symbol").unwrap_or_default(),
        "side": side,
        "entry_price": entry,
        "mark_price": entry,
        "exit_price": exit_price,
        "exit_reason": row.try_get::<Option<String>, _>("exit_reason").ok().flatten(),
        "size": row.try_get::<f64, _>("size").unwrap_or(0.0),
        "remaining_size": row.try_get::<f64, _>("remaining_size").unwrap_or(0.0),
        "stop_loss": row.try_get::<f64, _>("stop_loss").unwrap_or(0.0),
        "take_profit_levels": take_profit_levels,
        "status": row.try_get::<String, _>("status").unwrap_or_default(),
        "realized_pnl": realized_pnl,
        "realized_pnl_pct": realized_pnl_pct,
        "unrealized_pnl": 0.0,
        "unrealized_pnl_pct": 0.0,
        "strategy": row.try_get::<String, _>("strategy").unwrap_or_else(|_| "confluence".into()),
        "entry_mode": row
            .try_get::<Option<String>, _>("entry_mode")
            .ok()
            .flatten()
            .unwrap_or_else(|| "market".into()),
        "limit_price": row.try_get::<Option<f64>, _>("limit_price").ok().flatten(),
        "paper": row.try_get::<i64, _>("paper").unwrap_or(1) != 0,
        "leverage": row.try_get::<Option<i64>, _>("leverage").ok().flatten(),
        "signal_id": row.try_get::<Option<i64>, _>("signal_id").ok().flatten(),
        "exchange_position_id": row.try_get::<Option<i64>, _>("exchange_position_id").ok().flatten(),
        "source": row
            .try_get::<Option<String>, _>("source")
            .ok()
            .flatten()
            .unwrap_or_else(|| "bot".into()),
        "opened_at": row.try_get::<String, _>("opened_at").unwrap_or_default(),
        "closed_at": row.try_get::<Option<String>, _>("closed_at").ok().flatten(),
        "entry_context": {},
    })
}
