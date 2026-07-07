//! User-editable subset of `config/settings.yaml` — schema, read, merge, and persist.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{
    AppConfig, DecisionConfig, ExecutionConfig, LearningConfig, LlmConfig, RiskConfig,
    TradingConfig, WatchlistConfig, ZonesConfig,
};
use crate::error::{BotError, Result};
use crate::utils::discover_project_root;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsSection {
    pub id: String,
    pub title: String,
    pub description: String,
    pub fields: Vec<SettingsField>,
}

pub fn settings_file_path() -> PathBuf {
    std::env::var("MEXC_BOT_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            discover_project_root()
                .map(|p| p.join("config/settings.yaml"))
                .unwrap_or_else(|| PathBuf::from("config/settings.yaml"))
        })
}

pub fn save_app_config(cfg: &AppConfig) -> Result<()> {
    let path = settings_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let yaml = serde_yaml::to_string(cfg).map_err(|e| BotError::Config(e.to_string()))?;
    std::fs::write(&path, yaml)?;
    Ok(())
}

pub fn user_settings_values(cfg: &AppConfig) -> Value {
    json!({
        "mexc": {
            "rest_base_url": cfg.mexc.rest_base_url,
            "ws_url": cfg.mexc.ws_url,
        },
        "execution": cfg.execution,
        "trading": cfg.trading,
        "risk": cfg.risk,
        "scanner": {
            "min_24h_turnover_usdt": cfg.scanner.min_24h_turnover_usdt,
            "max_symbols_kline_poll": cfg.scanner.max_symbols_kline_poll,
            "kline_refresh_sec": cfg.scanner.kline_refresh_sec,
            "min_price_usdt": cfg.scanner.min_price_usdt,
            "usdt_m_crypto_only": cfg.scanner.usdt_m_crypto_only,
        },
        "zones": cfg.zones,
        "ml": {
            "enabled": cfg.ml.enabled,
            "supervised_enabled": cfg.ml.supervised_enabled,
            "supervised_threshold": cfg.ml.supervised_threshold,
            "min_training_samples": cfg.ml.min_training_samples,
            "hard_ml_gate": cfg.ml.hard_ml_gate,
            "trade_win_weight": cfg.ml.trade_win_weight,
            "trade_loss_weight": cfg.ml.trade_loss_weight,
            "kelly_fraction": cfg.ml.kelly_fraction,
            "auto_retrain_enabled": cfg.ml.auto_retrain_enabled,
        },
        "llm": cfg.llm,
        "decision": cfg.decision,
        "learning": cfg.learning,
        "watchlist": cfg.watchlist,
    })
}

