//! Risk manager — first critical module ported from Python (`pump_chaser/risk/manager.py`).
//!
//! Safety additions (circuit breakers):
//!   - Consecutive-loss streak auto-pause: after N losses in a row, block new
//!     entries for `loss_streak_cooldown_sec` seconds then auto-resume.
//!   - Max-drawdown auto-kill: if equity drops more than `max_drawdown_halt_pct`
//!     from peak, the kill switch is tripped and must be manually reset.
//!   - Per-symbol stop-out cooldown: after a loss on a symbol, block re-entry on
//!     that symbol for `symbol_loss_cooldown_sec` to prevent revenge-trades.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use serde::Serialize;

use crate::config::AppConfig;
use crate::db::{Database, PortfolioState};
use crate::error::{BotError, Result};

#[derive(Debug, Clone, Serialize)]
pub struct RiskMetrics {
    pub equity: f64,
    pub peak_equity: f64,
    pub daily_pnl: f64,
    pub daily_pnl_pct: f64,
    pub weekly_pnl: f64,
    pub equity_source: String,
    pub open_positions: i64,
    pub trading_paused: bool,
    pub kill_switch: bool,
    pub max_risk_per_trade_pct: f64,
}

pub struct RiskManager {
    config: Arc<AppConfig>,
    db: Arc<Database>,
    state: PortfolioState,
    /// In-memory per-symbol stop-out cooldown: symbol → unix timestamp expiry.
    /// Persisted implicitly through the cooldown window; not stored in DB (lost
    /// on restart, which is acceptable — a restart is a fresh session).
    symbol_cooldowns: HashMap<String, i64>,
}

impl RiskManager {
    pub async fn new(config: Arc<AppConfig>, db: Arc<Database>) -> Result<Self> {
        let mut state = db.get_portfolio_state().await?;
        roll_pnl_periods(&mut state);
        if state.daily_pnl_date.is_empty() {
            db.save_portfolio_state(&state).await?;
        }
        Ok(Self {
            config,
            db,
            state,
            symbol_cooldowns: HashMap::new(),
        })
    }

    pub fn metrics(&self, open_positions: i64) -> RiskMetrics {
        let equity = self.state.equity.max(1.0);
        RiskMetrics {
            equity: round2(self.state.equity),
            peak_equity: round2(self.state.peak_equity),
            daily_pnl: round2(self.state.daily_pnl),
            daily_pnl_pct: round2(self.state.daily_pnl / equity * 100.0),
            weekly_pnl: round2(self.state.weekly_pnl),
            equity_source: self.state.equity_source.clone(),
            open_positions,
            trading_paused: self.state.trading_paused != 0,
            kill_switch: self.state.kill_switch != 0,
            max_risk_per_trade_pct: round2(self.config.risk.max_risk_per_trade * 100.0),
        }
    }

    /// Record the outcome of a closed trade and apply circuit-breaker logic.
    ///
    /// On a win:  consecutive loss counter resets.
    /// On a loss: counter increments; if it hits `max_consecutive_losses` the
    ///            bot pauses for `loss_streak_cooldown_sec`; if drawdown exceeds
    ///            `max_drawdown_halt_pct` the kill switch fires automatically.
    pub async fn record_trade_outcome(&mut self, symbol: &str, won: bool) -> Result<()> {
        if won {
            self.state.consecutive_losses = 0;
        } else {
            self.state.consecutive_losses += 1;

            // Per-symbol cooldown: block re-entry on this symbol for a window.
            let cooldown = self.config.risk.symbol_loss_cooldown_sec as i64;
            if cooldown > 0 {
                let expiry = Utc::now().timestamp() + cooldown;
                self.symbol_cooldowns.insert(symbol.to_string(), expiry);
            }

            // Consecutive-loss streak breaker.
            let max_streak = self.config.risk.max_consecutive_losses as i64;
            if max_streak > 0 && self.state.consecutive_losses >= max_streak {
                let cooldown_sec = self.config.risk.loss_streak_cooldown_sec as i64;
                let until = Utc::now().timestamp() + cooldown_sec;
                self.state.paused_until = until;
                self.state.trading_paused = 1;
                let _ = self
                    .db
                    .log_event(
                        "circuit_breaker",
                        &format!(
                            "Loss streak circuit breaker tripped after {} consecutive losses — pausing for {}s",
                            self.state.consecutive_losses, cooldown_sec
                        ),
                        Some(serde_json::json!({
                            "consecutive_losses": self.state.consecutive_losses,
                            "paused_until": until,
                            "symbol": symbol,
                        })),
                    )
                    .await;
            }

            // Max-drawdown auto-kill-switch.
            let dd_limit = self.config.risk.max_drawdown_halt_pct;
            if dd_limit < 1.0 {
                let drawdown = self.current_drawdown();
                if drawdown >= dd_limit {
                    self.state.kill_switch = 1;
                    self.state.trading_paused = 1;
                    let _ = self
                        .db
                        .log_event(
                            "kill_switch",
                            &format!(
                                "Kill switch auto-activated: drawdown {:.1}% >= limit {:.1}%",
                                drawdown * 100.0,
                                dd_limit * 100.0
                            ),
                            Some(serde_json::json!({
                                "drawdown_pct": (drawdown * 10000.0).round() / 100.0,
                                "halt_threshold_pct": dd_limit * 100.0,
                                "trigger": "max_drawdown",
                            })),
                        )
                        .await;
                }
            }
        }
        self.persist().await
    }

