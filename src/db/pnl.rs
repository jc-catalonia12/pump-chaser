use serde_json::{json, Value};
use sqlx::Row;

use crate::db::Database;
use crate::error::Result;

impl Database {
    /// Sum realized PnL for positions closed on a UTC calendar day (`YYYY-MM-DD`).
    pub async fn sum_realized_pnl_for_day(
        &self,
        day: &str,
        paper: Option<bool>,
    ) -> Result<f64> {
        let row = match paper {
            Some(true) => {
                sqlx::query(
                    "SELECT COALESCE(SUM(realized_pnl), 0) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL AND paper = 1 \
                     AND substr(closed_at, 1, 10) = ?",
                )
                .bind(day)
                .fetch_one(self.pool())
                .await?
            }
            Some(false) => {
                sqlx::query(
                    "SELECT COALESCE(SUM(realized_pnl), 0) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL AND paper = 0 \
                     AND substr(closed_at, 1, 10) = ?",
                )
                .bind(day)
                .fetch_one(self.pool())
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT COALESCE(SUM(realized_pnl), 0) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL \
                     AND substr(closed_at, 1, 10) = ?",
                )
                .bind(day)
                .fetch_one(self.pool())
                .await?
            }
        };
        Ok(row.try_get::<f64, _>("pnl").unwrap_or(0.0))
    }

    /// Sum realized PnL for positions closed within an ISO week (inclusive, UTC dates).
    pub async fn sum_realized_pnl_for_week(
        &self,
        week_start: &str,
        week_end: &str,
        paper: Option<bool>,
    ) -> Result<f64> {
        let row = match paper {
            Some(true) => {
                sqlx::query(
                    "SELECT COALESCE(SUM(realized_pnl), 0) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL AND paper = 1 \
                     AND substr(closed_at, 1, 10) >= ? AND substr(closed_at, 1, 10) <= ?",
                )
                .bind(week_start)
                .bind(week_end)
                .fetch_one(self.pool())
                .await?
            }
            Some(false) => {
                sqlx::query(
                    "SELECT COALESCE(SUM(realized_pnl), 0) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL AND paper = 0 \
                     AND substr(closed_at, 1, 10) >= ? AND substr(closed_at, 1, 10) <= ?",
                )
                .bind(week_start)
                .bind(week_end)
                .fetch_one(self.pool())
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT COALESCE(SUM(realized_pnl), 0) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL \
                     AND substr(closed_at, 1, 10) >= ? AND substr(closed_at, 1, 10) <= ?",
                )
                .bind(week_start)
                .bind(week_end)
                .fetch_one(self.pool())
                .await?
            }
        };
        Ok(row.try_get::<f64, _>("pnl").unwrap_or(0.0))
    }

    pub async fn get_daily_pnl_history(&self, paper: Option<bool>) -> Result<Vec<Value>> {
        let rows = match paper {
            Some(true) => {
                sqlx::query(
                    "SELECT substr(closed_at, 1, 10) AS day, COUNT(*) AS trades, \
                     SUM(CASE WHEN realized_pnl > 0 THEN 1 ELSE 0 END) AS wins, \
                     SUM(realized_pnl) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL AND paper = 1 \
                     GROUP BY day ORDER BY day ASC",
                )
                .fetch_all(self.pool())
                .await?
            }
            Some(false) => {
                sqlx::query(
                    "SELECT substr(closed_at, 1, 10) AS day, COUNT(*) AS trades, \
                     SUM(CASE WHEN realized_pnl > 0 THEN 1 ELSE 0 END) AS wins, \
                     SUM(realized_pnl) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL AND paper = 0 \
                     GROUP BY day ORDER BY day ASC",
                )
                .fetch_all(self.pool())
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT substr(closed_at, 1, 10) AS day, COUNT(*) AS trades, \
                     SUM(CASE WHEN realized_pnl > 0 THEN 1 ELSE 0 END) AS wins, \
                     SUM(realized_pnl) AS pnl FROM positions \
                     WHERE status = 'closed' AND closed_at IS NOT NULL \
                     GROUP BY day ORDER BY day ASC",
                )
                .fetch_all(self.pool())
                .await?
            }
        };

        let mut cumulative = 0.0_f64;
        let mut history = Vec::new();
        for row in rows {
            use sqlx::Row;
            let day: String = row.try_get("day")?;
            let trades: i64 = row.try_get("trades")?;
            let wins: i64 = row.try_get("wins")?;
            let day_pnl: f64 = row.try_get::<Option<f64>, _>("pnl")?.unwrap_or(0.0);
            let day_pnl = (day_pnl * 10000.0).round() / 10000.0;
            cumulative = ((cumulative + day_pnl) * 10000.0).round() / 10000.0;
            let win_rate = if trades > 0 {
                ((wins as f64 / trades as f64) * 10000.0).round() / 10000.0
            } else {
                0.0
            };
            history.push(json!({
                "day": day,
                "trades": trades,
                "wins": wins,
                "losses": trades - wins,
                "win_rate": win_rate,
                "pnl": day_pnl,
                "cumulative_pnl": cumulative,
            }));
        }
        Ok(history)
    }

    pub async fn get_trading_started_at(&self) -> Result<Option<String>> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as("SELECT MIN(opened_at) FROM positions WHERE opened_at IS NOT NULL")
                .fetch_optional(self.pool())
                .await?;
        Ok(row.and_then(|(s,)| s))
    }
}
