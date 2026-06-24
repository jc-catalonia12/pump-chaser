//! Lightweight Telegram / webhook alerter for critical bot events.
//!
//! Two alert channels are supported:
//!   - **Telegram** via `UserSecrets` (configured in the Account tab — bot token + chat id)
//!   - **Webhook fallback** via `AlertsConfig` in `settings.yaml` (Discord or legacy Telegram URL)
//!
//! Trade events (`position_opened`, `position_closed`, `tp_hit`, `cut_loss`) are sent
//! through the Telegram channel when it is connected and the event is in the user's
//! `telegram_events` list.  The webhook fallback is used for kill-switch / circuit-breaker
//! style system alerts.
//!
//! Alerts are fire-and-forget; failures are logged as warnings and never surface as errors
//! to the caller.  A per-event-type minimum interval prevents alert floods.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::config::AlertsConfig;
use crate::utils::secrets::UserSecrets;

fn json_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|n| n as f64))
        .or_else(|| v.as_u64().map(|n| n as f64))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// Returns `(icon, human label)` for an event key.
fn event_label(event_key: &str) -> (&'static str, &'static str) {
    match event_key {
        "position_opened" => ("🟢", "Position Opened"),
        "position_closed" => ("🔴", "Position Closed"),
        "tp_hit"          => ("🎯", "Take Profit Hit"),
        "cut_loss"        => ("✂️", "Cut Loss"),
        "kill_switch"     => ("🛑", "Kill Switch Activated"),
        "ws_stale"        => ("⚠️", "Feed Warning"),
        _                 => ("📢", "Alert"),
    }
}

