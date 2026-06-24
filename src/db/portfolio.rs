use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::db::Database;
use crate::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PortfolioState {
    pub equity: f64,
    pub daily_pnl: f64,
    pub weekly_pnl: f64,
    pub peak_equity: f64,
    pub last_wallet_equity: f64,
    pub paper_pnl_total: f64,
    pub equity_source: String,
    pub daily_pnl_date: String,
    pub weekly_pnl_iso_week: String,
    pub trading_paused: i64,
    pub kill_switch: i64,
    /// Number of consecutive losses since the last win. Resets to 0 on a win.
    #[serde(default)]
    pub consecutive_losses: i64,
    /// Unix timestamp until which new entries are paused by the circuit breaker.
    /// 0 means not paused by circuit breaker.
    #[serde(default)]
    pub paused_until: i64,
}

impl Database {
    pub async fn get_portfolio_state(&self) -> Result<PortfolioState> {
        let row = sqlx::query_as::<_, PortfolioState>(
            r#"SELECT equity, daily_pnl, weekly_pnl, peak_equity, last_wallet_equity,
                      paper_pnl_total, equity_source, daily_pnl_date, weekly_pnl_iso_week,
                      trading_paused, kill_switch,
                      COALESCE(consecutive_losses, 0) AS consecutive_losses,
                      COALESCE(paused_until, 0) AS paused_until
               FROM portfolio_state WHERE id = 1"#,
        )
        .fetch_one(self.pool())
        .await?;
        Ok(row)
    }

    pub async fn save_portfolio_state(&self, state: &PortfolioState) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            r#"UPDATE portfolio_state SET
                equity = ?, daily_pnl = ?, weekly_pnl = ?, peak_equity = ?,
                last_wallet_equity = ?, paper_pnl_total = ?, equity_source = ?,
                daily_pnl_date = ?, weekly_pnl_iso_week = ?,
                trading_paused = ?, kill_switch = ?,
                consecutive_losses = ?, paused_until = ?,
                updated_at = ?
               WHERE id = 1"#,
        )
        .bind(state.equity)
        .bind(state.daily_pnl)
        .bind(state.weekly_pnl)
        .bind(state.peak_equity)
        .bind(state.last_wallet_equity)
        .bind(state.paper_pnl_total)
        .bind(&state.equity_source)
        .bind(&state.daily_pnl_date)
        .bind(&state.weekly_pnl_iso_week)
        .bind(state.trading_paused)
        .bind(state.kill_switch)
        .bind(state.consecutive_losses)
        .bind(state.paused_until)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}