pub fn settings_schema() -> Vec<SettingsSection> {
    vec![
        SettingsSection {
            id: "mexc".into(),
            title: "MEXC API Endpoints".into(),
            description: "REST + WebSocket hostnames (default: contract.mexc.co). Use contract.mexc.com if mexc.co is blocked in your region. Restart the scanner after saving; wallet sync uses the new URLs immediately.".into(),
            fields: vec![
                field_text(
                    "mexc.rest_base_url",
                    "Futures REST base URL",
                    Some("e.g. https://contract.mexc.co"),
                ),
                field_text(
                    "mexc.ws_url",
                    "Futures WebSocket URL",
                    Some("e.g. wss://contract.mexc.co/edge"),
                ),
            ],
        },
        SettingsSection {
            id: "execution".into(),
            title: "Execution & Safety".into(),
            description: "Controls whether real orders can be sent. Live mode in the sidebar still requires live trading enabled here.".into(),
            fields: vec![
                field_bool("execution.live_trading_enabled", "Allow live trading", Some("Must be enabled along with Live mode and valid API keys")),
                field_bool("execution.dry_run", "Dry run", Some("Log orders without sending when live")),
                field_bool("execution.sync_exchange_positions", "Sync exchange positions", Some("Reconcile open positions with MEXC on startup")),
                field_num(
                    "execution.paper_initial_equity",
                    "Paper starting equity (USDT)",
                    10.0,
                    1_000_000.0,
                    10.0,
                ),
                field_bool("execution.paper_reset_on_start", "Reset paper equity on next start", Some("One-shot: sets equity to paper starting value when no open positions")),
                field_pct("execution.limit_offset_pct", "Limit offset (%)", 0.0001, 0.01, 0.0001),
                field_int("execution.limit_ttl_sec", "Limit TTL (sec)", 5.0, 300.0, 5.0),
            ],
        },
        SettingsSection {
            id: "trading".into(),
            title: "Trading".into(),
            description: "Single AI-driven pipeline; per-strategy engines were retired.".into(),
            fields: vec![
                field_select(
                    "trading.mode",
                    "Trading mode",
                    vec!["ai"],
                    Some("ai = unified ML-driven signal pipeline"),
                ),
                field_int("trading.max_hold_sec", "Max hold (seconds)", 0.0, 86400.0, 60.0),
            ],
        },
        SettingsSection {
            id: "risk".into(),
            title: "Risk Limits".into(),
            description: "Portfolio-level caps enforced before each trade.".into(),
            fields: vec![
                field_pct("risk.max_risk_per_trade", "Max risk per trade", 0.001, 0.2, 0.001),
                field_int("risk.max_concurrent_positions", "Max concurrent positions", 1.0, 20.0, 1.0),
                field_num("risk.min_profit_usdt", "Min profit per trade (USDT)", 0.0, 100.0, 0.5),
                field_pct("risk.daily_loss_limit", "Daily loss limit", 0.01, 0.5, 0.01),
                field_int("risk.max_leverage", "Max leverage (hard cap)", 1.0, 300.0, 5.0),
                field_num("risk.min_position_margin_usdt", "Min position margin (USDT)", 1.0, 1000.0, 0.5),
                field_bool("risk.use_live_wallet_equity", "Use live wallet equity", Some("Anchor risk sizing to MEXC wallet when credentials are set")),
                field_pct("risk.default_sl_pct", "Default stop loss", 0.005, 0.15, 0.001),
                field_pct("risk.trailing_stop_pct", "Trailing stop", 0.001, 0.1, 0.001),
                field_pct("risk.trailing_activation_pct", "Trailing activation", 0.001, 0.1, 0.001),
                field_pct("risk.trailing_extended_activation_pct", "Wide trail activation", 0.01, 0.15, 0.001),
                field_pct("risk.trailing_extended_stop_pct", "Wide trail distance", 0.005, 0.08, 0.001),
                field_pct("risk.trailing_runner_activation_pct", "Runner trail activation", 0.02, 0.2, 0.001),
                field_pct("risk.trailing_runner_stop_pct", "Runner trail distance", 0.01, 0.1, 0.001),
                field_bool("risk.allow_hedge", "Allow hedging", Some("Off = never hold both a long and short on the same symbol")),
            ],
        },
        SettingsSection {
            id: "scanner".into(),
            title: "Scanner Filters".into(),
            description: "Universe size and liquidity filters for symbol polling.".into(),
            fields: vec![
                field_num("scanner.min_24h_turnover_usdt", "Min 24h turnover (USDT)", 10_000.0, 50_000_000.0, 10_000.0),
                field_int("scanner.max_symbols_kline_poll", "Max symbols polled", 10.0, 500.0, 10.0),
                field_int("scanner.kline_refresh_sec", "Kline refresh (sec)", 10.0, 600.0, 5.0),
                field_num("scanner.min_price_usdt", "Min price (USDT)", 0.0000001, 1.0, 0.00001),
                field_bool("scanner.usdt_m_crypto_only", "USDT-M crypto only", None),
            ],
        },
        SettingsSection {
            id: "zones".into(),
            title: "Supply / Demand Zones".into(),
            description: "Supply/demand zone detection used as AI pipeline features.".into(),
            fields: vec![
                field_bool("zones.enabled", "Zones enabled", None),
                field_int("zones.lookback_bars", "Lookback bars", 10.0, 200.0, 5.0),
                field_pct("zones.zone_width_pct", "Zone width", 0.05, 1.0, 0.01),
                field_pct("zones.proximity_pct", "Proximity", 0.05, 1.0, 0.01),
            ],
        },
        SettingsSection {
            id: "ml".into(),
            title: "Machine Learning".into(),
            description: "Online + ONNX ensemble gating and Kelly-based sizing.".into(),
            fields: vec![
                field_bool("ml.enabled", "ML enabled", None),
                field_bool("ml.supervised_enabled", "Supervised model enabled", None),
                field_num("ml.supervised_threshold", "Supervised threshold", 0.1, 0.99, 0.01),
                field_int("ml.min_training_samples", "Min training samples", 10.0, 10_000.0, 10.0),
                field_bool("ml.hard_ml_gate", "Hard ML gate", Some("Reject signals below threshold instead of soft scoring")),
                field_num("ml.kelly_fraction", "Kelly fraction", 0.05, 1.0, 0.05),
                field_bool("ml.auto_retrain_enabled", "Auto ONNX retrain", Some("Periodically retrain the ONNX model from the local DB (needs Python + requirements.txt)")),
            ],
        },
        SettingsSection {
            id: "llm".into(),
            title: "Local LLM Regime (Ollama)".into(),
            description: "Market-regime classification via a local Ollama instance. Fully optional — when Ollama is offline the bot trades on ML alone with a neutral regime.".into(),
            fields: vec![
                field_bool("llm.enabled", "LLM regime enabled", None),
                field_text("llm.base_url", "Ollama base URL", Some("e.g. http://localhost:11434")),
                field_text("llm.model", "Ollama model tag", Some("Must be pulled first, e.g. `ollama pull llama3.2`")),
                field_int("llm.poll_interval_sec", "Poll interval (sec)", 60.0, 3600.0, 30.0),
                field_int("llm.timeout_sec", "Request timeout (sec)", 5.0, 120.0, 5.0),
            ],
        },
        SettingsSection {
            id: "decision".into(),
            title: "Decision Engine".into(),
            description: "Unified go/no-go authority: expected-value and reward:risk gates plus regime/sentiment conviction sizing.".into(),
            fields: vec![
                field_bool("decision.enabled", "Decision engine enabled", None),
                field_num("decision.min_expected_value", "Min expected value (R)", -0.5, 1.0, 0.01),
                field_num("decision.min_reward_risk", "Min reward:risk", 0.2, 5.0, 0.1),
                field_num("decision.regime_veto_confidence", "Regime veto confidence", 0.0, 1.0, 0.05),
                field_num("decision.min_size_scale", "Min size multiplier", 0.1, 1.0, 0.05),
                field_num("decision.max_size_scale", "Max size multiplier", 1.0, 3.0, 0.1),
            ],
        },
        SettingsSection {
            id: "features".into(),
            title: "Feature Toggles".into(),
            description: "Learning loop and watchlist options.".into(),
            fields: vec![
                field_bool("learning.enabled", "Learning loop", Some("Record outcomes for model retraining")),
                field_select("watchlist.mode", "Watchlist mode", vec!["all", "manual"], Some("all = scan full universe")),
            ],
        },
    ]
}

