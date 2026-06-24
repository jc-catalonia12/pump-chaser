//! API credentials — env vars or local `data/secrets.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserSecrets {
    #[serde(default)]
    pub mexc_api_key: String,
    #[serde(default)]
    pub mexc_api_secret: String,
    #[serde(default = "default_true")]
    pub paper_trading: bool,
    #[serde(default)]
    pub live_trading: bool,
    // Telegram notifications
    #[serde(default)]
    pub telegram_bot_token: String,
    #[serde(default)]
    pub telegram_chat_id: String,
    #[serde(default)]
    pub telegram_enabled: bool,
    /// Cached from Telegram `getMe` when the bot token is saved.
    #[serde(default)]
    pub telegram_bot_username: String,
    #[serde(default)]
    pub telegram_bot_name: String,
    /// Which events to send. Empty = all trade events.
    #[serde(default = "default_telegram_events")]
    pub telegram_events: Vec<String>,
}

fn default_telegram_events() -> Vec<String> {
    vec![
        "position_opened".into(),
        "position_closed".into(),
        "tp_hit".into(),
        "cut_loss".into(),
        "kill_switch".into(),
    ]
}

fn default_true() -> bool {
    true
}

impl UserSecrets {
    pub fn has_credentials(&self) -> bool {
        !self.mexc_api_key.is_empty() && !self.mexc_api_secret.is_empty()
    }

    pub fn has_telegram(&self) -> bool {
        !self.telegram_bot_token.is_empty() && !self.telegram_chat_id.is_empty()
    }

    pub fn to_public(&self) -> serde_json::Value {
        serde_json::json!({
            "mexc_api_key_masked": mask_api_key(&self.mexc_api_key),
            "mexc_api_secret_masked": mask_api_secret(&self.mexc_api_secret),
            "has_credentials": self.has_credentials(),
            "paper_trading": self.paper_trading,
            "live_trading": self.live_trading,
        })
    }

    pub fn telegram_public(&self) -> serde_json::Value {
        let bot_link = if self.telegram_bot_username.is_empty() {
            Value::Null
        } else {
            json!(format!("https://t.me/{}", self.telegram_bot_username))
        };
        serde_json::json!({
            "connected": self.has_telegram(),
            "enabled": self.telegram_enabled,
            "chat_id_masked": if self.telegram_chat_id.is_empty() { "".into() } else { mask_api_secret(&self.telegram_chat_id) },
            "token_masked": if self.telegram_bot_token.is_empty() { "".into() } else { mask_api_secret(&self.telegram_bot_token) },
            "bot_username": self.telegram_bot_username,
            "bot_name": self.telegram_bot_name,
            "bot_link": bot_link,
            "events": self.telegram_events,
        })
    }

    pub fn clear_credentials(&mut self) {
        self.mexc_api_key.clear();
        self.mexc_api_secret.clear();
        self.paper_trading = true;
        self.live_trading = false;
    }

    pub fn clear_telegram(&mut self) {
        self.telegram_bot_token.clear();
        self.telegram_chat_id.clear();
        self.telegram_bot_username.clear();
        self.telegram_bot_name.clear();
        self.telegram_enabled = false;
        self.telegram_events = default_telegram_events();
    }
}

/// Mask API key — show only last 4 characters.
pub fn mask_api_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    if key.len() <= 4 {
        return "********".into();
    }
    format!("********{}", &key[key.len() - 4..])
}

/// Mask API secret — password-style, no partial reveal.
pub fn mask_api_secret(secret: &str) -> String {
    if secret.is_empty() {
        return String::new();
    }
    "********".into()
}

pub fn mask_secret(secret: &str) -> String {
    mask_api_secret(secret)
}

fn is_masked_credential(value: &str) -> bool {
    value.is_empty()
        || value == "********"
        || value.starts_with("********")
        || value.contains("...")
}

pub fn secrets_path() -> PathBuf {
    if let Ok(p) = std::env::var("MEXC_BOT_SECRETS_PATH") {
        return PathBuf::from(p);
    }
    PathBuf::from("data/secrets.json")
}

pub fn load_secrets() -> UserSecrets {
    let mut s = if secrets_path().exists() {
        std::fs::read_to_string(secrets_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    } else {
        UserSecrets::default()
    };

    if let Ok(k) = std::env::var("MEXC_API_KEY") {
        if !k.is_empty() {
            s.mexc_api_key = k;
        }
    }
    if let Ok(sec) = std::env::var("MEXC_API_SECRET") {
        if !sec.is_empty() {
            s.mexc_api_secret = sec;
        }
    }
    if let Ok(v) = std::env::var("PAPER_TRADING") {
        s.paper_trading = matches!(v.to_lowercase().as_str(), "1" | "true" | "yes");
    }
    if let Ok(v) = std::env::var("LIVE_TRADING") {
        s.live_trading = matches!(v.to_lowercase().as_str(), "1" | "true" | "yes");
    }
    s
}

pub fn save_secrets(secrets: &UserSecrets) -> crate::error::Result<()> {
    let path = secrets_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(secrets)?)?;
    Ok(())
}

pub fn merge_secrets_update(mut current: UserSecrets, patch: &serde_json::Value) -> UserSecrets {
    if let Some(mode) = patch.get("execution_mode").and_then(|v| v.as_str()) {
        match mode.to_lowercase().as_str() {
            "live" => {
                current.live_trading = true;
                current.paper_trading = false;
            }
            _ => {
                current.paper_trading = true;
                current.live_trading = false;
            }
        }
    }
    if let Some(k) = patch.get("mexc_api_key").and_then(|v| v.as_str()) {
        if !is_masked_credential(k) {
            current.mexc_api_key = k.to_string();
        }
    }
    if let Some(s) = patch.get("mexc_api_secret").and_then(|v| v.as_str()) {
        if !is_masked_credential(s) {
            current.mexc_api_secret = s.to_string();
        }
    }
    if let Some(p) = patch.get("paper_trading").and_then(|v| v.as_bool()) {
        current.paper_trading = p;
        if p {
            current.live_trading = false;
        }
    }
    if let Some(l) = patch.get("live_trading").and_then(|v| v.as_bool()) {
        current.live_trading = l;
        if l {
            current.paper_trading = false;
        }
    }
    if patch.get("clear_credentials").and_then(|v| v.as_bool()) == Some(true) {
        current.clear_credentials();
    }
    // Telegram fields
    if let Some(t) = patch.get("telegram_bot_token").and_then(|v| v.as_str()) {
        if !is_masked_credential(t) {
            current.telegram_bot_token = t.to_string();
        }
    }
    if let Some(c) = patch.get("telegram_chat_id").and_then(|v| v.as_str()) {
        if !is_masked_credential(c) {
            current.telegram_chat_id = c.to_string();
        }
    }
    if let Some(e) = patch.get("telegram_enabled").and_then(|v| v.as_bool()) {
        current.telegram_enabled = e;
    }
    if let Some(evts) = patch.get("telegram_events").and_then(|v| v.as_array()) {
        current.telegram_events = evts
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
    }
    if patch.get("clear_telegram").and_then(|v| v.as_bool()) == Some(true) {
        current.clear_telegram();
    }
    current
}
