use serde_json::{json, Value};
use sqlx::Row;

use crate::db::Database;
use crate::error::Result;

impl Database {
    pub async fn insert_param_evolution(
        &self,
        champion: &Value,
        challenger: &Value,
        oos_metrics: &Value,
        promoted: bool,
    ) -> Result<i64> {
        let now = chrono::Utc::now().to_rfc3339();
        let result = sqlx::query(
            "INSERT INTO param_evolution (champion, challenger, oos_metrics, promoted, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(champion.to_string())
        .bind(challenger.to_string())
        .bind(oos_metrics.to_string())
        .bind(if promoted { 1 } else { 0 })
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(result.last_insert_rowid())
    }

    pub async fn get_param_evolution_history(&self, limit: i64) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT id, champion, challenger, oos_metrics, promoted, created_at \
             FROM param_evolution ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|row| {
                let champion: String = row.try_get("champion").unwrap_or_else(|_| "{}".into());
                let challenger: String = row.try_get("challenger").unwrap_or_else(|_| "{}".into());
                let oos: String = row.try_get("oos_metrics").unwrap_or_else(|_| "{}".into());
                json!({
                    "id": row.try_get::<i64, _>("id").unwrap_or(0),
                    "champion": serde_json::from_str::<Value>(&champion).unwrap_or(json!({})),
                    "challenger": serde_json::from_str::<Value>(&challenger).unwrap_or(json!({})),
                    "oos_metrics": serde_json::from_str::<Value>(&oos).unwrap_or(json!({})),
                    "promoted": row.try_get::<i64, _>("promoted").unwrap_or(0) != 0,
                    "created_at": row.try_get::<String, _>("created_at").unwrap_or_default(),
                })
            })
            .collect())
    }

    pub async fn get_strategy_overlay(&self) -> Result<Value> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state_json FROM strategy_state WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        let state: Value = row
            .map(|(s,)| serde_json::from_str(&s).unwrap_or(json!({})))
            .unwrap_or(json!({}));
        Ok(state
            .get("param_overlay")
            .cloned()
            .unwrap_or(json!({})))
    }

    pub async fn set_strategy_overlay(&self, overlay: &Value) -> Result<()> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state_json FROM strategy_state WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        let mut state: Value = row
            .map(|(s,)| serde_json::from_str(&s).unwrap_or(json!({})))
            .unwrap_or(json!({}));
        if let Value::Object(ref mut m) = state {
            m.insert("param_overlay".into(), overlay.clone());
        }
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE strategy_state SET state_json = ?, updated_at = ? WHERE id = 1")
            .bind(state.to_string())
            .bind(now)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn get_ml_gate_auto_state(&self) -> Result<bool> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state_json FROM strategy_state WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        let state: Value = row
            .map(|(s,)| serde_json::from_str(&s).unwrap_or(json!({})))
            .unwrap_or(json!({}));
        Ok(state
            .get("ml_gate_auto_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
    }

    pub async fn set_ml_gate_auto_state(&self, enabled: bool) -> Result<()> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT state_json FROM strategy_state WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        let mut state: Value = row
            .map(|(s,)| serde_json::from_str(&s).unwrap_or(json!({})))
            .unwrap_or(json!({}));
        if let Value::Object(ref mut m) = state {
            m.insert("ml_gate_auto_enabled".into(), json!(enabled));
        }
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE strategy_state SET state_json = ?, updated_at = ? WHERE id = 1")
            .bind(state.to_string())
            .bind(now)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