pub fn apply_user_settings(cfg: &mut AppConfig, patch: &Value) -> Result<()> {
    let mut merged = user_settings_values(cfg);
    deep_merge(&mut merged, patch);

    if let Some(v) = merged.get("mexc") {
        let mut mexc = cfg.mexc.clone();
        let m = v
            .as_object()
            .ok_or_else(|| BotError::Config("mexc must be object".into()))?;
        if let Some(x) = m.get("rest_base_url") {
            mexc.rest_base_url = normalize_mexc_url(json_to_string(x)?, true)?;
        }
        if let Some(x) = m.get("ws_url") {
            mexc.ws_url = normalize_mexc_url(json_to_string(x)?, false)?;
        }
        cfg.mexc = mexc;
    }
    if let Some(v) = merged.get("execution") {
        cfg.execution = serde_json::from_value::<ExecutionConfig>(v.clone())?;
    }
    if let Some(v) = merged.get("trading") {
        cfg.trading = serde_json::from_value::<TradingConfig>(v.clone())?;
    }
    if let Some(v) = merged.get("risk") {
        cfg.risk = serde_json::from_value::<RiskConfig>(v.clone())?;
    }
    if let Some(v) = merged.get("scanner") {
        let s = v.as_object().ok_or_else(|| BotError::Config("scanner must be object".into()))?;
        if let Some(x) = s.get("min_24h_turnover_usdt") {
            cfg.scanner.min_24h_turnover_usdt = json_to_f64(x)?;
        }
        if let Some(x) = s.get("max_symbols_kline_poll") {
            cfg.scanner.max_symbols_kline_poll = json_to_u32(x)?;
        }
        if let Some(x) = s.get("kline_refresh_sec") {
            cfg.scanner.kline_refresh_sec = json_to_u64(x)?;
        }
        if let Some(x) = s.get("min_price_usdt") {
            cfg.scanner.min_price_usdt = json_to_f64(x)?;
        }
        if let Some(x) = s.get("usdt_m_crypto_only") {
            cfg.scanner.usdt_m_crypto_only = json_to_bool(x)?;
        }
    }
    if let Some(v) = merged.get("zones") {
        cfg.zones = serde_json::from_value::<ZonesConfig>(v.clone())?;
    }
    if let Some(v) = merged.get("ml") {
        let m = v.as_object().ok_or_else(|| BotError::Config("ml must be object".into()))?;
        if let Some(x) = m.get("enabled") {
            cfg.ml.enabled = json_to_bool(x)?;
        }
        if let Some(x) = m.get("supervised_enabled") {
            cfg.ml.supervised_enabled = json_to_bool(x)?;
        }
        if let Some(x) = m.get("supervised_threshold") {
            cfg.ml.supervised_threshold = json_to_f64(x)?;
        }
        if let Some(x) = m.get("min_training_samples") {
            cfg.ml.min_training_samples = json_to_u32(x)?;
        }
        if let Some(x) = m.get("hard_ml_gate") {
            cfg.ml.hard_ml_gate = json_to_bool(x)?;
        }
        if let Some(x) = m.get("trade_win_weight") {
            cfg.ml.trade_win_weight = json_to_f64(x)?;
        }
        if let Some(x) = m.get("trade_loss_weight") {
            cfg.ml.trade_loss_weight = json_to_f64(x)?;
        }
        if let Some(x) = m.get("kelly_fraction") {
            cfg.ml.kelly_fraction = json_to_f64(x)?;
        }
        if let Some(x) = m.get("auto_retrain_enabled") {
            cfg.ml.auto_retrain_enabled = json_to_bool(x)?;
        }
    }
    if let Some(v) = merged.get("llm") {
        cfg.llm = serde_json::from_value::<LlmConfig>(v.clone())?;
    }
    if let Some(v) = merged.get("decision") {
        cfg.decision = serde_json::from_value::<DecisionConfig>(v.clone())?;
    }
    if let Some(v) = merged.get("learning") {
        cfg.learning = serde_json::from_value::<LearningConfig>(v.clone())?;
    }
    if let Some(v) = merged.get("watchlist") {
        cfg.watchlist = serde_json::from_value::<WatchlistConfig>(v.clone())?;
    }

    Ok(())
}

