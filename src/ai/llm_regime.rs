//! Local LLM (Ollama) market-regime classifier — Phase 4.
//!
//! Every `llm.poll_interval_sec` the scanner feeds a compact market snapshot
//! (BTC/ETH higher-timeframe moves, BTC volatility, news sentiment, Fear &
//! Greed) to a local Ollama model, which classifies the regime as
//! trending / chop, high / normal volatility, risk-on / risk-off, plus a BTC
//! directional bias and a confidence score. The result is cached and consumed
//! by the ML feature builder (slots 27–32) and, from Phase 5, the decision
//! layer.
//!
//! Design rule: this layer NEVER hard-blocks a trade. If Ollama is offline,
//! times out, or returns garbage, the regime silently degrades to neutral
//! (`MarketRegime::default()`) and the bot keeps trading on ML alone.

use std::sync::Arc;

use chrono::Utc;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::config::SharedAppConfig;
use crate::ml::MarketRegime;

/// Market snapshot the scanner assembles for each classification request.
#[derive(Debug, Clone, Default)]
pub struct RegimeInputs {
    /// BTC move over the higher-timeframe lookback window, in percent.
    pub btc_move_pct: f64,
    /// ETH move over the same window, in percent.
    pub eth_move_pct: f64,
    /// BTC ATR as % of price — realized volatility proxy.
    pub btc_atr_pct: f64,
    /// Global news sentiment, -1..1.
    pub global_sentiment: f64,
    /// Fear & Greed index (0-100) when available.
    pub fear_greed: Option<i64>,
}

#[derive(Debug, Clone, Default)]
struct RegimeState {
    regime: MarketRegime,
    /// Whether the last classification attempt succeeded.
    available: bool,
    last_error: Option<String>,
    updated_at: String,
    /// Raw JSON text the model returned (for the status endpoint / debugging).
    last_raw: Option<String>,
    consecutive_failures: u32,
}

pub struct LlmRegimeService {
    config: SharedAppConfig,
    client: reqwest::Client,
    state: RwLock<RegimeState>,
}

