use serde_json::{json, Value};
use sqlx::Row;

use crate::db::Database;
use crate::error::Result;

impl Database {
    pub async fn get_recent_signals(&self, limit: i64) -> Result<Vec<Value>> {
        self.get_signals_paged(limit, 0).await
    }

    pub async fn count_signals(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM signals")
            .fetch_one(self.pool())
            .await?;
        Ok(row.try_get::<i64, _>("n").unwrap_or(0))
    }

    pub async fn get_signals_paged(&self, limit: i64, offset: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT id, symbol, payload, created_at, outcome FROM signals \
             ORDER BY id DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(signal_row_to_dict).collect())
    }

    pub async fn get_signals_for_symbol(&self, symbol: &str, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT id, symbol, payload, created_at, outcome FROM signals WHERE symbol = ? ORDER BY id DESC LIMIT ?",
        )
        .bind(symbol)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(signal_row_to_dict).collect())
    }

    pub async fn get_pending_signals(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT id, symbol, payload, created_at, outcome FROM signals \
             WHERE outcome IS NULL OR outcome = 'pending' ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(signal_row_to_dict).collect())
    }

    pub async fn get_strategy_outcome_stats(&self) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT json_extract(payload, '$.strategy') AS strategy, outcome, COUNT(*) AS n \
             FROM signals WHERE outcome IN ('win', 'loss', 'expired') \
             GROUP BY strategy, outcome",
        )
        .fetch_all(self.pool())
        .await?;

        let mut map: std::collections::HashMap<String, (i64, i64, i64)> = std::collections::HashMap::new();
        for row in rows {
            let strategy: String = row.try_get::<Option<String>, _>("strategy")?.unwrap_or_else(|| "unknown".into());
            let outcome: String = row.try_get("outcome")?;
            let n: i64 = row.try_get("n")?;
            let entry = map.entry(strategy).or_insert((0, 0, 0));
            match outcome.as_str() {
                "win" => entry.0 += n,
                "loss" => entry.1 += n,
                "expired" => entry.2 += n,
                _ => {}
            }
        }

        let mut out: Vec<Value> = map
            .into_iter()
            .map(|(strategy, (wins, losses, expired))| {
                let resolved = wins + losses;
                let win_rate = if resolved > 0 {
                    wins as f64 / resolved as f64
                } else {
                    0.0
                };
                json!({
                    "strategy": strategy,
                    "wins": wins,
                    "losses": losses,
                    "expired": expired,
                    "resolved": resolved,
                    "total_resolved": resolved,
                    "win_rate": (win_rate * 10000.0).round() / 10000.0,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            a.get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(b.get("strategy").and_then(|v| v.as_str()).unwrap_or(""))
        });
        Ok(out)
    }
}

fn signal_row_to_dict(row: &sqlx::sqlite::SqliteRow) -> Value {
    let id: i64 = row.try_get("id").unwrap_or(0);
    let symbol: String = row.try_get("symbol").unwrap_or_default();
    let payload: String = row.try_get("payload").unwrap_or_else(|_| "{}".into());
    let created_at: String = row.try_get("created_at").unwrap_or_default();
    let outcome: Option<String> = row.try_get("outcome").ok();

    let mut obj: Value = serde_json::from_str(&payload).unwrap_or(json!({}));
    if let Value::Object(ref mut map) = obj {
        map.insert("id".into(), json!(id));
        map.insert("symbol".into(), json!(symbol));
        map.insert("generated_at".into(), json!(created_at));
        map.insert("created_at".into(), json!(created_at));
        map.insert("outcome".into(), json!(outcome.unwrap_or_else(|| "pending".into())));
        if let Ok(shadow_only) = row.try_get::<i64, _>("shadow_only") {
            map.insert("shadow_only".into(), json!(shadow_only != 0));
        }
        if let Ok(reason) = row.try_get::<Option<String>, _>("reject_reason") {
            if let Some(r) = reason {
                map.insert("reject_reason".into(), json!(r));
            }
        }
    }
    obj
}

impl Database {
    pub async fn get_signal_by_id(&self, signal_id: i64) -> Result<Option<Value>> {
        let row = sqlx::query(
            "SELECT id, symbol, payload, created_at, outcome FROM signals WHERE id = ?",
        )
        .bind(signal_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(signal_row_to_dict))
    }

