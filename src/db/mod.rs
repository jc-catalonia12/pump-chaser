pub mod audit;
pub mod pnl;
pub mod portfolio;
pub mod positions;
pub mod signals;

pub use portfolio::PortfolioState;

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::ConnectOptions;
use serde_json::{json, Value};
use tracing::log::LevelFilter;

use crate::error::Result;

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn connect(sqlite_path: &str) -> Result<Self> {
        let path = Path::new(sqlite_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let options = SqliteConnectOptions::new()
            .filename(sqlite_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .disable_statement_logging()
            .log_statements(LevelFilter::Off);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS portfolio_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                equity REAL NOT NULL,
                daily_pnl REAL NOT NULL DEFAULT 0,
                weekly_pnl REAL NOT NULL DEFAULT 0,
                peak_equity REAL NOT NULL,
                last_wallet_equity REAL NOT NULL DEFAULT 0,
                paper_pnl_total REAL NOT NULL DEFAULT 0,
                equity_source TEXT NOT NULL DEFAULT 'paper',
                daily_pnl_date TEXT NOT NULL DEFAULT '',
                weekly_pnl_iso_week TEXT NOT NULL DEFAULT '',
                trading_paused INTEGER NOT NULL DEFAULT 0,
                kill_switch INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS positions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                symbol TEXT NOT NULL,
                side TEXT NOT NULL,
                entry_price REAL NOT NULL,
                size REAL NOT NULL,
                remaining_size REAL NOT NULL,
                stop_loss REAL NOT NULL,
                status TEXT NOT NULL,
                opened_at TEXT NOT NULL,
                closed_at TEXT,
                realized_pnl REAL NOT NULL DEFAULT 0,
                paper INTEGER NOT NULL DEFAULT 1,
                strategy TEXT NOT NULL DEFAULT 'confluence',
                leverage INTEGER,
                signal_id INTEGER
            );

            CREATE TABLE IF NOT EXISTS signals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                symbol TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                outcome TEXT DEFAULT 'pending'
            );

            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                message TEXT NOT NULL,
                payload TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS strategy_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                state_json TEXT NOT NULL DEFAULT '{}',
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS optimization_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                symbol TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Idempotent column additions for databases created before these fields existed.
        for stmt in [
            "ALTER TABLE positions ADD COLUMN exit_price REAL",
            "ALTER TABLE positions ADD COLUMN exit_reason TEXT",
            "ALTER TABLE positions ADD COLUMN exchange_position_id INTEGER",
            "ALTER TABLE positions ADD COLUMN source TEXT NOT NULL DEFAULT 'bot'",
            "ALTER TABLE positions ADD COLUMN take_profit_levels TEXT",
            "ALTER TABLE audit_log ADD COLUMN seen INTEGER NOT NULL DEFAULT 0",
            // Circuit-breaker state (Phase 5 additions)
            "ALTER TABLE portfolio_state ADD COLUMN consecutive_losses INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE portfolio_state ADD COLUMN paused_until INTEGER NOT NULL DEFAULT 0",
            // Shadow-only learning signals
            "ALTER TABLE signals ADD COLUMN shadow_only INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE signals ADD COLUMN reject_reason TEXT",
        ] {
            let _ = sqlx::query(stmt).execute(&self.pool).await;
        }

        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM portfolio_state WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;

        if row.0 == 0 {
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO portfolio_state (id, equity, daily_pnl, weekly_pnl, peak_equity, updated_at)
                 VALUES (1, 10000, 0, 0, 10000, ?)",
            )
            .bind(&now)
            .execute(&self.pool)
            .await?;

            sqlx::query(
                "INSERT INTO strategy_state (id, state_json, updated_at) VALUES (1, '{}', ?)",
            )
            .bind(now)
            .execute(&self.pool)
            .await?;
        }

        // One-time backfill: historical audit rows predate seen tracking.
        let state_row: Option<(String,)> =
            sqlx::query_as("SELECT state_json FROM strategy_state WHERE id = 1")
                .fetch_optional(&self.pool)
                .await?;
        let mut strategy_state: Value = state_row
            .map(|(s,)| serde_json::from_str(&s).unwrap_or(json!({})))
            .unwrap_or(json!({}));
        if strategy_state
            .get("notifications_seen_initialized")
            .and_then(|v| v.as_bool())
            != Some(true)
        {
            let _ = sqlx::query("UPDATE audit_log SET seen = 1")
                .execute(&self.pool)
                .await;
            if let Value::Object(ref mut m) = strategy_state {
                m.insert("notifications_seen_initialized".into(), json!(true));
            }
            let now = chrono::Utc::now().to_rfc3339();
            let _ = sqlx::query("UPDATE strategy_state SET state_json = ?, updated_at = ? WHERE id = 1")
                .bind(strategy_state.to_string())
                .bind(now)
                .execute(&self.pool)
                .await;
        }

        Ok(())
    }
}
