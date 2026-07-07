//! Telegram bot command handler — polls for commands and inline keyboard actions.
//! Only responds to the configured `telegram_chat_id`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::app_state::AppState;
use crate::execution::LiveTrader;
use crate::utils::UserSecrets;

const SEP: &str = "──────────────────";

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
    let mut commands_registered = false;

    loop {
        let creds = read_telegram_creds(&state.secrets).await;
        let Some((token, chat_id)) = creds else {
            webhook_cleared = false;
            commands_registered = false;
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

        if !commands_registered {
            if register_bot_commands(&client, &token).await {
                commands_registered = true;
            }
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
                        if update.get("callback_query").is_some() {
                            handle_callback(&state, &client, &token, &chat_id, update).await;
                        } else {
                            handle_message(&state, &client, &token, &chat_id, update).await;
                        }
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

fn chat_id_matches(allowed: &str, actual: &str) -> bool {
    actual == allowed
}

async fn handle_message(
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
    if !chat_id_matches(allowed_chat_id, &chat_id) {
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
    let with_keyboard = matches!(cmd.as_str(), "/start" | "/help" | "/info");

    let reply = match cmd.as_str() {
        "/start" | "/help" => help_message(),
        "/info" => {
            send_typing(client, token, &chat_id).await;
            build_info_message(state).await
        }
        "/run" => {
            send_typing(client, token, &chat_id).await;
            run_scanner(state).await
        }
        "/stop" => stop_scanner(state).await,
        "/sync" => {
            send_typing(client, token, &chat_id).await;
            sync_positions(state).await
        }
        _ => return,
    };

    let markup = if with_keyboard {
        Some(main_keyboard())
    } else {
        None
    };

    if let Err(e) = send_message(client, token, &chat_id, &reply, markup).await {
        warn!("Telegram reply failed: {e}");
    }
}

async fn handle_callback(
    state: &Arc<AppState>,
    client: &reqwest::Client,
    token: &str,
    allowed_chat_id: &str,
    update: &Value,
) {
    let cq = match update.get("callback_query") {
        Some(v) => v,
        None => return,
    };
    let chat_id = cq
        .get("message")
        .and_then(|m| m.get("chat"))
        .and_then(|c| c.get("id"))
        .map(|id| id.to_string())
        .unwrap_or_default();
    if !chat_id_matches(allowed_chat_id, &chat_id) {
        return;
    }

    let data = cq
        .get("data")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let callback_id = cq.get("id").and_then(|v| v.as_str()).unwrap_or("");

    let reply = match data {
        "cmd:info" => {
            send_typing(client, token, &chat_id).await;
            build_info_message(state).await
        }
        "cmd:run" => {
            send_typing(client, token, &chat_id).await;
            run_scanner(state).await
        }
        "cmd:stop" => stop_scanner(state).await,
        "cmd:sync" => {
            send_typing(client, token, &chat_id).await;
            sync_positions(state).await
        }
        "cmd:help" => help_message(),
        _ => return,
    };

    let _ = answer_callback(client, token, callback_id).await;
    if let Err(e) = send_message(client, token, &chat_id, &reply, Some(main_keyboard())).await {
        warn!("Telegram callback reply failed: {e}");
    }
}

fn main_keyboard() -> Value {
    json!({
        "inline_keyboard": [
            [
                { "text": "📊 Status", "callback_data": "cmd:info" },
                { "text": "🔄 Sync", "callback_data": "cmd:sync" }
            ],
            [
                { "text": "▶ Run", "callback_data": "cmd:run" },
                { "text": "⏹ Stop", "callback_data": "cmd:stop" }
            ]
        ]
    })
}

fn help_message() -> String {
    format!(
        "🤖 <b>MEXC Pump Chaser Bot</b>\n\
         {SEP}\n\n\
         <b>Commands</b>\n\
         • /info — wallet, positions, risk\n\
         • /sync — reconcile positions with MEXC\n\
         • /run — start the scanner\n\
         • /stop — stop the scanner\n\
         • /help — this message\n\n\
         Trade alerts are sent automatically when enabled in the app.\n\
         Use the buttons below for quick actions."
    )
}

async fn run_scanner(state: &AppState) -> String {
    let scanner = state.scanner.read().await;
    match scanner.start().await {
        Ok(v) => {
            let running = v.get("running").and_then(|x| x.as_bool()).unwrap_or(false);
            let tracked = v.get("tracked_symbols").and_then(|x| x.as_u64()).unwrap_or(0);
            let status = v.get("status").and_then(|x| x.as_str()).unwrap_or("starting");
            format!(
                "▶ <b>Scanner Started</b>\n{SEP}\n\n\
                 Status: <code>{status}</code>\n\
                 Running: {}\n\
                 Tracked symbols: <code>{tracked}</code>",
                if running { "yes ✅" } else { "no" }
            )
        }
        Err(exc) => format!("⛔ <b>Start failed</b>\n\n<code>{}</code>", html_escape(&exc.to_string())),
    }
}

async fn stop_scanner(state: &AppState) -> String {
    let scanner = state.scanner.read().await;
    let v = scanner.stop().await;
    let running = v.get("running").and_then(|x| x.as_bool()).unwrap_or(false);
    format!(
        "⏹ <b>Scanner Stopped</b>\n{SEP}\n\n\
         Running: {}",
        if running { "yes" } else { "no ⛔" }
    )
}

async fn sync_positions(state: &AppState) -> String {
    let scanner = state.scanner.read().await;
    let result = scanner.sync_exchange_positions().await;
    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        return format!("⛔ <b>Sync failed</b>\n\n<code>{}</code>", html_escape(err));
    }
    let exchange_count = result
        .get("exchange_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let imported = result.get("imported").and_then(|v| v.as_u64()).unwrap_or(0);
    let updated = result.get("updated").and_then(|v| v.as_u64()).unwrap_or(0);
    let closed = result.get("closed").and_then(|v| v.as_u64()).unwrap_or(0);
    let healed = result.get("healed").and_then(|v| v.as_u64()).unwrap_or(0);
    let linked = result.get("linked").and_then(|v| v.as_u64()).unwrap_or(0);
    let msg = result
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Sync complete");

    let mut text = format!(
        "🔄 <b>Exchange Sync</b>\n{SEP}\n\n\
         On exchange: <code>{exchange_count}</code>\n\
         Imported: <code>{imported}</code> · Updated: <code>{updated}</code>\n\
         Closed locally: <code>{closed}</code> · Healed: <code>{healed}</code>\n\
         Linked: <code>{linked}</code>\n\n\
         <i>{msg}</i>"
    );

    if let Some(symbols) = result.get("closed_symbols").and_then(|v| v.as_array()) {
        if !symbols.is_empty() {
            let list: Vec<String> = symbols
                .iter()
                .filter_map(|s| s.as_str().map(|x| x.to_string()))
                .collect();
            text.push_str(&format!("\n\n<b>Closed symbols</b>\n<code>{}</code>", list.join(", ")));
        }
    }
    text
}

/// Build an HTML-formatted status snapshot for Telegram.
pub async fn build_info_message(state: &AppState) -> String {
    let secrets = state.secrets.read().await;
    let cfg = state.config.read().unwrap().clone();
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
        let live = LiveTrader::new(state.config.clone(), state.db.clone(), secrets.clone());
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
         <i>{}</i>\n{SEP}\n\n\
         <b>Mode</b>       {mode}\n\
         <b>Strategy</b>   {strategy}\n\
         <b>Scanner</b>    {scan}\n\
         <b>WebSocket</b>  {ws}\n\
         <b>Symbols</b>    <code>{tracked}</code>\n\n\
         <b>Wallet</b> ({wallet_source})\n\
         Equity     <code>{equity:.4}</code> USDT\n\
         Available  <code>{available:.4}</code> USDT\n\n\
         <b>Risk</b>\n\
         Positions  <code>{open_n}</code> / <code>{max_pos}</code>\n\
         Daily PnL  {daily}\n\
         Weekly PnL {weekly}\n\
         Drawdown   <code>{drawdown:.2}%</code>\n\
         Kill SW    {ks}\n\
         Circuit    {cb}\n",
        Utc::now().format("%Y-%m-%d %H:%M UTC"),
        strategy = html_escape(&cfg.trading.mode),
        scan = if scanner_running {
            "Running ✅"
        } else {
            "Stopped ⛔"
        },
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
            format!("Active ({circuit_rem}s)")
        } else {
            "Off".to_string()
        },
    );

    text.push_str(&format!("\n{SEP}\n<b>Open Positions</b>\n"));
    if positions.is_empty() {
        text.push_str("<i>None</i>\n");
    } else {
        text.push_str("<pre>");
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
            let strategy = pos
                .get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            text.push_str(&format!(
                "{symbol:<12} {side:5} {lev:>3}x\n  entry {entry:.6}  mark {mark:.6}\n  uPnL {upnl:+.4}  [{strategy}]\n\n"
            ));
        }
        text.push_str("</pre>");
    }
    text
}

fn fmt_pnl(v: f64) -> String {
    if v >= 0.0 {
        format!("<code>+{v:.4}</code> USDT")
    } else {
        format!("<code>{v:.4}</code> USDT")
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn register_bot_commands(client: &reqwest::Client, token: &str) -> bool {
    let url = format!("https://api.telegram.org/bot{token}/setMyCommands");
    let body = json!({
        "commands": [
            { "command": "info", "description": "Wallet, positions, risk" },
            { "command": "sync", "description": "Reconcile positions with MEXC" },
            { "command": "run", "description": "Start scanner" },
            { "command": "stop", "description": "Stop scanner" },
            { "command": "help", "description": "Command list" }
        ]
    });
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            warn!("setMyCommands HTTP {}", resp.status());
            false
        }
        Err(exc) => {
            warn!("setMyCommands error: {exc}");
            false
        }
    }
}

async fn send_typing(client: &reqwest::Client, token: &str, chat_id: &str) {
    let url = format!("https://api.telegram.org/bot{token}/sendChatAction");
    let _ = client
        .post(&url)
        .json(&json!({ "chat_id": chat_id, "action": "typing" }))
        .send()
        .await;
}

async fn answer_callback(client: &reqwest::Client, token: &str, callback_id: &str) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{token}/answerCallbackQuery");
    let resp = client
        .post(&url)
        .json(&json!({ "callback_query_id": callback_id }))
        .send()
        .await
        .map_err(|e| format!("Network error: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(resp.text().await.unwrap_or_default())
    }
}

async fn send_message(
    client: &reqwest::Client,
    token: &str,
    chat_id: &str,
    text: &str,
    reply_markup: Option<Value>,
) -> Result<(), String> {
    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let mut body = json!({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": true,
    });
    if let Some(markup) = reply_markup {
        body["reply_markup"] = markup;
    }
    let resp = client
        .post(&url)
        .json(&body)
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
