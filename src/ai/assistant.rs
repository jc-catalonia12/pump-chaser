//! In-app assistant — answers questions about bot status via local Ollama.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::ai::assistant_tools::{cap_tool_result, execute_tool, ollama_tool_defs};
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

    let cfg = config.read().unwrap().clone();
    let llm = cfg.llm.clone();
    let assistant_cfg = cfg.assistant.clone();
    let context = build_bot_context(state.as_ref()).await;

    let tool_lines = {
        let mut lines: Vec<String> = vec![
            "- get_settings(): read config/settings.yaml (user-editable fields)".into(),
        ];
        if assistant_cfg.web_enabled {
            lines.push("- web_fetch(url): fetch public web pages for research".into());
        }
        if assistant_cfg.settings_write_enabled {
            lines.push(
                "- update_settings(patch): merge JSON into settings.yaml — only when the user explicitly asks"
                    .into(),
            );
        }
        lines.join("\n")
    };

    let system = format!(
        "You are Pump Chaser Bot Assistant — a helpful co-pilot for the MEXC futures trading bot. \
Answer concisely in plain English (2–6 sentences unless the user asks for detail). \
Use the live bot snapshot below for factual status; say if data is missing. \
You can explain: scanner/risk state, why trades may be blocked, open positions, PnL, sentiment, \
ML/training progress, and how to resume trading (Start button, reset circuit breaker). \
Do not invent trades or prices not in the snapshot.\n\n\
You have tools:\n{tool_lines}\n\
When changing settings, confirm what you changed and the config path.\n\n\
--- LIVE SNAPSHOT ---\n{context}\n--- END ---"
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

    if !llm.enabled {
        return AssistantChatResponse {
            reply: offline_fallback(&context, message),
            ollama_available: false,
        };
    }

    let tools = ollama_tool_defs(assistant_cfg.web_enabled, assistant_cfg.settings_write_enabled);
    let max_rounds = assistant_cfg.max_tool_rounds.max(1).min(8) as usize;
    let base_timeout = std::time::Duration::from_secs(llm.timeout_sec.max(10).min(120));
    let max_tool_chars = assistant_cfg.max_tool_result_chars.max(256).min(32_000);
    let url = format!("{}/api/chat", llm.base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut ollama_reached = false;
    let mut used_tools = false;

    for round in 0..max_rounds {
        let round_timeout = if round == 0 || !used_tools {
            base_timeout
        } else {
            base_timeout.saturating_mul(3).min(std::time::Duration::from_secs(180))
        };
        let include_tools = !tools.is_empty() && round + 1 < max_rounds;

        let mut attempt = 0u8;
        loop {
            attempt += 1;
            let mut body = json!({
                "model": llm.model,
                "messages": messages,
                "stream": false,
                "options": { "temperature": 0.35, "num_predict": 1024 },
            });
            if include_tools {
                body["tools"] = json!(tools);
            }

            let result = client.post(&url).timeout(round_timeout).json(&body).send().await;

            match result {
                Ok(resp) if resp.status().is_success() => {
                    ollama_reached = true;
                    let v = match resp.json::<Value>().await {
                        Ok(v) => v,
                        Err(exc) => {
                            warn!("Assistant Ollama JSON parse: {exc}");
                            break;
                        }
                    };
                    let message_obj = v.get("message").cloned().unwrap_or(json!({}));
                    let content = message_obj
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    messages.push(message_obj.clone());

                    let tool_calls = message_obj
                        .get("tool_calls")
                        .and_then(|t| t.as_array())
                        .cloned()
                        .unwrap_or_default();

                    if tool_calls.is_empty() {
                        if content.is_empty() {
                            return AssistantChatResponse {
                                reply: "Ollama returned an empty reply. Check that the model is pulled.".into(),
                                ollama_available: true,
                            };
                        }
                        return AssistantChatResponse {
                            reply: content,
                            ollama_available: true,
                        };
                    }

                    used_tools = true;
                    for tc in tool_calls {
                        let func = tc.get("function").cloned().unwrap_or_default();
                        let name = func
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args: Value = func
                            .get("arguments")
                            .map(|a| {
                                if a.is_string() {
                                    serde_json::from_str(a.as_str().unwrap_or("{}"))
                                        .unwrap_or(json!({}))
                                } else {
                                    a.clone()
                                }
                            })
                            .unwrap_or(json!({}));
                        info!(tool = %name, round, "assistant tool call");
                        let result = execute_tool(
                            state.as_ref(),
                            &name,
                            &args,
                            assistant_cfg.web_enabled,
                            assistant_cfg.settings_write_enabled,
                            assistant_cfg.max_fetch_bytes,
                            max_tool_chars,
                        )
                        .await;
                        messages.push(json!({
                            "role": "tool",
                            "content": result,
                            "tool_name": name,
                        }));
                    }
                    break;
                }
                Ok(resp) => {
                    warn!("Assistant Ollama HTTP {}", resp.status());
                    if ollama_reached {
                        return AssistantChatResponse {
                            reply: format!(
                                "Ollama returned HTTP {} while summarizing tool results. Try a narrower question.",
                                resp.status()
                            ),
                            ollama_available: true,
                        };
                    }
                    return AssistantChatResponse {
                        reply: format!(
                            "Ollama error (HTTP {}). {}\n\nStart Ollama or enable LLM in Settings, then try again.",
                            resp.status(),
                            offline_fallback(&context, message)
                        ),
                        ollama_available: false,
                    };
                }
                Err(exc) => {
                    let is_timeout = exc.is_timeout();
                    if ollama_reached && attempt == 1 {
                        warn!(
                            error = %exc,
                            round,
                            "Assistant Ollama retry with smaller tool payload"
                        );
                        shrink_tool_messages(&mut messages, max_tool_chars / 2);
                        continue;
                    }
                    if ollama_reached {
                        warn!("Assistant Ollama failed after tools: {exc}");
                        return AssistantChatResponse {
                            reply: if is_timeout {
                                "Ollama timed out summarizing the web page (it was too large). Try asking for a specific headline or shorter summary.".into()
                            } else {
                                format!(
                                    "Ollama could not finish after fetching data ({exc}). Try a more specific question."
                                )
                            },
                            ollama_available: true,
                        };
                    }
                    warn!("Assistant Ollama unreachable: {exc}");
                    return AssistantChatResponse {
                        reply: format!(
                            "Ollama is offline ({exc}). {}\n\nStart Ollama or enable LLM in Settings, then try again.",
                            offline_fallback(&context, message)
                        ),
                        ollama_available: false,
                    };
                }
            }
        }
    }

    AssistantChatResponse {
        reply: if ollama_reached {
            "Assistant reached the tool-call limit. Try a simpler question or increase assistant.max_tool_rounds.".into()
        } else {
            "Assistant could not reach Ollama.".into()
        },
        ollama_available: ollama_reached,
    }
}

fn shrink_tool_messages(messages: &mut [Value], max_chars: usize) {
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("tool") {
            continue;
        }
        let Some(content) = msg.get("content").and_then(|c| c.as_str()) else {
            continue;
        };
        let capped = cap_tool_result(content.to_string(), max_chars);
        msg["content"] = json!(capped);
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