    /// Check whether a new position may be opened (system-wide check).
    pub async fn can_open_position(&self, open_count: i64) -> Result<()> {
        if self.state.kill_switch != 0 {
            return Err(BotError::RiskBlocked("Kill switch active".into()));
        }
        // Circuit-breaker timed pause.
        let now = Utc::now().timestamp();
        if self.state.paused_until > now {
            let remaining = self.state.paused_until - now;
            return Err(BotError::RiskBlocked(format!(
                "Circuit breaker active — paused for another {remaining}s (loss streak)"
            )));
        }
        // Lift the trading_paused flag automatically when the cooldown expires.
        if self.state.trading_paused != 0 && self.state.paused_until > 0 && self.state.paused_until <= now {
            // Caller should call lift_circuit_breaker_if_expired for a persistent lift;
            // here we just allow the trade without erroring.
        } else if self.state.trading_paused != 0 {
            return Err(BotError::RiskBlocked("Trading paused".into()));
        }
        if open_count >= self.config.risk.max_concurrent_positions as i64 {
            return Err(BotError::RiskBlocked(format!(
                "Max positions ({}) reached",
                self.config.risk.max_concurrent_positions
            )));
        }
        let equity = self.state.equity.max(1.0);
        if self.state.daily_pnl.abs() / equity >= self.config.risk.daily_loss_limit {
            return Err(BotError::RiskBlocked("Daily loss limit hit".into()));
        }
        Ok(())
    }

    /// Check whether a new position may be opened on a specific symbol.
    /// Returns an error if the symbol is in its per-symbol stop-out cooldown.
    pub fn can_open_symbol(&self, symbol: &str) -> Result<()> {
        let now = Utc::now().timestamp();
        if let Some(&expiry) = self.symbol_cooldowns.get(symbol) {
            if expiry > now {
                let remaining = expiry - now;
                return Err(BotError::RiskBlocked(format!(
                    "{symbol} in stop-out cooldown for another {remaining}s"
                )));
            }
        }
        Ok(())
    }

    /// If the circuit-breaker pause has expired, clear `paused_until` and
    /// resume trading. Call this at the start of each scan cycle.
    pub async fn lift_circuit_breaker_if_expired(&mut self) -> bool {
        if self.state.paused_until == 0 {
            return false;
        }
        let now = Utc::now().timestamp();
        if self.state.paused_until <= now {
            self.state.paused_until = 0;
            // Only lift trading_paused if kill_switch is not also active.
            if self.state.kill_switch == 0 {
                self.state.trading_paused = 0;
            }
            let _ = self.persist().await;
            let _ = self
                .db
                .log_event(
                    "circuit_breaker",
                    "Circuit breaker cooldown expired — trading resumed",
                    None,
                )
                .await;
            return true;
        }
        false
    }

    fn current_drawdown(&self) -> f64 {
        if self.state.peak_equity <= 0.0 {
            return 0.0;
        }
        ((self.state.peak_equity - self.state.equity) / self.state.peak_equity).max(0.0)
    }

