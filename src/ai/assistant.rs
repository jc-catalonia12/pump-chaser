//! In-app assistant — answers questions about bot status and can apply settings via Ollama.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::config::SharedAppConfig;
use crate::execution::LiveTrader;
use crate::user_settings::{apply_config_patch, settings_prompt_block};
use crate::AppState;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct AssistantChatRequest {
    pub message: String,
    #[serde(default)]
    pub history: Vec<ChatMessage>,
}

#[derive(Debug, Serialize)]
pub struct AssistantChatResponse {
    pub reply: String,
    pub ollama_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings_applied: Option<Value>,
}

/// Plain-text snapshot of current bot state for the LLM system prompt.
pub async fn build_bot_context(state: &AppState) -> String {
    let scanner = state.scanner.read().await;
    let secrets = state.secrets.read().await;
    let cfg = state.config.read().unwrap().clone();

    let status = scanner.get_status().await;
    let risk = scanner.get_risk_metrics().await;
    let positions = scanner.get_open_positions_live().await;
    let sentiment = scanner.sentiment_status().await;
    let llm = scanner.llm_regime_status().await;
    let learning = state.db.promotion_metrics().await.unwrap_or(json!({}));

    let open_n = risk
        .get("open_positions")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let max_pos = cfg.risk.max_concurrent_positions;
    let equity = risk.get("equity").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let daily_pnl = risk.get("daily_pnl").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let weekly_pnl = risk.get("weekly_pnl").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let drawdown = risk
        .get("drawdown_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let kill = risk.get("kill_switch").and_then(|v| v.as_bool()).unwrap_or(false);
    let paused = risk
        .get("trading_paused")
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
    let scanner_on = status.get("running").and_then(|v| v.as_bool()).unwrap_or(false);
    let ws_on = status
        .get("ws_connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let tracked = status
        .get("tracked_symbols")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mode = if secrets.live_trading && cfg.execution.live_trading_enabled {
        if cfg.execution.dry_run {
            "live (dry-run)"
        } else {
            "live"
        }
    } else {
        "paper"
    };

    let global_sent = sentiment
        .get("global_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let fear_greed = sentiment.get("fear_greed").and_then(|v| v.as_i64());
    let headline_count = sentiment
        .get("headline_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let regime = llm.get("regime").cloned().unwrap_or(json!({}));
    let llm_available = llm.get("available").and_then(|v| v.as_bool()).unwrap_or(false);

    let promo_wr = learning
        .get("win_rate")
        .and_then(|v| v.as_f64())
        .map(|w| format!("{:.1}%", w * 100.0))
        .unwrap_or_else(|| "—".into());
    let promo_trades = learning
        .get("total_trades")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let meta = crate::version::build_metadata();
    let version = meta
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();

    let mut lines = vec![
        format!("Bot: MEXC Pump Chaser v{version}"),
        format!("Time (UTC): {}", chrono::Utc::now().format("%Y-%m-%d %H:%M")),
        format!("Trading mode: {mode}"),
        format!("Strategy: {}", cfg.trading.mode),
        format!(
            "Scanner: {} | WebSocket: {}{}",
            if scanner_on { "running" } else { "stopped" },
            if ws_on { "connected" } else { "disconnected" },
            if ws_stale { " (stale)" } else { "" }
        ),
        format!("Tracked symbols: {tracked}"),
        format!("Equity: {equity:.4} USDT"),
        format!("Daily PnL: {daily_pnl:+.4} | Weekly PnL: {weekly_pnl:+.4}"),
        format!("Drawdown: {drawdown:.2}%"),
        format!(
            "Open positions: {open_n}/{max_pos} | Kill switch: {} | Trading paused: {} | Circuit breaker: {}{}",
            if kill { "ON" } else { "off" },
            if paused { "yes" } else { "no" },
            if circuit { "active" } else { "off" },
            if circuit { format!(" ({circuit_rem}s left)") } else { String::new() }
        ),
        format!(
            "Sentiment: news {:.2}, fear/greed {:?}, {headline_count} headlines",
            global_sent, fear_greed
        ),
        format!(
            "LLM regime: available={llm_available}, trending={}, chop={}, bias={:.2}",
            regime.get("trending").and_then(|v| v.as_bool()).unwrap_or(false),
            regime.get("chop").and_then(|v| v.as_bool()).unwrap_or(false),
            regime
                .get("btc_bias")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
        ),
        format!("Live promotion stats: {promo_trades} trades, win rate {promo_wr}"),
    ];

    if positions.is_empty() {
        lines.push("Open positions detail: none".into());
    } else {
        lines.push("Open positions:".into());
        for pos in positions.iter().take(12) {
            let symbol = pos.get("symbol").and_then(|v| v.as_str()).unwrap_or("?");
            let side = pos
                .get("side")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_uppercase();
            let upnl = pos.get("unrealized_pnl").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let lev = pos.get("leverage").and_then(|v| v.as_i64()).unwrap_or(1);
            lines.push(format!("  - {symbol} {side} {lev}x uPnL {upnl:+.4} USDT"));
        }
    }

    if secrets.has_credentials() {
        let live = LiveTrader::new(state.config.clone(), state.db.clone(), secrets.clone());
        if live.is_live() {
            if let Ok(balance) = live.get_wallet_balance().await {
                lines.push(format!(
                    "Live wallet: equity {:.4} USDT, available {:.4} USDT",
                    balance.anchor_equity(),
                    balance.available
                ));
            }
        }
    }

    lines.join("\n")
}

pub async fn chat(
    state: Arc<AppState>,
    config: SharedAppConfig,
    req: AssistantChatRequest,
) -> AssistantChatResponse {
    let message = req.message.trim();
    if message.is_empty() {
        return AssistantChatResponse {
            reply: "Ask me about scanner status, risk, positions, training — or tell me to change a setting (e.g. \"set max positions to 5\").".into(),
            ollama_available: false,
            settings_applied: None,
        };
    }

    let cfg_snap = config.read().unwrap().clone();
    let llm_cfg = cfg_snap.llm.clone();
    let context = build_bot_context(state.as_ref()).await;
    let settings_block = settings_prompt_block(&cfg_snap);

    let system = format!(
        "You are Pump Chaser Bot Assistant — a helpful co-pilot for the MEXC futures trading bot. \
Answer concisely in plain English (2–6 sentences unless the user asks for detail). \
Use ONLY the live bot snapshot below for factual status; say if data is missing. \
You can explain scanner/risk state, trades blocked, positions, PnL, sentiment, ML/training, \
and how to resume trading (Start button, reset circuit breaker). \
Do not invent trades or prices not in the snapshot.\n\n\
SETTINGS: When the user asks to change, enable, disable, or tune a bot setting, you MAY apply it. \
Use the editable settings list below for exact JSON keys and valid ranges. \
Percent fields use decimals (0.03 = 3%). \
If you apply a change, end your reply with a single line (no markdown): \
SETTINGS_PATCH: {{\"section\":{{\"field\":value}}}} using only keys being changed. \
Confirm what changed in plain English before that line. \
If the request is unclear, unsafe, or outside the editable list, explain and do NOT emit SETTINGS_PATCH. \
Never enable live trading unless the user explicitly asks.\n\n\
--- LIVE SNAPSHOT ---\n{context}\n--- END ---\n\n\
--- EDITABLE SETTINGS ---\n{settings_block}\n--- END ---"
    );

    let mut messages: Vec<Value> = vec![json!({ "role": "system", "content": system })];
    for h in req.history.iter().rev().take(8).rev() {
        let role = h.role.as_str();
        if role != "user" && role != "assistant" {
            continue;
        }
        let content = h.content.trim();
        if content.is_empty() {
            continue;
        }
        messages.push(json!({ "role": role, "content": content }));
    }
    messages.push(json!({ "role": "user", "content": message }));

    let mut settings_applied = None;
    let mut reply = if !llm_cfg.enabled {
        offline_fallback(&context, message)
    } else {
        match call_ollama(&llm_cfg, &messages).await {
            Ok(text) => text,
            Err(exc) => {
                warn!("Assistant Ollama unreachable: {exc}");
                format!(
                    "Ollama is offline ({exc}). {}",
                    offline_fallback(&context, message)
                )
            }
        }
    };

    let ollama_available = llm_cfg.enabled && !reply.starts_with("Ollama is offline");

    if let Some(patch) = extract_settings_patch(&reply) {
        let apply_result = apply_config_patch(state.as_ref(), patch).await;
        settings_applied = Some(apply_result.clone());
        reply = strip_settings_patch(&reply);
        if let Some(err) = apply_result.get("error").and_then(|v| v.as_str()) {
            reply.push_str(&format!("\n\n⚠️ Settings could not be saved: {err}"));
        } else if apply_result.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            reply.push_str("\n\n✓ Settings saved and applied.");
            if apply_result
                .get("scanner_restart_recommended")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                reply.push_str(" Restart the scanner for MEXC URL changes to take effect.");
            }
        }
    } else if !llm_cfg.enabled {
        if let Some(patch) = infer_settings_patch_from_message(message) {
            let apply_result = apply_config_patch(state.as_ref(), patch).await;
            settings_applied = Some(apply_result.clone());
            if let Some(err) = apply_result.get("error").and_then(|v| v.as_str()) {
                reply.push_str(&format!("\n\n⚠️ Settings could not be saved: {err}"));
            } else if apply_result.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                reply.push_str("\n\n✓ Settings saved (offline mode — limited parsing).");
            }
        }
    }

    AssistantChatResponse {
        reply: reply.trim().to_string(),
        ollama_available,
        settings_applied,
    }
}

async fn call_ollama(
    cfg: &crate::config::LlmConfig,
    messages: &[Value],
) -> Result<String, String> {
    let url = format!("{}/api/chat", cfg.base_url.trim_end_matches('/'));
    let body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": false,
        "options": { "temperature": 0.35, "num_predict": 640 },
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(cfg.timeout_sec.max(10).min(120)))
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "empty reply".into())
}