fn deep_merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                if v.is_null() {
                    continue;
                }
                match a.get_mut(k) {
                    Some(slot) => deep_merge(slot, v),
                    None => {
                        a.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (slot, value) => *slot = value.clone(),
    }
}

fn field_text(key: &str, label: &str, hint: Option<&str>) -> SettingsField {
    SettingsField {
        key: key.into(),
        label: label.into(),
        field_type: "text".into(),
        hint: hint.map(str::to_string),
        options: None,
        min: None,
        max: None,
        step: None,
    }
}

fn field_bool(key: &str, label: &str, hint: Option<&str>) -> SettingsField {
    SettingsField {
        key: key.into(),
        label: label.into(),
        field_type: "bool".into(),
        hint: hint.map(str::to_string),
        options: None,
        min: None,
        max: None,
        step: None,
    }
}

fn field_num(key: &str, label: &str, min: f64, max: f64, step: f64) -> SettingsField {
    SettingsField {
        key: key.into(),
        label: label.into(),
        field_type: "number".into(),
        hint: None,
        options: None,
        min: Some(min),
        max: Some(max),
        step: Some(step),
    }
}

fn field_pct(key: &str, label: &str, min: f64, max: f64, step: f64) -> SettingsField {
    SettingsField {
        key: key.into(),
        label: label.into(),
        field_type: "percent".into(),
        hint: Some("Decimal fraction (0.03 = 3%)".into()),
        options: None,
        min: Some(min),
        max: Some(max),
        step: Some(step),
    }
}