    pub async fn sync_from_live_wallet(&mut self, live_equity: f64, force_reanchor: bool) -> Result<bool> {
        if live_equity <= 0.0 {
            return Ok(false);
        }
        roll_pnl_periods(&mut self.state);

        let stale = force_reanchor
            || self.state.last_wallet_equity <= 0.0
            || self.state.peak_equity > live_equity * 2.0
            || (self.state.peak_equity >= 1000.0 && live_equity < self.state.peak_equity * 0.2);

        if stale {
            self.state.equity = live_equity;
            self.state.peak_equity = live_equity;
            self.state.last_wallet_equity = live_equity;
            self.state.paper_pnl_total = 0.0;
            self.state.trading_paused = 0;
            self.state.kill_switch = 0;
            self.state.consecutive_losses = 0;
            self.state.paused_until = 0;
            self.state.equity_source = "live".into();
            self.persist().await?;
            return Ok(true);
        }

        self.state.last_wallet_equity = live_equity;
        self.state.equity = live_equity + self.state.paper_pnl_total;
        self.state.peak_equity = self.state.peak_equity.max(self.state.equity);
        self.state.equity_source = "live".into();
        self.persist().await?;
        Ok(false)
    }

    pub async fn activate_kill_switch(&mut self) -> Result<()> {
        self.state.kill_switch = 1;
        self.state.trading_paused = 1;
        self.persist().await?;
        self.db
            .log_event("kill_switch", "Manual kill switch activated", None)
            .await
    }

    pub async fn deactivate_kill_switch(&mut self) -> Result<()> {
        self.state.kill_switch = 0;
        self.state.trading_paused = 0;
        self.state.consecutive_losses = 0;
        self.state.paused_until = 0;
        self.persist().await?;
        self.db
            .log_event("kill_switch", "Kill switch deactivated", None)
            .await
    }

    pub fn metrics_json(
        &self,
        open_positions: i64,
        open_scalp: i64,
        open_confluence: i64,
        ws_stale: bool,
    ) -> serde_json::Value {
        let equity = self.state.equity.max(1.0);
        let drawdown = if self.state.peak_equity > 0.0 {
            (self.state.peak_equity - self.state.equity) / self.state.peak_equity * 100.0
        } else {
            0.0
        };
        let now = Utc::now().timestamp();
        let circuit_active = self.state.paused_until > now;
        let circuit_remaining = if circuit_active { self.state.paused_until - now } else { 0 };
        serde_json::json!({
            "equity": round2(self.state.equity),
            "peak_equity": round2(self.state.peak_equity),
            "drawdown_pct": round2(drawdown),
            "equity_source": self.state.equity_source,
            "last_wallet_equity": round2(self.state.last_wallet_equity),
            "paper_pnl_total": round2(self.state.paper_pnl_total),
            "daily_pnl": round2(self.state.daily_pnl),
            "daily_pnl_pct": round2(self.state.daily_pnl / equity * 100.0),
            "weekly_pnl": round2(self.state.weekly_pnl),
            "open_positions": open_positions,
            "open_scalp_positions": open_scalp,
            "open_confluence_positions": open_confluence,
            "max_positions": self.config.risk.max_concurrent_positions,
            "max_scalp_positions": 0,
            "max_confluence_positions": self.config.risk.max_concurrent_positions,
            "trading_paused": self.state.trading_paused != 0,
            "kill_switch": self.state.kill_switch != 0,
            "max_risk_per_trade_pct": round2(self.config.risk.max_risk_per_trade * 100.0),
            "paper_trading": true,
            "live_trading": self.config.execution.live_trading_enabled,
            "ml_enabled": self.config.ml.enabled,
            "learning_enabled": self.config.learning.enabled,
            "trading_mode": self.config.trading.mode,
            "scalp_enabled": self.config.scalp.enabled,
            "ws_stale": ws_stale,
            // Circuit-breaker state
            "consecutive_losses": self.state.consecutive_losses,
            "circuit_breaker_active": circuit_active,
            "circuit_breaker_remaining_sec": circuit_remaining,
            "max_drawdown_halt_pct": self.config.risk.max_drawdown_halt_pct * 100.0,
        })
    }

    pub async fn update_pnl(&mut self, delta: f64) -> Result<()> {
        roll_pnl_periods(&mut self.state);
        self.state.paper_pnl_total += delta;
        if self.state.equity_source == "live" && self.state.last_wallet_equity > 0.0 {
            self.state.equity = self.state.last_wallet_equity + self.state.paper_pnl_total;
        } else {
            self.state.equity += delta;
        }
        self.state.daily_pnl += delta;
        self.state.weekly_pnl += delta;
        self.state.peak_equity = self.state.peak_equity.max(self.state.equity);

        let equity = self.state.equity.max(1.0);
        if self.state.daily_pnl.abs() / equity >= self.config.risk.daily_loss_limit {
            self.state.trading_paused = 1;
        }
        self.persist().await
    }

