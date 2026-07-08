//! Persist user settings patches — shared by the REST API and the assistant.

use serde_json::{json, Value};

use crate::error::Result;
use crate::user_settings::{apply_user_settings, save_app_config, settings_file_path, user_settings_values};
use crate::AppState;

pub async fn commit_user_settings_patch(state: &AppState, patch: &Value) -> Result<Value> {
    let (prev_mexc, prev_paper_equity) = {
        let cfg = state.config.read().unwrap().clone();
        (cfg.mexc.clone(), cfg.execution.paper_initial_equity)
    };
    let mut updated = state.config.read().unwrap().clone();
    apply_user_settings(&mut updated, patch)?;
    save_app_config(&updated)?;
    {
        let mut cfg = state.config.write().unwrap();
        *cfg = updated.clone();
    }
    {
        let scanner = state.scanner.read().await;
        scanner.on_config_updated(prev_mexc).await;
    }

    let new_paper_equity = updated.execution.paper_initial_equity;
    let paper_equity_applied = if (new_paper_equity - prev_paper_equity).abs() > 0.001 {
        let secrets = state.secrets.read().await;
        let paper_mode = secrets.paper_trading || !updated.execution.live_trading_enabled;
        drop(secrets);
        if paper_mode {
            let open = state.db.count_open_positions().await.unwrap_or(0);
            if open == 0 {
                let mut risk = state.risk.write().await;
                match risk.reset_paper_equity(new_paper_equity).await {
                    Ok(()) => true,
                    Err(exc) => {
                        tracing::warn!(error = %exc, "failed to apply paper starting equity after settings save");
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    Ok(json!({
        "ok": true,
        "config_path": settings_file_path().display().to_string(),
        "values": user_settings_values(&updated),
        "live_trading_enabled": updated.execution.live_trading_enabled,
        "applied_live": true,
        "paper_equity_applied": paper_equity_applied,
        "paper_equity_blocked_open_positions": !paper_equity_applied
            && (new_paper_equity - prev_paper_equity).abs() > 0.001,
        "scanner_restart_recommended": false,
    }))
}