impl LlmRegimeService {
    pub fn new(config: SharedAppConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            state: RwLock::new(RegimeState::default()),
        }
    }

    /// Current cached regime. Neutral default until the first successful
    /// classification (or forever, if Ollama is never reachable).
    pub async fn regime(&self) -> MarketRegime {
        self.state.read().await.regime.clone()
    }

    pub async fn status_json(&self) -> Value {
        let cfg = self.config.read().unwrap().llm.clone();
        let s = self.state.read().await;
        json!({
            "enabled": cfg.enabled,
            "base_url": cfg.base_url,
            "model": cfg.model,
            "poll_interval_sec": cfg.poll_interval_sec,
            "available": s.available,
            "consecutive_failures": s.consecutive_failures,
            "last_error": s.last_error,
            "updated_at": s.updated_at,
            "regime": {
                "trending": s.regime.trending,
                "chop": s.regime.chop,
                "high_vol": s.regime.high_vol,
                "risk_off": s.regime.risk_off,
                "btc_bias": (s.regime.btc_bias * 1000.0).round() / 1000.0,
                "confidence": (s.regime.confidence * 1000.0).round() / 1000.0,
            },
            "last_raw": s.last_raw,
        })
    }

    /// Run one classification against Ollama and update the cached regime.
    /// All failure modes degrade to neutral without propagating errors.
    pub async fn refresh(self: &Arc<Self>, inputs: &RegimeInputs) {
        let cfg = self.config.read().unwrap().llm.clone();
        if !cfg.enabled {
            self.reset_neutral(None).await;
            return;
        }

        let prompt = build_prompt(inputs);
        let url = format!("{}/api/generate", cfg.base_url.trim_end_matches('/'));
        let body = json!({
            "model": cfg.model,
            "prompt": prompt,
            "stream": false,
            "format": "json",
            "options": { "temperature": 0.2, "num_predict": 200 },
        });

        let result = self
            .client
            .post(&url)
            .timeout(std::time::Duration::from_secs(cfg.timeout_sec.max(5)))
            .json(&body)
            .send()
            .await;

        let response_text = match result {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(v) => v
                    .get("response")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string()),
                Err(exc) => {
                    self.record_failure(format!("Ollama response decode failed: {exc}")).await;
                    return;
                }
            },
            Ok(resp) => {
                self.record_failure(format!("Ollama HTTP {}", resp.status())).await;
                return;
            }
            Err(exc) => {
                self.record_failure(format!("Ollama unreachable: {exc}")).await;
                return;
            }
        };

        let Some(text) = response_text else {
            self.record_failure("Ollama reply had no `response` field".to_string()).await;
            return;
        };

        match parse_regime_response(&text) {
            Some(regime) => {
                let mut s = self.state.write().await;
                let was_offline = !s.available && s.consecutive_failures > 0;
                s.regime = regime.clone();
                s.available = true;
                s.last_error = None;
                s.last_raw = Some(text);
                s.consecutive_failures = 0;
                s.updated_at = Utc::now().to_rfc3339();
                drop(s);
                if was_offline {
                    info!("LLM regime layer back online");
                }
                debug!(
                    "LLM regime: trending={} chop={} high_vol={} risk_off={} bias={:+.2} conf={:.2}",
                    regime.trending, regime.chop, regime.high_vol, regime.risk_off,
                    regime.btc_bias, regime.confidence
                );
            }
            None => {
                self.record_failure(format!("Unparseable regime reply: {}", truncate(&text, 200)))
                    .await;
            }
        }
    }

    async fn record_failure(&self, error: String) {
        let mut s = self.state.write().await;
        let first_failure = s.consecutive_failures == 0;
        s.regime = MarketRegime::default();
        s.available = false;
        s.consecutive_failures = s.consecutive_failures.saturating_add(1);
        s.last_error = Some(error.clone());
        s.updated_at = Utc::now().to_rfc3339();
        drop(s);
        // Warn once per outage, then drop to debug so an offline Ollama
        // doesn't flood the logs every poll cycle.
        if first_failure {
            warn!("LLM regime layer degraded to neutral: {error}");
        } else {
            debug!("LLM regime still unavailable: {error}");
        }
    }

    async fn reset_neutral(&self, error: Option<String>) {
        let mut s = self.state.write().await;
        s.regime = MarketRegime::default();
        s.available = false;
        s.last_error = error;
        s.updated_at = Utc::now().to_rfc3339();
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

/// Compact, deterministic prompt. `format: "json"` in the request forces
/// Ollama to emit valid JSON, so the schema below is all the model needs.
fn build_prompt(inputs: &RegimeInputs) -> String {
    let fear_greed = inputs
        .fear_greed
        .map(|v| v.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "You are a crypto futures market-regime classifier. Based on the snapshot, \
respond with ONLY this JSON schema: \
{{\"trend\": \"trending\"|\"chop\"|\"neutral\", \
\"volatility\": \"high\"|\"normal\", \
\"risk\": \"risk_on\"|\"risk_off\"|\"neutral\", \
\"btc_bias\": <number -1..1, negative=bearish positive=bullish>, \
\"confidence\": <number 0..1>}}\n\
Market snapshot:\n\
- BTC higher-timeframe move: {btc:+.2}%\n\
- ETH higher-timeframe move: {eth:+.2}%\n\
- BTC ATR (volatility, % of price): {atr:.3}%\n\
- Global news sentiment (-1..1): {sent:+.2}\n\
- Fear & Greed index (0-100): {fg}\n",
        btc = inputs.btc_move_pct,
        eth = inputs.eth_move_pct,
        atr = inputs.btc_atr_pct,
        sent = inputs.global_sentiment,
        fg = fear_greed,
    )
}

/// Parse the model's JSON reply into a `MarketRegime`. Tolerant of extra
/// prose around the JSON object and of missing fields; returns `None` only
/// when no usable JSON object can be found at all.
fn parse_regime_response(text: &str) -> Option<MarketRegime> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let v: Value = serde_json::from_str(&text[start..=end]).ok()?;

    let trend = v.get("trend").and_then(|t| t.as_str()).unwrap_or("neutral").to_lowercase();
    let volatility = v.get("volatility").and_then(|t| t.as_str()).unwrap_or("normal").to_lowercase();
    let risk = v.get("risk").and_then(|t| t.as_str()).unwrap_or("neutral").to_lowercase();
    let btc_bias = v.get("btc_bias").and_then(|b| b.as_f64()).unwrap_or(0.0);
    let confidence = v.get("confidence").and_then(|c| c.as_f64()).unwrap_or(0.0);

    Some(MarketRegime {
        trending: trend.contains("trend"),
        chop: trend.contains("chop") || trend.contains("rang"),
        high_vol: volatility.contains("high"),
        risk_off: risk.contains("off"),
        btc_bias: btc_bias.clamp(-1.0, 1.0),
        confidence: confidence.clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_json_reply() {
        let text = r#"{"trend": "trending", "volatility": "high", "risk": "risk_off", "btc_bias": -0.6, "confidence": 0.85}"#;
        let r = parse_regime_response(text).unwrap();
        assert!(r.trending);
        assert!(!r.chop);
        assert!(r.high_vol);
        assert!(r.risk_off);
        assert!((r.btc_bias + 0.6).abs() < 1e-9);
        assert!((r.confidence - 0.85).abs() < 1e-9);
    }

    #[test]
    fn parses_json_wrapped_in_prose() {
        let text = "Here is my analysis:\n{\"trend\": \"chop\", \"volatility\": \"normal\", \"risk\": \"risk_on\", \"btc_bias\": 0.1, \"confidence\": 0.4}\nHope that helps!";
        let r = parse_regime_response(text).unwrap();
        assert!(!r.trending);
        assert!(r.chop);
        assert!(!r.high_vol);
        assert!(!r.risk_off);
    }

    #[test]
    fn missing_fields_default_to_neutral() {
        let r = parse_regime_response("{}").unwrap();
        assert!(!r.trending && !r.chop && !r.high_vol && !r.risk_off);
        assert_eq!(r.btc_bias, 0.0);
        assert_eq!(r.confidence, 0.0);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let text = r#"{"trend": "trending", "btc_bias": 5.0, "confidence": 3.0}"#;
        let r = parse_regime_response(text).unwrap();
        assert_eq!(r.btc_bias, 1.0);
        assert_eq!(r.confidence, 1.0);
    }

    #[test]
    fn garbage_reply_yields_none() {
        assert!(parse_regime_response("the market looks good").is_none());
        assert!(parse_regime_response("").is_none());
    }

    #[test]
    fn prompt_includes_snapshot_numbers() {
        let inputs = RegimeInputs {
            btc_move_pct: 1.25,
            eth_move_pct: -0.8,
            btc_atr_pct: 0.42,
            global_sentiment: 0.3,
            fear_greed: Some(65),
        };
        let p = build_prompt(&inputs);
        assert!(p.contains("+1.25%"));
        assert!(p.contains("-0.80%"));
        assert!(p.contains("0.420%"));
        assert!(p.contains("65"));
        assert!(p.contains("btc_bias"));
    }
}