    async fn persist(&mut self) -> Result<()> {
        self.db.save_portfolio_state(&self.state).await
    }

    pub async fn try_open_from_signal(
        &mut self,
        signal: &crate::signals::PumpSignal,
        paper: bool,
    ) -> Result<Option<i64>> {
        let open_count = self.db.count_open_positions().await?;
        if let Err(crate::error::BotError::RiskBlocked(msg)) = self.can_open_position(open_count).await {
            let _ = self
                .db
                .log_event("trade_blocked", &msg, Some(signal.to_payload()))
                .await;
            return Ok(None);
        }

        // Per-symbol cooldown check.
        if let Err(crate::error::BotError::RiskBlocked(msg)) = self.can_open_symbol(&signal.symbol) {
            let _ = self
                .db
                .log_event("trade_blocked", &msg, Some(signal.to_payload()))
                .await;
            return Ok(None);
        }

        if signal.suggested_risk_pct / 100.0 > self.config.risk.max_risk_per_trade {
            let _ = self
                .db
                .log_event(
                    "trade_blocked",
                    "Signal risk exceeds max",
                    Some(signal.to_payload()),
                )
                .await;
            return Ok(None);
        }

        let side = if signal.price_change_pct > 0.0 {
            "long"
        } else {
            "short"
        };

        // Hedge guard: unless explicitly allowed, never open a position that is
        // opposite to one already open on the same symbol.
        if !self.config.risk.allow_hedge {
            let opposite = if side == "long" { "short" } else { "long" };
            if let Ok(Some(_)) = self
                .db
                .get_open_position_by_symbol_side(&signal.symbol, opposite, None)
                .await
            {
                let _ = self
                    .db
                    .log_event(
                        "trade_blocked",
                        &format!(
                            "Hedge blocked: {} already has an open {opposite} position",
                            signal.symbol
                        ),
                        Some(signal.to_payload()),
                    )
                    .await;
                return Ok(None);
            }
        }

        let leverage = signal.suggested_leverage.max(1);
        let equity = self.state.equity.max(1.0);
        let risk_usd = equity * (signal.suggested_risk_pct / 100.0);
        let sl_dist = (signal.last_price - signal.projected_stop_loss).abs().max(signal.last_price * 0.005);
        let mut size = risk_usd / sl_dist;
        let margin = size * signal.last_price / leverage as f64;
        if margin < self.config.risk.min_position_margin_usdt {
            size = self.config.risk.min_position_margin_usdt * leverage as f64 / signal.last_price;
        }
        if size <= 0.0 {
            return Ok(None);
        }

        let id = self
            .db
            .insert_position(
                &signal.symbol,
                side,
                signal.last_price,
                size,
                signal.projected_stop_loss,
                paper,
                &signal.strategy,
                leverage as i64,
                signal.signal_id,
            )
            .await?;
        let _ = self
            .db
            .log_event(
                "position_opened",
                &format!("Opened {side} {} @ {:.6}", signal.symbol, signal.last_price),
                Some(serde_json::json!({ "position_id": id, "strategy": signal.strategy })),
            )
            .await;
        Ok(Some(id))
    }
}

fn utc_today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

fn utc_iso_week() -> String {
    Utc::now().format("%G-W%V").to_string()
}

