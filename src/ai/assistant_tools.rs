//! Tool definitions and execution for the in-app assistant (Ollama tool calling).

use std::net::IpAddr;

use serde_json::{json, Value};
use tracing::info;

use crate::error::{BotError, Result};
use crate::settings_commit::commit_user_settings_patch;
use crate::user_settings::{settings_file_path, settings_schema, user_settings_values};
use crate::AppState;

/// Cap text returned to Ollama from a tool call.
pub fn cap_tool_result(mut content: String, max_chars: usize) -> String {
    let max_chars = max_chars.max(256).min(32_000);
    if content.chars().count() <= max_chars {
        return content;
    }
    let mut end = max_chars;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    content.truncate(end);
    content.push_str("\n… [truncated for model context]");
    content
}

pub fn ollama_tool_defs(web_enabled: bool, settings_write_enabled: bool) -> Vec<Value> {
    let mut tools = Vec::new();

    if web_enabled {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch a public HTTP/HTTPS web page (news, docs, exchange announcements). Returns plain text excerpt.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "Full http or https URL to fetch"
                        }
                    },
                    "required": ["url"]
                }
            }
        }));
    }

    tools.push(json!({
        "type": "function",
        "function": {
            "name": "get_settings",
            "description": "Read current bot settings from config/settings.yaml (user-editable subset) plus field schema.",
            "parameters": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }
    }));

    if settings_write_enabled {
        tools.push(json!({
            "type": "function",
            "function": {
                "name": "update_settings",
                "description": "Merge a JSON patch into config/settings.yaml (same fields as the Settings UI). Use nested objects, e.g. {\"risk\": {\"max_leverage\": 50}}. Only call when the user explicitly asks to change settings.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patch": {
                            "type": "object",
                            "description": "Partial settings object to deep-merge into the current config"
                        }
                    },
                    "required": ["patch"]
                }
            }
        }));
    }

    tools
}

pub async fn execute_tool(
    state: &AppState,
    name: &str,
    args: &Value,
    web_enabled: bool,
    settings_write_enabled: bool,
    max_fetch_bytes: usize,
    max_tool_result_chars: usize,
) -> String {
    let raw = match name {
        "web_fetch" if web_enabled => {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim();
            if url.is_empty() {
                json!({ "error": "url is required" }).to_string()
            } else {
                info!(url, "assistant web_fetch");
                match web_fetch(url, max_fetch_bytes, max_tool_result_chars).await {
                    Ok(text) => json!({ "url": url, "content": text }).to_string(),
                    Err(exc) => json!({ "error": exc.to_string() }).to_string(),
                }
            }
        }
        "get_settings" => {
            let cfg = state.config.read().unwrap().clone();
            let schema_keys: Vec<String> = settings_schema()
                .iter()
                .flat_map(|s| s.fields.iter().map(|f| f.key.clone()))
                .collect();
            json!({
                "config_path": settings_file_path().display().to_string(),
                "values": user_settings_values(&cfg),
                "editable_fields": schema_keys,
            })
            .to_string()
        }
        "update_settings" if settings_write_enabled => {
            let patch = args.get("patch").cloned().unwrap_or_else(|| args.clone());
            if !patch.is_object() {
                json!({ "error": "patch must be a JSON object" }).to_string()
            } else {
                info!(?patch, "assistant update_settings");
                match commit_user_settings_patch(state, &patch).await {
                    Ok(result) => result.to_string(),
                    Err(exc) => json!({ "error": exc.to_string() }).to_string(),
                }
            }
        }
        "web_fetch" | "update_settings" => {
            json!({ "error": format!("tool {name} is disabled in assistant settings") }).to_string()
        }
        other => json!({ "error": format!("unknown tool: {other}") }).to_string(),
    };
    cap_tool_result(raw, max_tool_result_chars)
}

pub async fn web_fetch(url: &str, max_bytes: usize, max_text_chars: usize) -> Result<String> {
    validate_public_url(url)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("MEXC-Pump-Chaser-Assistant/1.0")
        .build()
        .map_err(|e| BotError::Config(e.to_string()))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| BotError::Config(format!("fetch failed: {e}")))?;

    if let Some(loc) = resp.headers().get(reqwest::header::LOCATION) {
        if let Ok(next) = loc.to_str() {
            validate_public_url(next)?;
        }
    }

    if !resp.status().is_success() {
        return Err(BotError::Config(format!("HTTP {}", resp.status())));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| BotError::Config(e.to_string()))?;
    let cap = max_bytes.max(4096).min(512 * 1024);
    let slice = if bytes.len() > cap {
        &bytes[..cap]
    } else {
        &bytes
    };
    let raw = String::from_utf8_lossy(slice);
    let without_scripts = strip_html_blocks(&raw, &["script", "style", "noscript"]);
    let mut text = if looks_like_html(&without_scripts) {
        html_to_text(&without_scripts)
    } else {
        without_scripts
    };
    text = collapse_whitespace(&text);
    Ok(cap_tool_result(text, max_text_chars))
}

fn validate_public_url(url: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url).map_err(|e| BotError::Config(e.to_string()))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(BotError::Config("only http and https URLs are allowed".into()));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| BotError::Config("URL must have a host".into()))?;
    if is_blocked_host(host) {
        return Err(BotError::Config("private or local URLs are not allowed".into()));
    }
    Ok(())
}

fn is_blocked_host(host: &str) -> bool {
    let lower = host.to_lowercase();
    if lower == "localhost"
        || lower.ends_with(".localhost")
        || lower == "0.0.0.0"
        || lower.ends_with(".local")
        || lower.ends_with(".internal")
    {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_private_ip(ip);
    }
    false
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 169 && v4.octets()[1] == 254
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local(),
    }
}

fn looks_like_html(text: &str) -> bool {
    let sample = text.get(..512).unwrap_or(text).to_lowercase();
    sample.contains("<html") || sample.contains("<body") || sample.contains("<!doctype")
}

fn strip_html_blocks(html: &str, tags: &[&str]) -> String {
    let mut result = html.to_string();
    for tag in tags {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        loop {
            let lower = result.to_lowercase();
            let Some(start) = lower.find(&open) else {
                break;
            };
            let Some(rel_end) = lower[start..].find(&close) else {
                result.truncate(start);
                break;
            };
            let end = start + rel_end + close.len();
            result.replace_range(start..end, " ");
        }
    }
    result
}

fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}