/// Parse `SETTINGS_PATCH: {...}` from the model reply.
fn extract_settings_patch(text: &str) -> Option<Value> {
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("SETTINGS_PATCH:") {
            let json_str = rest.trim();
            if let Ok(v) = serde_json::from_str::<Value>(json_str) {
                if v.is_object() {
                    return Some(v);
                }
            }
            if let Some(obj) = extract_balanced_json(json_str) {
                return serde_json::from_str(&obj).ok();
            }
        }
    }
    if let Some(start) = text.find("```settings") {
        let rest = &text[start + "```settings".len()..];
        let json_part = rest.trim_start_matches('\n').trim_start();
        let end = json_part.find("```").unwrap_or(json_part.len());
        let json_str = json_part[..end].trim();
        if let Ok(v) = serde_json::from_str::<Value>(json_str) {
            if v.is_object() {
                return Some(v);
            }
        }
    }
    None
}

fn strip_settings_patch(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        if line.trim().starts_with("SETTINGS_PATCH:") {
            continue;
        }
        if line.trim().starts_with("```settings") {
            in_block = true;
            continue;
        }
        if in_block {
            if line.trim() == "```" {
                in_block = false;
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

fn extract_balanced_json(s: &str) -> Option<String> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(s[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Rule-based fallback when Ollama is disabled.
fn infer_settings_patch_from_message(message: &str) -> Option<Value> {
    let m = message.to_lowercase();
    if let Some(n) = extract_usize_after(
        &m,
        &[
            "max positions",
            "max concurrent positions",
            "position limit",
            "concurrent positions",
        ],
    ) {
        return Some(json!({ "risk": { "max_concurrent_positions": n } }));
    }
    if let Some(n) = extract_usize_after(&m, &["max leverage", "leverage cap", "leverage limit"]) {
        return Some(json!({ "risk": { "max_leverage": n } }));
    }
    if let Some(pct) = extract_percent_after(
        &m,
        &["daily loss limit", "daily loss", "loss limit"],
    ) {
        return Some(json!({ "risk": { "daily_loss_limit": pct } }));
    }
    if let Some(pct) = extract_percent_after(&m, &["risk per trade", "max risk"]) {
        return Some(json!({ "risk": { "max_risk_per_trade": pct } }));
    }
    if m.contains("enable live trading") || m.contains("turn on live trading") {
        return Some(json!({ "execution": { "live_trading_enabled": true } }));
    }
    if m.contains("disable live trading") || m.contains("turn off live trading") {
        return Some(json!({ "execution": { "live_trading_enabled": false } }));
    }
    if m.contains("enable ml") || m.contains("turn on ml") {
        return Some(json!({ "ml": { "enabled": true } }));
    }
    if m.contains("disable ml") || m.contains("turn off ml") {
        return Some(json!({ "ml": { "enabled": false } }));
    }
    if m.contains("enable decision") {
        return Some(json!({ "decision": { "enabled": true } }));
    }
    if m.contains("disable decision") {
        return Some(json!({ "decision": { "enabled": false } }));
    }
    None
}

fn extract_usize_after(text: &str, phrases: &[&str]) -> Option<usize> {
    for phrase in phrases {
        if let Some(idx) = text.find(phrase) {
            let tail = &text[idx + phrase.len()..];
            if let Some(n) = first_number_in(tail) {
                return Some(n);
            }
        }
    }
    None
}

fn extract_percent_after(text: &str, phrases: &[&str]) -> Option<f64> {
    for phrase in phrases {
        if let Some(idx) = text.find(phrase) {
            let tail = &text[idx + phrase.len()..];
            if let Some(n) = first_number_in(tail) {
                let val = n as f64;
                return Some(if val > 1.0 { val / 100.0 } else { val });
            }
        }
    }
    None
}

fn first_number_in(s: &str) -> Option<usize> {
    let mut digits = String::new();
    let mut started = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            started = true;
        } else if started {
            break;
        }
    }
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn offline_fallback(context: &str, question: &str) -> String {
    let q = question.to_lowercase();
    let mut hints = Vec::new();
    if q.contains("pause") || q.contains("block") || q.contains("resume") || q.contains("start") {
        hints.push(
            "To resume: click Start in the sidebar. If circuit breaker is active, use Reset Circuit Breaker. Kill switch clears on Start.",
        );
    }
    if q.contains("position") || q.contains("pnl") || q.contains("profit") {
        hints.push("See open positions and PnL in the snapshot below.");
    }
    if q.contains("setting") || q.contains("change") || q.contains("set ") {
        hints.push(
            "You can ask me to change settings (e.g. \"set max positions to 5\"). Enable Ollama for full natural-language setting changes.",
        );
    }
    if hints.is_empty() {
        hints.push("LLM is off — here is the latest bot snapshot:");
    }
    format!("{}\n\n{}", hints.join(" "), context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_settings_patch_line() {
        let text = "Done.\nSETTINGS_PATCH: {\"risk\":{\"max_concurrent_positions\":5}}";
        let patch = extract_settings_patch(text).unwrap();
        assert_eq!(patch["risk"]["max_concurrent_positions"], 5);
        assert!(!strip_settings_patch(text).contains("SETTINGS_PATCH"));
    }

    #[test]
    fn infers_max_positions_offline() {
        let patch = infer_settings_patch_from_message("please set max positions to 7").unwrap();
        assert_eq!(patch["risk"]["max_concurrent_positions"], 7);
    }
}