fn roll_pnl_periods(state: &mut PortfolioState) {
    let today = utc_today();
    let week = utc_iso_week();
    if !state.daily_pnl_date.is_empty() && state.daily_pnl_date != today {
        state.daily_pnl = 0.0;
    }
    state.daily_pnl_date = today;
    if !state.weekly_pnl_iso_week.is_empty() && state.weekly_pnl_iso_week != week {
        state.weekly_pnl = 0.0;
    }
    state.weekly_pnl_iso_week = week;
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use std::path::PathBuf;
    use tempfile::tempdir;

    async fn test_db() -> (Arc<Database>, Arc<AppConfig>) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let cfg_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/settings.yaml");
        std::env::set_var("MEXC_BOT_CONFIG", cfg_path.to_str().unwrap());
        let mut cfg = AppConfig::load().unwrap();
        cfg.storage.sqlite_path = db_path.to_string_lossy().into();
        let db = Arc::new(Database::connect(&cfg.storage.sqlite_path).await.unwrap());
        db.migrate().await.unwrap();
        (db, Arc::new(cfg))
    }

    #[tokio::test]
    async fn reanchor_preserves_daily_pnl() {
        let (db, cfg) = test_db().await;
        let mut rm = RiskManager::new(cfg.clone(), db.clone()).await.unwrap();
        rm.state.daily_pnl = 6.30;
        rm.persist().await.unwrap();

        let reanchored = rm.sync_from_live_wallet(58.27, false).await.unwrap();
        assert!(reanchored);
        assert!((rm.state.daily_pnl - 6.30).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn consecutive_loss_streak_trips_circuit_breaker() {
        let (db, mut cfg) = test_db().await;
        Arc::get_mut(&mut cfg).unwrap().risk.max_consecutive_losses = 3;
        Arc::get_mut(&mut cfg).unwrap().risk.loss_streak_cooldown_sec = 600;
        let mut rm = RiskManager::new(cfg.clone(), db.clone()).await.unwrap();
        rm.state.equity = 1000.0;
        rm.state.peak_equity = 1000.0;

        rm.record_trade_outcome("BTC_USDT", false).await.unwrap();
        rm.record_trade_outcome("BTC_USDT", false).await.unwrap();
        assert_eq!(rm.state.consecutive_losses, 2);
        assert_eq!(rm.state.paused_until, 0); // not yet tripped

        rm.record_trade_outcome("BTC_USDT", false).await.unwrap();
        assert_eq!(rm.state.consecutive_losses, 3);
        assert!(rm.state.paused_until > 0, "circuit breaker should have tripped");

        // Win resets the counter.
        rm.state.paused_until = 0;
        rm.record_trade_outcome("BTC_USDT", true).await.unwrap();
        assert_eq!(rm.state.consecutive_losses, 0);
    }

    #[tokio::test]
    async fn drawdown_halt_activates_kill_switch() {
        let (db, mut cfg) = test_db().await;
        Arc::get_mut(&mut cfg).unwrap().risk.max_drawdown_halt_pct = 0.10;
        Arc::get_mut(&mut cfg).unwrap().risk.max_consecutive_losses = 0; // disable streak breaker
        let mut rm = RiskManager::new(cfg.clone(), db.clone()).await.unwrap();
        rm.state.equity = 900.0;
        rm.state.peak_equity = 1000.0; // 10% drawdown exactly

        rm.record_trade_outcome("ETH_USDT", false).await.unwrap();
        assert_eq!(rm.state.kill_switch, 1, "kill switch should auto-activate at drawdown limit");
    }

    #[tokio::test]
    async fn symbol_cooldown_blocks_reentry() {
        let (db, mut cfg) = test_db().await;
        Arc::get_mut(&mut cfg).unwrap().risk.symbol_loss_cooldown_sec = 900;
        let mut rm = RiskManager::new(cfg.clone(), db.clone()).await.unwrap();
        rm.state.equity = 1000.0;
        rm.state.peak_equity = 1000.0;

        rm.record_trade_outcome("WLD_USDT", false).await.unwrap();
        assert!(
            rm.can_open_symbol("WLD_USDT").is_err(),
            "WLD_USDT should be in cooldown"
        );
        assert!(
            rm.can_open_symbol("BTC_USDT").is_ok(),
            "BTC_USDT should not be affected"
        );
    }

    #[tokio::test]
    async fn circuit_breaker_lifts_after_expiry() {
        let (db, mut cfg) = test_db().await;
        Arc::get_mut(&mut cfg).unwrap().risk.max_consecutive_losses = 1;
        Arc::get_mut(&mut cfg).unwrap().risk.loss_streak_cooldown_sec = 1;
        let mut rm = RiskManager::new(cfg.clone(), db.clone()).await.unwrap();
        rm.state.equity = 1000.0;
        rm.state.peak_equity = 1000.0;

        rm.record_trade_outcome("SOL_USDT", false).await.unwrap();
        assert!(rm.state.paused_until > 0);

        // Manually expire the pause.
        rm.state.paused_until = Utc::now().timestamp() - 1;
        let lifted = rm.lift_circuit_breaker_if_expired().await;
        assert!(lifted, "circuit breaker should lift after expiry");
        assert_eq!(rm.state.trading_paused, 0);
    }
}
