use serde_json::{json, Value};
use sqlx::Row;

use crate::db::Database;
use crate::error::Result;

/// Event types surfaced in the notifications bell and activity feed.
pub const NOTIFICATION_EVENT_TYPES: &[&str] = &[
    "position_opened",
    "position_closed",
    "trade_blocked",
    "tp_hit",
    "position_partial_tp",
    "breakeven",
    "strategy_tune",
    "strategy_optimize",
    "exchange_position_imported",
    "exchange_position_linked",
    "exchange_position_closed",
    "cut_loss",
    "live_order",
    "live_order_error",
    "live_order_dry_run",
    "position_rollback",
    "scanner",
    "signal",
    "volume_pump_detected",
    "volume_pump_signal",
    "volume_pump_armed",
    "volume_pump_expired",
    "scan",
    "kill_switch",
    "shadow_signal_saved",
    "shadow_signal_resolved",
];

impl Database {
    pub async fn log_event(&self, event_type: &str, message: &str, payload: Option<Value>) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let body = payload.unwrap_or(json!({}));
        sqlx::query(
            "INSERT INTO audit_log (event_type, message, payload, created_at, seen) VALUES (?, ?, ?, ?, 0)",
        )
        .bind(event_type)
        .bind(message)
        .bind(body.to_string())
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn get_audit_log(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT id, event_type, message, payload, created_at, seen FROM audit_log ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(audit_row).collect())
    }

    pub async fn get_trade_activity(&self, limit: i64) -> Result<Vec<Value>> {
        let placeholders = std::iter::repeat("?")
            .take(NOTIFICATION_EVENT_TYPES.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, event_type, message, payload, created_at, seen FROM audit_log \
             WHERE event_type IN ({placeholders}) ORDER BY id DESC LIMIT ?"
        );
        let mut q = sqlx::query(&sql);
        for t in NOTIFICATION_EVENT_TYPES {
            q = q.bind(*t);
        }
        q = q.bind(limit);
        let rows = q.fetch_all(self.pool()).await?;
        Ok(rows.iter().map(audit_row).collect())
    }

    pub async fn count_unread_activity(&self) -> Result<i64> {
        let placeholders = std::iter::repeat("?")
            .take(NOTIFICATION_EVENT_TYPES.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT COUNT(*) FROM audit_log WHERE seen = 0 AND event_type IN ({placeholders})"
        );
        let mut q = sqlx::query_as::<_, (i64,)>(&sql);
        for t in NOTIFICATION_EVENT_TYPES {
            q = q.bind(*t);
        }
        let row = q.fetch_one(self.pool()).await?;
        Ok(row.0)
    }

    /// Mark notification events as seen. Returns the number of rows updated.
    pub async fn mark_activity_seen(&self, ids: Option<&[i64]>, all: bool) -> Result<u64> {
        if all {
            let placeholders = std::iter::repeat("?")
                .take(NOTIFICATION_EVENT_TYPES.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "UPDATE audit_log SET seen = 1 WHERE seen = 0 AND event_type IN ({placeholders})"
            );
            let mut q = sqlx::query(&sql);
            for t in NOTIFICATION_EVENT_TYPES {
                q = q.bind(*t);
            }
            let result = q.execute(self.pool()).await?;
            return Ok(result.rows_affected());
        }

        let Some(ids) = ids else {
            return Ok(0);
        };
        if ids.is_empty() {
            return Ok(0);
        }

        let placeholders = std::iter::repeat("?").take(ids.len()).collect::<Vec<_>>().join(",");
        let sql = format!("UPDATE audit_log SET seen = 1 WHERE id IN ({placeholders})");
        let mut q = sqlx::query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        let result = q.execute(self.pool()).await?;
        Ok(result.rows_affected())
    }

    /// Time-ordered history of online-model learning checkpoints. Each row is the
    /// model's stats snapshot at the moment it learned from new resolved trades,
    /// used to draw the training/performance curves on the Training screen.
    pub async fn get_model_learn_history(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT message, payload, created_at FROM audit_log \
             WHERE event_type = 'model_learn' ORDER BY id ASC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let payload: String = row.try_get("payload").unwrap_or_else(|_| "{}".into());
                let mut v: Value = serde_json::from_str(&payload).unwrap_or(json!({}));
                if let Value::Object(ref mut m) = v {
                    m.insert(
                        "created_at".into(),
                        json!(row.try_get::<String, _>("created_at").unwrap_or_default()),
                    );
                }
                v
            })
            .collect())
    }

    pub async fn get_optimization_runs(&self, symbol: Option<&str>, limit: i64) -> Result<Vec<Value>> {
        let rows = if let Some(sym) = symbol {
            sqlx::query(
                "SELECT symbol, payload, created_at FROM optimization_runs WHERE symbol = ? ORDER BY id DESC LIMIT ?",
            )
            .bind(sym)
            .bind(limit)
            .fetch_all(self.pool())
            .await?
        } else {
            sqlx::query(
                "SELECT symbol, payload, created_at FROM optimization_runs ORDER BY id DESC LIMIT ?",
            )
            .bind(limit)
            .fetch_all(self.pool())
            .await?
        };

        Ok(rows
            .iter()
            .map(|row| {
                let payload: String = row.try_get("payload").unwrap_or_else(|_| "{}".into());
                let mut v: Value = serde_json::from_str(&payload).unwrap_or(json!({}));
                if let Value::Object(ref mut m) = v {
                    m.insert("symbol".into(), json!(row.try_get::<String, _>("symbol").unwrap_or_default()));
                    m.insert("created_at".into(), json!(row.try_get::<String, _>("created_at").unwrap_or_default()));
                }
                v
            })
            .collect())
    }

    pub async fn get_strategy_state(&self) -> Result<Value> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state_json FROM strategy_state WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        Ok(row
            .map(|(s,)| serde_json::from_str(&s).unwrap_or(json!({})))
            .unwrap_or(json!({})))
    }

    pub async fn save_strategy_state(&self, state: &Value) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE strategy_state SET state_json = ?, updated_at = ? WHERE id = 1")
            .bind(state.to_string())
            .bind(now)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

fn audit_row(row: &sqlx::sqlite::SqliteRow) -> Value {
    let payload: String = row.try_get("payload").unwrap_or_else(|_| "{}".into());
    let seen: i64 = row.try_get("seen").unwrap_or(0);
    json!({
        "id": row.try_get::<i64, _>("id").unwrap_or(0),
        "event_type": row.try_get::<String, _>("event_type").unwrap_or_default(),
        "message": row.try_get::<String, _>("message").unwrap_or_default(),
        "payload": serde_json::from_str::<Value>(&payload).unwrap_or(json!({})),
        "created_at": row.try_get::<String, _>("created_at").unwrap_or_default(),
        "seen": seen != 0,
    })
}
