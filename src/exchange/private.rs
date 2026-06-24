//! MEXC Futures private REST API (HMAC-SHA256 signing).

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{json, Value};
use sha2::Sha256;
use tracing::{debug, warn};

use crate::config::MexcConfig;
use crate::error::{BotError, Result};
use crate::models::PositionSide;
use crate::utils::secrets::UserSecrets;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Copy)]
pub struct AssetBalance {
    pub equity: f64,
    pub available: f64,
}

impl AssetBalance {
    /// Equity used to anchor risk limits (matches Python `wallet.py`).
    pub fn anchor_equity(&self) -> f64 {
        if self.equity > 0.0 {
            self.equity
        } else {
            self.available
        }
    }
}

fn parse_json_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn asset_balance_from_value(data: &Value) -> AssetBalance {
    let equity = data.get("equity").and_then(parse_json_f64).unwrap_or(0.0);
    let available = data
        .get("availableBalance")
        .or_else(|| data.get("available"))
        .and_then(parse_json_f64)
        .unwrap_or(0.0);
    AssetBalance { equity, available }
}

#[derive(Clone)]
pub struct MexcPrivateClient {
    http: Client,
    base_url: String,
    api_key: String,
    api_secret: String,
}

impl MexcPrivateClient {
    pub fn from_secrets(mexc: &MexcConfig, secrets: &UserSecrets) -> Self {
        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs_f64(
                    mexc.request_timeout_sec.max(1.0),
                ))
                .build()
                .unwrap_or_default(),
            base_url: mexc.rest_base_url.trim_end_matches('/').to_string(),
            api_key: secrets.mexc_api_key.clone(),
            api_secret: secrets.mexc_api_secret.clone(),
        }
    }

    pub fn has_credentials(&self) -> bool {
        !self.api_key.is_empty() && !self.api_secret.is_empty()
    }

    fn sign(&self, sign_target: &str, req_time: &str) -> String {
        let payload = format!("{}{}{}", self.api_key, req_time, sign_target);
        let mut mac =
            HmacSha256::new_from_slice(self.api_secret.as_bytes()).expect("HMAC key length");
        mac.update(payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn req_time_ms() -> String {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis().to_string())
            .unwrap_or_else(|_| "0".into())
    }

    fn sorted_query(params: &BTreeMap<String, String>) -> String {
        params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    }

    async fn signed_get(&self, path: &str, params: BTreeMap<String, String>) -> Result<Value> {
        if !self.has_credentials() {
            return Err(BotError::Config("MEXC API credentials not configured".into()));
        }
        let req_time = Self::req_time_ms();
        let query = Self::sorted_query(&params);
        let sign_target = if query.is_empty() {
            String::new()
        } else {
            query.clone()
        };
        let signature = self.sign(&sign_target, &req_time);
        let url = if query.is_empty() {
            format!("{}{}", self.base_url, path)
        } else {
            format!("{}{}?{}", self.base_url, path, query)
        };

        let resp = self
            .http
            .get(&url)
            .header("User-Agent", "MEXC-Trading-Bot-Rust/1.0")
            .header("Accept", "application/json")
            .header("ApiKey", &self.api_key)
            .header("Request-Time", &req_time)
            .header("Signature", &signature)
            .header("Recv-Window", "10000")
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| BotError::Exchange(e.to_string()))?;

        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| BotError::Exchange(e.to_string()))?;
        if !status.is_success() {
            return Err(BotError::Exchange(format!("HTTP {status}: {body}")));
        }
        if body.get("success").and_then(|v| v.as_bool()) == Some(false) {
            let msg = body
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("MEXC API error");
            return Err(BotError::Exchange(msg.to_string()));
        }
        Ok(body)
    }

    async fn signed_post(&self, path: &str, body: &Value) -> Result<Value> {
        if !self.has_credentials() {
            return Err(BotError::Config("MEXC API credentials not configured".into()));
        }
        let req_time = Self::req_time_ms();
        let body_str = serde_json::to_string(body).map_err(|e| BotError::Exchange(e.to_string()))?;
        let signature = self.sign(&body_str, &req_time);
        let url = format!("{}{}", self.base_url, path);

        debug!("MEXC POST {} body={}", path, body_str);

        let resp = self
            .http
            .post(&url)
            .header("User-Agent", "MEXC-Trading-Bot-Rust/1.0")
            .header("Accept", "application/json")
            .header("ApiKey", &self.api_key)
            .header("Request-Time", &req_time)
            .header("Signature", &signature)
            .header("Recv-Window", "10000")
            .header("Content-Type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| BotError::Exchange(e.to_string()))?;

        let status = resp.status();
        let result: Value = resp
            .json()
            .await
            .map_err(|e| BotError::Exchange(e.to_string()))?;
        if !status.is_success() {
            return Err(BotError::Exchange(format!("HTTP {status}: {result}")));
        }
        Ok(result)
    }

    pub async fn get_assets(&self) -> Result<Vec<Value>> {
        let body = self
            .signed_get("/api/v1/private/account/assets", BTreeMap::new())
            .await?;
        let data = &body["data"];
        if let Some(arr) = data.as_array() {
            return Ok(arr.clone());
        }
        if data.is_object() {
            return Ok(vec![data.clone()]);
        }
        Ok(vec![])
    }

    pub async fn get_asset(&self, currency: &str) -> Result<AssetBalance> {
        let path = format!("/api/v1/private/account/asset/{currency}");
        let body = self.signed_get(&path, BTreeMap::new()).await?;
        let data = &body["data"];
        if data.is_object() {
            return Ok(asset_balance_from_value(data));
        }
        Err(BotError::Exchange(format!(
            "unexpected asset response for {currency}"
        )))
    }

    /// Resolve USDT balance — prefers `/account/assets` list, falls back to per-currency path.
    pub async fn get_usdt_balance(&self) -> Result<AssetBalance> {
        if let Ok(assets) = self.get_assets().await {
            if let Some(row) = assets
                .iter()
                .find(|a| a.get("currency").and_then(|v| v.as_str()) == Some("USDT"))
            {
                return Ok(asset_balance_from_value(row));
            }
        }
        self.get_asset("USDT").await
    }

    pub async fn get_open_positions(&self) -> Result<Vec<Value>> {
        let body = self
            .signed_get("/api/v1/private/position/open_positions", BTreeMap::new())
            .await?;
        Ok(body["data"]
            .as_array()
            .cloned()
            .unwrap_or_default())
    }

    pub async fn submit_order(&self, payload: Value) -> Result<Value> {
        self.signed_post("/api/v1/private/order/create", &payload)
            .await
    }

    /// Place a trigger (plan) order — used for stop-loss and take-profit.
    /// MEXC endpoint: POST /api/v1/private/planorder/place/v2
    pub async fn place_plan_order(&self, payload: Value) -> Result<Value> {
        self.signed_post("/api/v1/private/planorder/place/v2", &payload)
            .await
    }

    /// Cancel **all** open plan orders (SL/TP triggers) for a symbol.
    /// Called after a full position close so dangling plan orders don't re-open
    /// a position when price drifts back through the trigger level.
    /// MEXC endpoint: POST /api/v1/private/planorder/cancel_all
    pub async fn cancel_all_plan_orders(&self, symbol: &str) -> Result<Value> {
        let payload = serde_json::json!({ "symbol": symbol });
        self.signed_post("/api/v1/private/planorder/cancel_all", &payload)
            .await
    }

    pub async fn change_leverage(
        &self,
        symbol: &str,
        leverage: i32,
        side: PositionSide,
    ) -> Result<Value> {
        let position_type = match side {
            PositionSide::Long => 1,
            PositionSide::Short => 2,
        };
        let body = json!({
            "symbol": symbol,
            "leverage": leverage,
            "openType": 2,
            "positionType": position_type,
        });
        self.signed_post("/api/v1/private/position/change_leverage", &body)
            .await
    }

    pub async fn get_symbol_max_leverage(&self, symbol: &str, side: PositionSide) -> Result<i32> {
        let mut params = BTreeMap::new();
        params.insert("symbol".into(), symbol.into());
        let position_type = match side {
            PositionSide::Long => 1,
            PositionSide::Short => 2,
        };
        params.insert("positionType".into(), position_type.to_string());
        let body = self
            .signed_get("/api/v1/private/position/leverage", params)
            .await?;
        let lev = body["data"]
            .get("maxLeverage")
            .or_else(|| body["data"].get("leverage"))
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0) as i32;
        Ok(lev)
    }

    /// Reconcile local open positions with exchange (best-effort).
    pub async fn sync_positions(&self) -> Result<Vec<Value>> {
        match self.get_open_positions().await {
            Ok(pos) => Ok(pos),
            Err(e) => {
                warn!("position sync failed: {e}");
                Err(e)
            }
        }
    }
}