fn field_int(key: &str, label: &str, min: f64, max: f64, step: f64) -> SettingsField {
    SettingsField {
        key: key.into(),
        label: label.into(),
        field_type: "integer".into(),
        hint: None,
        options: None,
        min: Some(min),
        max: Some(max),
        step: Some(step),
    }
}

fn field_select(key: &str, label: &str, options: Vec<&str>, hint: Option<&str>) -> SettingsField {
    SettingsField {
        key: key.into(),
        label: label.into(),
        field_type: "select".into(),
        hint: hint.map(str::to_string),
        options: Some(options.into_iter().map(str::to_string).collect()),
        min: None,
        max: None,
        step: None,
    }
}

fn json_to_f64(v: &Value) -> Result<f64> {
    v.as_f64()
        .ok_or_else(|| BotError::Config(format!("expected number, got {v}")))
}

fn json_to_u32(v: &Value) -> Result<u32> {
    let n = json_to_f64(v)?;
    if n < 0.0 || n.fract() != 0.0 {
        return Err(BotError::Config(format!("expected positive integer, got {n}")));
    }
    Ok(n as u32)
}

fn json_to_u64(v: &Value) -> Result<u64> {
    let n = json_to_f64(v)?;
    if n < 0.0 || n.fract() != 0.0 {
        return Err(BotError::Config(format!("expected positive integer, got {n}")));
    }
    Ok(n as u64)
}

fn json_to_bool(v: &Value) -> Result<bool> {
    v.as_bool()
        .ok_or_else(|| BotError::Config(format!("expected boolean, got {v}")))
}

fn json_to_string(v: &Value) -> Result<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| BotError::Config(format!("expected non-empty string, got {v}")))
}

/// Trim trailing slashes; require https for REST and wss for WebSocket.
fn normalize_mexc_url(raw: String, rest: bool) -> Result<String> {
    let url = raw.trim().trim_end_matches('/').to_string();
    if rest {
        if !url.starts_with("https://") {
            return Err(BotError::Config(
                "REST base URL must start with https://".into(),
            ));
        }
    } else if !url.starts_with("wss://") {
        return Err(BotError::Config(
            "WebSocket URL must start with wss://".into(),
        ));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_unedited_scanner_fields() {
        let mut cfg = AppConfig::load().expect("settings.yaml");
        let original_interval = cfg.scanner.kline_interval.clone();
        apply_user_settings(
            &mut cfg,
            &json!({ "scanner": { "min_24h_turnover_usdt": 750_000.0 } }),
        )
        .unwrap();
        assert_eq!(cfg.scanner.min_24h_turnover_usdt, 750_000.0);
        assert_eq!(cfg.scanner.kline_interval, original_interval);
    }

    #[test]
    fn schema_mexc_fields_are_text_type() {
        let sections = settings_schema();
        let v = serde_json::to_value(&sections).expect("serialize schema");
        let fields = v[0]["fields"].as_array().expect("mexc fields");
        assert_eq!(fields[0]["type"], "text");
        assert_eq!(fields[0]["key"], "mexc.rest_base_url");
        assert_eq!(fields[1]["type"], "text");
    }

    #[test]
    fn merge_updates_mexc_endpoints() {
        let mut cfg = AppConfig::load().expect("settings.yaml");
        apply_user_settings(
            &mut cfg,
            &json!({
                "mexc": {
                    "rest_base_url": "https://contract.mexc.co",
                    "ws_url": "wss://contract.mexc.co/edge"
                }
            }),
        )
        .unwrap();
        assert_eq!(cfg.mexc.rest_base_url, "https://contract.mexc.co");
        assert_eq!(cfg.mexc.ws_url, "wss://contract.mexc.co/edge");
    }
}
