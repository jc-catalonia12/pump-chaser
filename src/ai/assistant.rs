//! In-app assistant — answers questions about bot status via local Ollama.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::config::SharedAppConfig;
use crate::execution::LiveTrader;
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
            reply: "Ask me anything about scanner status, risk, positions, or training progress.".into(),
            ollama_available: false,
        };
    }

    let cfg = config.read().unwrap().llm.clone();
    let context = build_bot_context(state.as_ref()).await;

    let system = format!(
        "You are Pump Chaser Bot Assistant — a helpful co-pilot for the MEXC futures trading bot. \
Answer concisely in plain English (2–6 sentences unless the user asks for detail). \
Use ONLY the live bot snapshot below for factual status; say if data is missing. \
You can explain: scanner/risk state, why trades may be blocked, open positions, PnL, sentiment, \
ML/training progress, and how to resume trading (Start button, reset circuit breaker). \
Do not invent trades or prices not in the snapshot.\n\n--- LIVE SNAPSHOT ---\n{context}\n--- END ---"
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

    if !cfg.enabled {
        return AssistantChatResponse {
            reply: offline_fallback(&context, message),
            ollama_available: false,
        };
    }

    let url = format!("{}/api/chat", cfg.base_url.trim_end_matches('/'));
    let body = json!({
        "model": cfg.model,
        "messages": messages,
        "stream": false,
        "options": { "temperature": 0.35, "num_predict": 512 },
    });

    let client = reqwest::Client::new();
    let result = client
        .post(&url)
        .timeout(std::time::Duration::from_secs(cfg.timeout_sec.max(10).min(120)))
        .json(&body)
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(v) = resp.json::<Value>().await {
                if let Some(text) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    return AssistantChatResponse {
                        reply: text.trim().to_string(),
                        ollama_available: true,
                    };
                }
            }
            AssistantChatResponse {
                reply: "Ollama returned an empty reply. Check that the model is pulled.".into(),
                ollama_available: false,
            }
        }
        Ok(resp) => {
            warn!("Assistant Ollama HTTP {}", resp.status());
            AssistantChatResponse {
                reply: format!(
                    "Ollama error (HTTP {}). {}\n\nStart Ollama or enable LLM in Settings, then try again.",
                    resp.status(),
                    offline_fallback(&context, message)
                ),
                ollama_available: false,
            }
        }
        Err(exc) => {
            warn!("Assistant Ollama unreachable: {exc}");
            AssistantChatResponse {
                reply: format!(
                    "Ollama is offline ({exc}). {}\n\nStart Ollama or enable LLM in Settings, then try again.",
                    offline_fallback(&context, message)
                ),
                ollama_available: false,
            }
        }
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
    if hints.is_empty() {
        hints.push("LLM is off — here is the latest bot snapshot:");
    }
    format!("{}\n\n{}", hints.join(" "), context)
}