    pub async fn update_signal_outcome(&self, signal_id: i64, outcome: &str) -> Result<()> {
        sqlx::query("UPDATE signals SET outcome = ? WHERE id = ?")
            .bind(outcome)
            .bind(signal_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn set_signal_reject_reason(&self, signal_id: i64, reject_reason: &str) -> Result<()> {
        sqlx::query("UPDATE signals SET reject_reason = ? WHERE id = ?")
            .bind(reject_reason)
            .bind(signal_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Write a recomputed feature vector back into a signal's stored payload so
    /// historically featureless signals become trainable.
    pub async fn set_signal_features(&self, signal_id: i64, features: &[f64]) -> Result<()> {
        let row = sqlx::query("SELECT payload FROM signals WHERE id = ?")
            .bind(signal_id)
            .fetch_optional(self.pool())
            .await?;
        let Some(row) = row else {
            return Ok(());
        };
        let payload: String = row.try_get("payload").unwrap_or_else(|_| "{}".into());
        let mut v: Value = serde_json::from_str(&payload).unwrap_or(json!({}));
        if let Value::Object(ref mut m) = v {
            m.insert("ml_features".into(), json!(features));
        }
        sqlx::query("UPDATE signals SET payload = ? WHERE id = ?")
            .bind(v.to_string())
            .bind(signal_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Aggregate signal outcome counts (win / loss / expired / pending) used by
    /// the Training screen breakdown.
    pub async fn get_signal_outcome_counts(&self) -> Result<Value> {
        let rows = sqlx::query("SELECT outcome, COUNT(*) AS n FROM signals GROUP BY outcome")
            .fetch_all(self.pool())
            .await?;
        let (mut win, mut loss, mut expired, mut pending) = (0i64, 0i64, 0i64, 0i64);
        for row in rows {
            let outcome: Option<String> = row.try_get("outcome").ok();
            let n: i64 = row.try_get("n").unwrap_or(0);
            match outcome.as_deref().unwrap_or("pending") {
                "win" => win = n,
                "loss" => loss = n,
                "expired" => expired = n,
                _ => pending += n,
            }
        }
        let resolved = win + loss + expired;
        let win_rate = if resolved > 0 {
            win as f64 / resolved as f64
        } else {
            0.0
        };
        Ok(json!({
            "win": win,
            "loss": loss,
            "expired": expired,
            "pending": pending,
            "resolved": resolved,
            "total": resolved + pending,
            "win_rate": (win_rate * 10000.0).round() / 10000.0,
        }))
    }

    /// Closed positions whose linked signal has not been resolved yet. Used by the
    /// learning loop to mark win/loss outcomes and train the online model.
    pub async fn get_unresolved_closed_positions(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT p.signal_id AS signal_id, p.realized_pnl AS realized_pnl, \
             p.symbol AS symbol, p.side AS side, p.strategy AS strategy, \
             p.exit_price AS exit_price, \
             p.exit_reason AS exit_reason, p.entry_price AS entry_price, \
             p.size AS size, p.leverage AS leverage, \
             s.payload AS payload, s.outcome AS outcome \
             FROM positions p JOIN signals s ON s.id = p.signal_id \
             WHERE p.status = 'closed' AND p.signal_id IS NOT NULL \
             AND (s.outcome IS NULL OR s.outcome = 'pending') \
             ORDER BY p.closed_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        Ok(rows
            .iter()
            .map(|row| {
                let payload: String = row.try_get("payload").unwrap_or_else(|_| "{}".into());
                json!({
                    "signal_id": row.try_get::<i64, _>("signal_id").unwrap_or(0),
                    "realized_pnl": row.try_get::<f64, _>("realized_pnl").unwrap_or(0.0),
                    "symbol": row.try_get::<String, _>("symbol").unwrap_or_default(),
                    "side": row.try_get::<String, _>("side").unwrap_or_default(),
                    "strategy": row.try_get::<Option<String>, _>("strategy").ok().flatten(),
                    "exit_price": row.try_get::<Option<f64>, _>("exit_price").ok().flatten(),
                    "exit_reason": row.try_get::<Option<String>, _>("exit_reason").ok().flatten(),
                    "entry_price": row.try_get::<f64, _>("entry_price").unwrap_or(0.0),
                    "size": row.try_get::<f64, _>("size").unwrap_or(0.0),
                    "leverage": row.try_get::<Option<i64>, _>("leverage").ok().flatten(),
                    "payload": serde_json::from_str::<Value>(&payload).unwrap_or(json!({})),
                })
            })
            .collect())
    }

    /// All resolved signals (win/loss) that carry stored ML features, oldest first,
    /// for bootstrapping/replaying into the online model.
    pub async fn get_resolved_signals_with_features(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT payload, outcome FROM signals \
             WHERE outcome IN ('win', 'loss', 'expired') \
             ORDER BY id ASC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;

        Ok(rows
            .iter()
            .filter_map(|row| {
                let payload: String = row.try_get("payload").ok()?;
                let outcome: String = row.try_get("outcome").ok()?;
                let mut v: Value = serde_json::from_str(&payload).ok()?;
                if let Value::Object(ref mut m) = v {
                    m.insert("outcome".into(), json!(outcome));
                }
                Some(v)
            })
            .collect())
    }

    /// Win rate only on signals that passed the ML gate (setup_probability_pct >= threshold).
    /// Returns a JSON object with win/loss/expired/win_rate for both gated and all signals.
    pub async fn get_postgatewin_stats(&self, threshold_pct: f64) -> Result<Value> {
        // All resolved
        let all = self.get_signal_outcome_counts().await?;

        // Post-gate: setup_probability_pct stored in payload JSON
        let rows = sqlx::query(
            "SELECT outcome, COUNT(*) AS n FROM signals \
             WHERE outcome IN ('win','loss','expired') \
             AND CAST(json_extract(payload,'$.setup_probability_pct') AS REAL) >= ? \
             GROUP BY outcome",
        )
        .bind(threshold_pct)
        .fetch_all(self.pool())
        .await?;

        let (mut win, mut loss, mut expired) = (0i64, 0i64, 0i64);
        for row in rows {
            let outcome: String = row.try_get("outcome").unwrap_or_default();
            let n: i64 = row.try_get("n").unwrap_or(0);
            match outcome.as_str() {
                "win" => win = n,
                "loss" => loss = n,
                "expired" => expired = n,
                _ => {}
            }
        }
        let resolved = win + loss;
        let win_rate = if resolved > 0 { win as f64 / resolved as f64 } else { 0.0 };
        Ok(json!({
            "all": all,
            "post_gate": {
                "win": win,
                "loss": loss,
                "expired": expired,
                "resolved": resolved,
                "win_rate": (win_rate * 10000.0).round() / 10000.0,
                "threshold_pct": threshold_pct,
            },
        }))
    }

    /// Outcome breakdown by side (long / short) based on price_change_pct in payload.
    pub async fn get_side_outcome_stats(&self) -> Result<Value> {
        let rows = sqlx::query(
            "SELECT \
               CASE WHEN CAST(json_extract(payload,'$.price_change_pct') AS REAL) >= 0 THEN 'long' ELSE 'short' END AS side, \
               outcome, COUNT(*) AS n \
             FROM signals \
             WHERE outcome IN ('win','loss','expired') \
             GROUP BY side, outcome",
        )
        .fetch_all(self.pool())
        .await?;

        let mut long = (0i64, 0i64, 0i64); // (win, loss, expired)
        let mut short = (0i64, 0i64, 0i64);
        for row in rows {
            let side: String = row.try_get("side").unwrap_or_default();
            let outcome: String = row.try_get("outcome").unwrap_or_default();
            let n: i64 = row.try_get("n").unwrap_or(0);
            let bucket = if side == "long" { &mut long } else { &mut short };
            match outcome.as_str() {
                "win" => bucket.0 += n,
                "loss" => bucket.1 += n,
                "expired" => bucket.2 += n,
                _ => {}
            }
        }
        let side_obj = |label: &str, (win, loss, expired): (i64, i64, i64)| -> Value {
            let resolved = win + loss;
            let wr = if resolved > 0 { win as f64 / resolved as f64 } else { 0.0 };
            json!({
                "side": label,
                "win": win,
                "loss": loss,
                "expired": expired,
                "resolved": resolved,
                "win_rate": (wr * 10000.0).round() / 10000.0,
            })
        };
        Ok(json!({
            "long": side_obj("long", long),
            "short": side_obj("short", short),
        }))
    }

    /// Win rate over the last N days, grouped by day (for the rolling chart).
    pub async fn get_rolling_win_rate(&self, days: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT date(created_at) AS day, outcome, COUNT(*) AS n \
             FROM signals \
             WHERE outcome IN ('win','loss') \
             AND created_at >= datetime('now', ?||' days') \
             GROUP BY day, outcome \
             ORDER BY day ASC",
        )
        .bind(format!("-{}", days))
        .fetch_all(self.pool())
        .await?;

        let mut map: std::collections::BTreeMap<String, (i64, i64)> = std::collections::BTreeMap::new();
        for row in rows {
            let day: String = row.try_get("day").unwrap_or_default();
            let outcome: String = row.try_get("outcome").unwrap_or_default();
            let n: i64 = row.try_get("n").unwrap_or(0);
            let entry = map.entry(day).or_insert((0, 0));
            if outcome == "win" { entry.0 += n; } else { entry.1 += n; }
        }
        Ok(map
            .into_iter()
            .map(|(day, (win, loss))| {
                let resolved = win + loss;
                let wr = if resolved > 0 { win as f64 / resolved as f64 } else { 0.0 };
                json!({ "day": day, "win": win, "loss": loss, "win_rate": (wr * 10000.0).round() / 10000.0 })
            })
            .collect())
    }

    pub async fn insert_signal(&self, signal: &crate::signals::PumpSignal) -> Result<i64> {
        let payload = signal.to_payload().to_string();
        let now = signal.generated_at.to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO signals (symbol, payload, created_at, outcome, shadow_only) \
             VALUES (?, ?, ?, 'pending', 0)",
        )
        .bind(&signal.symbol)
        .bind(payload)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// Persist a signal for shadow-only training — never eligible for execution.
    pub async fn insert_shadow_signal(
        &self,
        signal: &crate::signals::PumpSignal,
        reject_reason: &str,
    ) -> Result<i64> {
        let mut payload = signal.to_payload();
        if let Value::Object(ref mut m) = payload {
            m.insert("shadow_only".into(), json!(true));
            m.insert("reject_reason".into(), json!(reject_reason));
        }
        let now = signal.generated_at.to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO signals (symbol, payload, created_at, outcome, shadow_only, reject_reason) \
             VALUES (?, ?, ?, 'pending', 1, ?)",
        )
        .bind(&signal.symbol)
        .bind(payload.to_string())
        .bind(&now)
        .bind(reject_reason)
        .execute(self.pool())
        .await?;
        Ok(result.last_insert_rowid())
    }

    /// Count pending shadow-only signals (for queue cap).
    pub async fn count_pending_shadow_signals(&self) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM signals WHERE shadow_only = 1 \
             AND (outcome IS NULL OR outcome = 'pending')",
        )
        .fetch_one(self.pool())
        .await?;
        Ok(row.0)
    }

    /// Shadow saves for a symbol in the last `within_sec` seconds.
    pub async fn count_recent_shadow_signals(&self, symbol: &str, within_sec: i64) -> Result<i64> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(within_sec)).to_rfc3339();
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM signals WHERE shadow_only = 1 AND symbol = ? AND created_at >= ?",
        )
        .bind(symbol)
        .bind(cutoff)
        .fetch_one(self.pool())
        .await?;
        Ok(row.0)
    }

    /// Shadow saves for a symbol+side within cooldown (dedupe guard).
    pub async fn count_recent_shadow_signals_side(
        &self,
        symbol: &str,
        side_long: bool,
        within_sec: i64,
    ) -> Result<i64> {
        let cutoff = (chrono::Utc::now() - chrono::Duration::seconds(within_sec)).to_rfc3339();
        let side_cmp = if side_long { ">" } else { "<=" };
        let sql = format!(
            "SELECT COUNT(*) FROM signals WHERE shadow_only = 1 AND symbol = ? \
             AND created_at >= ? AND json_extract(payload, '$.price_change_pct') {side_cmp} 0"
        );
        let row: (i64,) = sqlx::query_as(&sql)
            .bind(symbol)
            .bind(cutoff)
            .fetch_one(self.pool())
            .await?;
        Ok(row.0)
    }

    /// Recent shadow-only signals for the Training tab.
    pub async fn get_recent_shadow_signals(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT id, symbol, payload, created_at, outcome, shadow_only, reject_reason \
             FROM signals WHERE shadow_only = 1 ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(signal_row_to_dict).collect())
    }

    /// Aggregate shadow signal stats for the Training tab.
    pub async fn get_shadow_signal_stats(&self) -> Result<Value> {
        let pending: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM signals WHERE shadow_only = 1 \
             AND (outcome IS NULL OR outcome = 'pending')",
        )
        .fetch_one(self.pool())
        .await?;

        let saved_24h: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM signals WHERE shadow_only = 1 \
             AND created_at >= datetime('now', '-1 day')",
        )
        .fetch_one(self.pool())
        .await?;

        let resolved_24h: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM signals WHERE shadow_only = 1 \
             AND outcome IN ('win','loss','expired') \
             AND created_at >= datetime('now', '-1 day')",
        )
        .fetch_one(self.pool())
        .await?;

        let by_reason = sqlx::query(
            "SELECT reject_reason, outcome, COUNT(*) AS n FROM signals \
             WHERE shadow_only = 1 GROUP BY reject_reason, outcome",
        )
        .fetch_all(self.pool())
        .await?;

        let mut breakdown: Vec<Value> = Vec::new();
        for row in by_reason {
            breakdown.push(json!({
                "reject_reason": row.try_get::<Option<String>, _>("reject_reason").ok().flatten(),
                "outcome": row.try_get::<Option<String>, _>("outcome").ok().flatten(),
                "count": row.try_get::<i64, _>("n").unwrap_or(0),
            }));
        }

        // ML-gate shadow outcomes — high loss rate means the gate rejected correctly.
        let ml_gate_rows = sqlx::query(
            "SELECT outcome, COUNT(*) AS n FROM signals \
             WHERE shadow_only = 1 AND reject_reason = 'ml_gate' \
             AND outcome IN ('win', 'loss') GROUP BY outcome",
        )
        .fetch_all(self.pool())
        .await?;
        let mut ml_wins = 0i64;
        let mut ml_losses = 0i64;
        for row in ml_gate_rows {
            let outcome: String = row.try_get("outcome").unwrap_or_default();
            let n: i64 = row.try_get("n").unwrap_or(0);
            match outcome.as_str() {
                "win" => ml_wins += n,
                "loss" => ml_losses += n,
                _ => {}
            }
        }
        let ml_resolved = ml_wins + ml_losses;
        let ml_gate_reject_precision = if ml_resolved > 0 {
            (ml_losses as f64 / ml_resolved as f64 * 10000.0).round() / 10000.0
        } else {
            0.0
        };

        // Aggregate by reject_reason (all outcomes).
        let reason_totals = sqlx::query(
            "SELECT reject_reason, COUNT(*) AS n FROM signals \
             WHERE shadow_only = 1 GROUP BY reject_reason",
        )
        .fetch_all(self.pool())
        .await?;
        let mut by_reason_summary: Vec<Value> = Vec::new();
        for row in reason_totals {
            by_reason_summary.push(json!({
                "reject_reason": row.try_get::<Option<String>, _>("reject_reason").ok().flatten(),
                "count": row.try_get::<i64, _>("n").unwrap_or(0),
            }));
        }

        Ok(json!({
            "pending": pending.0,
            "saved_24h": saved_24h.0,
            "resolved_24h": resolved_24h.0,
            "by_reason": breakdown,
            "by_reason_summary": by_reason_summary,
            "ml_gate_shadow": {
                "wins": ml_wins,
                "losses": ml_losses,
                "resolved": ml_resolved,
                "reject_precision": ml_gate_reject_precision,
            },
        }))
    }
}