/// Human-readable label for a strategy id (`confluence`, `volume_pump`, …).
fn fmt_strategy(strategy: &str) -> String {
    match strategy {
        "confluence" => "Confluence".into(),
        "volume_pump" => "Volume Pump".into(),
        "scalp" => "Scalp".into(),
        other => other
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Human-readable label for an exit/close reason string.
fn fmt_reason(reason: &str) -> String {
    if reason.starts_with("take_profit_l") {
        let n = reason.trim_start_matches("take_profit_l");
        return format!("Take Profit L{n}");
    }
    match reason {
        "stop_loss"                 => "Stop Loss".into(),
        "trailing_stop"             => "Trailing Stop".into(),
        "take_profit"               => "Take Profit".into(),
        "exchange_closed"           => "Exchange Closed".into(),
        "manual" | "manual_close"   => "Manual Close".into(),
        "kill_switch"               => "Kill Switch".into(),
        "expired" | "timeout"       => "Expired (Max Hold)".into(),
        "cut_loss"                  => "Cut Loss".into(),
        other => other
            .split('_')
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

const SEP: &str = "──────────────────";

/// Build the full Telegram HTML message for a trade event.
fn build_trade_message(event_key: &str, details: Option<&Value>) -> String {
    let (icon, label) = event_label(event_key);
    let Some(d) = details else {
        return format!("{icon} <b>{label}</b>\n\n<i>MEXC Pump Chaser</i>");
    };

    let sym  = d.get("symbol").and_then(|v| v.as_str()).unwrap_or("—");
    let side = d.get("side").and_then(|v| v.as_str()).map(|s| s.to_uppercase());
    let lev  = json_f64(d.get("leverage").unwrap_or(&Value::Null)).map(|l| format!("  {l:.0}×"));
    let mode = d.get("mode").and_then(|v| v.as_str());

    // ── Header ────────────────────────────────────────────────────────────
    let mut lines: Vec<String> = vec![
        format!("{icon} <b>{label}</b>"),
        SEP.into(),
    ];

    // Symbol / side / leverage / mode badge
    let mut sym_line = format!("<b>{sym}</b>");
    if let Some(s) = &side  { sym_line.push_str(&format!("  {s}")); }
    if let Some(l) = &lev   { sym_line.push_str(l); }
    if let Some(m) = mode {
        let badge = if m.to_lowercase() == "paper" { "  📋 PAPER" } else { "  🔴 LIVE" };
        sym_line.push_str(badge);
    }
    lines.push(sym_line);

    let strategy = d
        .get("strategy")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            d.get("payload")
                .and_then(|p| p.get("strategy"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        });
    if let Some(s) = strategy {
        lines.push(format!("🧠 <b>Strategy</b>  {}", fmt_strategy(s)));
    }
    lines.push(String::new());

    // ── Price fields ──────────────────────────────────────────────────────
    if let Some(ep) = json_f64(d.get("entry_price").unwrap_or(&Value::Null)) {
        if ep > 0.0 {
            lines.push(format!("📍 <b>Entry</b>    <code>{ep:.6}</code>"));
        }
    }

    if let Some(xp) = json_f64(d.get("exit_price").unwrap_or(&Value::Null)) {
        if xp > 0.0 {
            let exit_icon_label = match event_key {
                "tp_hit"   => "🎯 <b>TP Exit</b> ",
                "cut_loss" => "🚫 <b>SL Exit</b> ",
                _          => "🏁 <b>Exit</b>    ",
            };
            lines.push(format!("{exit_icon_label} <code>{xp:.6}</code>"));
        }
    }

    // ── PnL ───────────────────────────────────────────────────────────────
    if let Some(pnl) = json_f64(d.get("pnl").unwrap_or(&Value::Null)) {
        let sign     = if pnl >= 0.0 { "+" } else { "" };
        let pnl_icon = if pnl >= 0.0 { "💰" } else { "💸" };
        lines.push(format!("{pnl_icon} <b>PnL</b>     <code>{sign}{pnl:.4} USDT</code>"));
    }

    // ── Open-only fields ──────────────────────────────────────────────────
    if let Some(margin) = json_f64(d.get("margin_usdt").unwrap_or(&Value::Null)) {
        if margin > 0.0 {
            lines.push(format!("💰 <b>Margin</b>   <code>{margin:.2} USDT</code>"));
        }
    }

    if let Some(sz) = json_f64(d.get("size").unwrap_or(&Value::Null)) {
        if sz > 0.0 {
            lines.push(format!("📦 <b>Size</b>     <code>{sz}</code>"));
        }
    }

    // ── TP-specific ───────────────────────────────────────────────────────
    if let Some(tp_pct) = json_f64(d.get("tp_pct").unwrap_or(&Value::Null)) {
        if tp_pct > 0.0 {
            lines.push(format!("📈 <b>TP Move</b>  <code>+{tp_pct:.2}%</code>"));
        }
    }

    // ── Reason footer ─────────────────────────────────────────────────────
    if let Some(reason) = d.get("reason").and_then(|v| v.as_str()) {
        if !reason.is_empty() {
            lines.push(SEP.into());
            lines.push(format!("Reason: {}", fmt_reason(reason)));
        }
    }

    lines.push(String::new());
    lines.push("<i>MEXC Pump Chaser</i>".into());

    lines.join("\n")
}

pub struct Alerter {
    config: AlertsConfig,
    /// Tracks the last fire time (unix ts) per event-type key.
    last_sent: Mutex<HashMap<String, i64>>,
    /// Live reference to user secrets so Telegram credentials are always current.
    secrets: Option<Arc<RwLock<UserSecrets>>>,
}

impl Alerter {
    pub fn new(config: AlertsConfig) -> Self {
        Self {
            config,
            last_sent: Mutex::new(HashMap::new()),
            secrets: None,
        }
    }

    /// Attach a live secrets handle so the alerter always uses the latest
    /// Telegram credentials without needing a restart.
    pub fn with_secrets(mut self, secrets: Arc<RwLock<UserSecrets>>) -> Self {
        self.secrets = Some(secrets);
        self
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    fn is_rate_limited(&self, event_key: &str, interval_sec: u64) -> bool {
        let mut last = self.last_sent.lock().unwrap();
        let now = Utc::now().timestamp();
        if let Some(&prev) = last.get(event_key) {
            if now - prev < interval_sec as i64 {
                return true;
            }
        }
        last.insert(event_key.to_string(), now);
        false
    }

    /// Build a per-trade rate-limit key so multiple symbols (or TP levels) in quick
    /// succession are not collapsed into a single Telegram message.
    fn trade_event_rate_key(event_key: &str, details: Option<&Value>) -> String {
        let mut key = format!("tg:{event_key}");
        let Some(d) = details else {
            return key;
        };
        if let Some(sym) = d.get("symbol").and_then(|v| v.as_str()) {
            key.push(':');
            key.push_str(sym);
        }
        if event_key == "tp_hit" {
            if let Some(reason) = d.get("reason").and_then(|v| v.as_str()) {
                key.push(':');
                key.push_str(reason);
            }
        }
        if let Some(id) = d.get("position_id").and_then(|v| v.as_i64()) {
            key.push_str(&format!(":id{id}"));
        }
        key
    }

    async fn send_telegram(&self, token: &str, chat_id: &str, text: &str, event_key: &str) {
        let url = format!("https://api.telegram.org/bot{token}/sendMessage");
        let client = reqwest::Client::new();
        match client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": text,
                "parse_mode": "HTML",
            }))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => warn!("Telegram alert HTTP {}: {event_key}", resp.status()),
            Err(e) => warn!("Telegram alert error for {event_key}: {e}"),
        }
    }

    async fn send_discord(&self, url: &str, text: &str, event_key: &str) {
        let client = reqwest::Client::new();
        match client
            .post(url)
            .json(&serde_json::json!({ "content": text }))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => warn!("Discord alert HTTP {}: {event_key}", resp.status()),
            Err(e) => warn!("Discord alert error for {event_key}: {e}"),
        }
    }

    // -------------------------------------------------------------------------
    // Public API
    // -------------------------------------------------------------------------

    /// Fire a system alert (kill-switch, circuit-breaker, ws_stale, …).
    /// Uses the `AlertsConfig` webhook URL from `settings.yaml`.
    pub async fn fire(&self, event_key: &str, message: &str) {
        if !self.config.enabled {
            return;
        }
        let url = match self.config.webhook_url.as_deref() {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => return,
        };
        if self.is_rate_limited(event_key, self.config.min_interval_sec) {
            return;
        }

        let is_telegram = url.contains("api.telegram.org");
        if is_telegram {
            let chat_id = match self.config.telegram_chat_id.as_deref() {
                Some(c) if !c.is_empty() => c.to_string(),
                _ => {
                    warn!("Telegram alert skipped — telegram_chat_id not set in config");
                    return;
                }
            };
            // Legacy URL style: extract token from the URL path segment.
            let token = url
                .trim_start_matches("https://api.telegram.org/bot")
                .split('/')
                .next()
                .unwrap_or("")
                .to_string();
            let (icon, label) = event_label(event_key);
            let text = format!("{icon} <b>MEXC Pump Chaser</b> — <b>{label}</b>\n\n{message}\n\n<i>MEXC Pump Chaser</i>");
            self.send_telegram(&token, &chat_id, &text, event_key).await;
        } else {
            let text = format!("**MEXC Bot — {event_key}**\n{message}");
            self.send_discord(&url, &text, event_key).await;
        }
    }

    /// Fire a trade-event Telegram notification using the user's saved credentials.
    ///
    /// `event_key` — one of `position_opened`, `position_closed`, `tp_hit`, `cut_loss`, etc.
    /// `details`   — optional extra JSON payload used to build a richer message.
    pub async fn trade_event(&self, event_key: &str, _message: &str, details: Option<&Value>) {
        let secrets_arc = match &self.secrets {
            Some(s) => s.clone(),
            None => return,
        };
        let secrets = secrets_arc.read().await;
        if !secrets.telegram_enabled || !secrets.has_telegram() {
            return;
        }
        // Check the event filter list (empty = all events pass).
        if !secrets.telegram_events.is_empty()
            && !secrets
                .telegram_events
                .iter()
                .any(|e| e == event_key || e == "all")
        {
            return;
        }

        let token = secrets.telegram_bot_token.clone();
        let chat_id = secrets.telegram_chat_id.clone();
        drop(secrets); // release the read lock before await

        let rate_key = Self::trade_event_rate_key(event_key, details);
        // 30-second minimum between identical trade alerts (per symbol / TP level).
        if self.is_rate_limited(&rate_key, 30) {
            debug!("Telegram {event_key} skipped (rate limit): {rate_key}");
            return;
        }

        let text = build_trade_message(event_key, details);
        self.send_telegram(&token, &chat_id, &text, event_key).await;
    }

    /// Test the Telegram connection using explicit credentials (not the secrets store).
    /// Returns `Ok(())` on success or `Err(human-readable description)` on failure.
    pub async fn test_telegram(token: &str, chat_id: &str) -> Result<(), String> {
        let url = format!("https://api.telegram.org/bot{token}/sendMessage");
        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": "🤖 <b>MEXC Pump Chaser</b> — Telegram connected successfully! ✅",
                "parse_mode": "HTML",
            }))
            .send()
            .await
            .map_err(|e| format!("Network error: {e}"))?;

        if resp.status().is_success() {
            return Ok(());
        }

        let status = resp.status();
        // Try to parse Telegram's JSON error body: { "ok": false, "error_code": 401, "description": "..." }
        let body = resp.text().await.unwrap_or_default();
        let description = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("description").and_then(|d| d.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| body.clone());

        Err(format!("HTTP {status} — {description}"))
    }

    /// Fetch bot display name and @username from Telegram `getMe`.
    pub async fn fetch_bot_info(token: &str) -> Result<(String, String), String> {
        let url = format!("https://api.telegram.org/bot{token}/getMe");
        let client = reqwest::Client::new();
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Network error: {e}"))?;

        let body = resp.text().await.unwrap_or_default();
        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| format!("Invalid Telegram response: {e}"))?;

        if !parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
            let description = parsed
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("Unknown error");
            return Err(description.to_string());
        }

        let result = parsed
            .get("result")
            .ok_or_else(|| "Missing bot info in Telegram response".to_string())?;
        let username = result
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let first_name = result
            .get("first_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if username.is_empty() {
            return Err("Bot has no username — set one via @BotFather".to_string());
        }

        Ok((username, first_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fmt_strategy_labels() {
        assert_eq!(fmt_strategy("confluence"), "Confluence");
        assert_eq!(fmt_strategy("volume_pump"), "Volume Pump");
        assert_eq!(fmt_strategy("scalp"), "Scalp");
    }

    #[test]
    fn trade_message_includes_strategy() {
        let details = json!({
            "symbol": "BILL_USDT",
            "side": "LONG",
            "strategy": "volume_pump",
            "entry_price": 0.00123,
            "margin_usdt": 5.0,
        });
        let text = build_trade_message("position_opened", Some(&details));
        assert!(text.contains("Volume Pump"));
        assert!(text.contains("Strategy"));
    }

    #[test]
    fn trade_event_rate_key_is_per_symbol() {
        let beat = json!({ "symbol": "BEAT_USDT" });
        let wld = json!({ "symbol": "WLD_USDT" });
        assert_ne!(
            Alerter::trade_event_rate_key("position_opened", Some(&beat)),
            Alerter::trade_event_rate_key("position_opened", Some(&wld)),
        );
    }

    #[test]
    fn trade_event_rate_key_tp_includes_reason() {
        let tp1 = json!({ "symbol": "WLD_USDT", "reason": "take_profit_l1" });
        let tp2 = json!({ "symbol": "WLD_USDT", "reason": "take_profit_l2" });
        assert_ne!(
            Alerter::trade_event_rate_key("tp_hit", Some(&tp1)),
            Alerter::trade_event_rate_key("tp_hit", Some(&tp2)),
        );
    }
}
