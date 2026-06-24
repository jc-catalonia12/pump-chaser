//! Telegram bot command handler — polls for `/info`, `/start`, `/help` and replies
//! with live app statistics. Only responds to the configured `telegram_chat_id`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::app_state::AppState;
use crate::execution::LiveTrader;
use crate::utils::UserSecrets;

/// Spawn the long-running Telegram getUpdates poller.
pub fn spawn_command_poller(state: Arc<AppState>) {
    tokio::spawn(async move {
        poll_loop(state).await;
    });
}

async fn poll_loop(state: Arc<AppState>) {
    let client = reqwest::Client::new();
    let mut offset: i64 = 0;
    let mut webhook_cleared = false;

    loop {
        let creds = read_telegram_creds(&state.secrets).await;
        let Some((token, chat_id)) = creds else {
            webhook_cleared = false;
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        };

        if !webhook_cleared {
            let _ = client
                .post(format!("https://api.telegram.org/bot{token}/deleteWebhook"))
                .send()
                .await;
            webhook_cleared = true;
            debug!("Telegram command poller active for chat {chat_id}");
        }

        let url = format!(
            "https://api.telegram.org/bot{token}/getUpdates?timeout=25&offset={offset}"
        );
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let body: Value = resp.json().await.unwrap_or(Value::Null);
                if let Some(results) = body.get("result").and_then(|v| v.as_array()) {
                    for update in results {
                        if let Some(id) = update.get("update_id").and_then(|v| v.as_i64()) {
                            offset = id + 1;
                        }
                        handle_update(&state, &client, &token, &chat_id, update).await;
                    }
                }
            }
            Ok(resp) => {
                warn!("Telegram getUpdates HTTP {}", resp.status());
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(exc) => {
                warn!("Telegram getUpdates error: {exc}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn read_telegram_creds(
    secrets: &Arc<RwLock<UserSecrets>>,
) -> Option<(String, String)> {
    let s = secrets.read().await;
    if !s.telegram_enabled || !s.has_telegram() {
        return None;
    }
    Some((s.telegram_bot_token.clone(), s.telegram_chat_id.clone()))
}

async fn handle_update(
    state: &Arc<AppState>,
    client: &reqwest::Client,
    token: &str,
    allowed_chat_id: &str,
    update: &Value,
) {
    let message = match update.get("message") {
        Some(m) => m,
        None => return,
    };
    let chat_id = match message.get("chat").and_then(|c| c.get("id")) {
        Some(id) => id.to_string(),
        None => return,
    };
    if chat_id != allowed_chat_id {
        debug!("Ignoring Telegram message from unauthorized chat {chat_id}");
        return;
    }

    let text = message
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return;
    }

    let cmd = text.split_whitespace().next().unwrap_or("").to_lowercase();
    let reply = match cmd.as_str() {
        "/start" => help_message(),
        "/help" => help_message(),
        "/info" => build_info_message(state).await,
        _ => return,
    };

    if let Err(e) = send_message(client, token, &chat_id, &reply).await {
        warn!("Telegram reply failed: {e}");
    }
}

fn help_message() -> String {
    format!(
        "🤖 <b>MEXC Pump Chaser Bot</b>\n\n\
         Commands:\n\
         • /info — wallet, open positions, scanner &amp; risk stats\n\
         • /help — this message\n\n\
         Trade alerts are sent automatically when notifications are enabled in the app."
    )
}

/// Build an HTML-formatted status snapshot for Telegram.
pub async fn build_info_message(state: &AppState) -> String {
    let secrets = state.secrets.read().await;
    let cfg = state.config.read().await;
    let scanner = state.scanner.read().await;

    let status = scanner.get_status().await;
    let risk = scanner.get_risk_metrics().await;
    let positions = scanner.get_open_positions_live().await;

    let open_n = risk
        .get("open_positions")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let max_pos = risk
        .get("max_positions")
        .and_then(|v| v.as_i64())
        .unwrap_or(3);

    let mut equity = risk
        .get("equity")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let mut available = equity;
    let mut wallet_source = risk
        .get("equity_source")
        .and_then(|v| v.as_str())
        .unwrap_or("internal")
        .to_string();

    if secrets.has_credentials() {
        let live = LiveTrader::new(Arc::new(cfg.clone()), state.db.clone(), secrets.clone());
        if let Ok(balance) = live.get_wallet_balance().await {
            equity = balance.anchor_equity();
            available = balance.available;
            wallet_source = "live".into();
        }
    }

    let mode = if secrets.live_trading && cfg.execution.live_trading_enabled {
        if cfg.execution.dry_run {
            "Live (dry-run)"
        } else {
            "Live"
        }
    } else if secrets.paper_trading {
        "Paper"
    } else {
        "Paper"
    };

    let scanner_running = status
        .get("running")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ws_connected = status
        .get("ws_connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let tracked = status
        .get("tracked_symbols")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let daily_pnl = risk.get("daily_pnl").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let weekly_pnl = risk.get("weekly_pnl").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let drawdown = risk
        .get("drawdown_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let kill_switch = risk
        .get("kill_switch")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let circuit = risk
        .get("circuit_breaker_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let circuit_rem = risk
        .get("circuit_breaker_remaining_sec")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let ws_stale = risk.get("ws_stale").and_then(|v| v.as_bool()).unwrap_or(false);

    let version = crate::version::build_metadata()
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();

    let mut text = format!(
        "🤖 <b>MEXC Pump Chaser</b> v{version}\n\
         <i>{ts}</i>\n\n\
         <b>Mode</b>  {mode}\n\
         <b>Strategy</b>  {strategy}\n\
         <b>Scanner</b>  {scan}\n\
         <b>WebSocket</b>  {ws}\n\
         <b>Symbols</b>  {tracked}\n\n\
         <b>Wallet</b> ({wallet_source})\n\
         • Equity: <code>{equity:.4}</code> USDT\n\
         • Available: <code>{available:.4}</code> USDT\n\n\
         <b>Risk</b>\n\
         • Open positions: {open_n} / {max_pos}\n\
         • Daily PnL: {daily}\n\
         • Weekly PnL: {weekly}\n\
         • Drawdown: {drawdown:.2}%\n\
         • Kill switch: {ks}\n\
         • Circuit breaker: {cb}\n",
        ts = Utc::now().format("%Y-%m-%d %H:%M UTC"),
        strategy = html_escape(&cfg.trading.mode),
        scan = if scanner_running { "Running ✅" } else { "Stopped ⛔" },
        ws = if ws_stale {
            "Stale ⚠️".to_string()
        } else if ws_connected {
            "Connected ✅".to_string()
        } else {
            "Disconnected ⛔".to_string()
        },
        daily = fmt_pnl(daily_pnl),
        weekly = fmt_pnl(weekly_pnl),
        ks = if kill_switch { "ON 🛑" } else { "Off" },
        cb = if circuit {
            format!("Active ({circuit_rem}s left)")
        } else {
            "Off".to_string()
        },
    );

    text.push_str("\n<b>Open Positions</b>\n");
    if positions.is_empty() {
        text.push_str("• None\n");
    } else {
        for pos in &positions {
            let symbol = pos.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
            let side = pos
                .get("side")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_uppercase();
            let entry = pos.get("entry_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let mark = pos.get("mark_price").and_then(|v| v.as_f64()).unwrap_or(entry);
            let upnl = pos.get("unrealized_pnl").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let lev = pos.get("leverage").and_then(|v| v.as_i64()).unwrap_or(1);
            text.push_str(&format!(
                "• <b>{symbol}</b> {side} {lev}x\n  \
                 Entry <code>{entry:.6}</code> → Mark <code>{mark:.6}</code>\n  \
                 uPnL {upnl_s}\n",
                upnl_s = fmt_pnl(upnl),
            ));
        }
    }

    text
}

fn fmt_pnl(v: f64) -> String {
    if v >= 0.0 {
        format!("+<code>{v:.4}</code> USDT")
    } else {
        format!("<code>{v:.4}</code> USDT")
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn send_message(
    client: &reqwest::Client,
    token: &str,
    chat_id: &str,
    text: &str,
) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;

    if resp.status().is_success() {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(body)
    }
}
