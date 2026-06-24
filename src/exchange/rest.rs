//! MEXC Futures REST client — port of `pump_chaser/data/mexc_rest.py`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::warn;

use crate::config::MexcConfig;
use crate::error::{BotError, Result};
use crate::exchange::types::{KlineBar, TickerSnapshot};

pub struct MexcRestClient {
    config: Arc<MexcConfig>,
    http: Client,
    last_request_at: Mutex<Instant>,
}

impl MexcRestClient {
    pub fn new(config: Arc<MexcConfig>) -> Result<Self> {
        let timeout = Duration::from_secs_f64(config.request_timeout_sec.max(1.0));
        let http = Client::builder().timeout(timeout).build()?;
        Ok(Self {
            config,
            http,
            last_request_at: Mutex::new(Instant::now() - Duration::from_secs(60)),
        })
    }

    async fn throttle(&self) {
        let delay = Duration::from_millis(self.config.rate_limit_delay_ms);
        let mut last = self.last_request_at.lock().await;
        let elapsed = last.elapsed();
        if elapsed < delay {
            tokio::time::sleep(delay - elapsed).await;
        }
        *last = Instant::now();
    }

    async fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Value> {
        // MEXC occasionally returns a truncated/non-JSON body (transient gateway
        // hiccups, rate limiting). Retry a couple of times before giving up so a
        // single bad response does not drop a symbol's klines for a whole cycle.
        let url = format!("{}{}", self.config.rest_base_url, path);
        let mut last_err: Option<BotError> = None;

        for attempt in 0..3 {
            self.throttle().await;
            let resp = match self.http.get(&url).query(params).send().await {
                Ok(r) => r,
                Err(exc) => {
                    last_err = Some(exc.into());
                    continue;
                }
            };
            if let Err(exc) = resp.error_for_status_ref() {
                // 4xx are not worth retrying; 5xx may recover.
                let status = exc.status();
                last_err = Some(exc.into());
                if status.map(|s| s.is_client_error()).unwrap_or(false) {
                    break;
                }
                continue;
            }

            // Read the raw body once and parse manually so a decode failure can be
            // retried instead of surfacing as "error decoding response body".
            let body = match resp.bytes().await {
                Ok(b) => b,
                Err(exc) => {
                    last_err = Some(exc.into());
                    continue;
                }
            };
            let payload: Value = match serde_json::from_slice(&body) {
                Ok(v) => v,
                Err(exc) => {
                    last_err = Some(BotError::Exchange(format!(
                        "decode failed (attempt {}): {exc}",
                        attempt + 1
                    )));
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
            };

            let success = payload.get("success").and_then(|v| v.as_bool()).unwrap_or(true);
            let code = payload.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
            if !success && code != 0 {
                return Err(BotError::Exchange(format!("MEXC API error: {payload}")));
            }

            return Ok(payload.get("data").cloned().unwrap_or(payload));
        }

        Err(last_err.unwrap_or_else(|| BotError::Exchange("request failed".into())))
    }

    pub async fn ping(&self) -> bool {
        match self.get("/api/v1/contract/ping", &[]).await {
            Ok(_) => true,
            Err(exc) => {
                warn!("MEXC ping failed: {exc}");
                false
            }
        }
    }

    pub async fn get_contracts_raw(&self) -> Result<Vec<Value>> {
        let data = self.get("/api/v1/contract/detail", &[]).await?;
        normalize_array(data)
    }

    pub async fn get_tickers_raw(&self) -> Result<Vec<Value>> {
        let data = self.get("/api/v1/contract/ticker", &[]).await?;
        normalize_array(data)
    }

    pub async fn get_tickers(&self) -> Result<Vec<TickerSnapshot>> {
        let raw = self.get_tickers_raw().await?;
        Ok(raw
            .iter()
            .filter_map(|t| t.get("symbol").and_then(|s| s.as_str()).map(|_| parse_ticker(t)))
            .collect())
    }

    pub async fn get_funding_rate(&self, symbol: &str) -> Result<Value> {
        let path = format!("/api/v1/contract/funding_rate/{symbol}");
        let data = self.get(&path, &[]).await?;
        Ok(if data.is_object() {
            data
        } else {
            Value::Null
        })
    }

    pub async fn get_klines(
        &self,
        symbol: &str,
        interval: &str,
        start: Option<i64>,
        end: Option<i64>,
    ) -> Result<Vec<KlineBar>> {
        let path = format!("/api/v1/contract/kline/{symbol}");
        let mut params: Vec<(&str, String)> = vec![("interval", interval.to_string())];
        if let Some(s) = start {
            params.push(("start", s.to_string()));
        }
        if let Some(e) = end {
            params.push(("end", e.to_string()));
        }
        let data = self.get(&path, &params).await?;
        Ok(parse_klines(symbol, &data))
    }
}

fn normalize_array(data: Value) -> Result<Vec<Value>> {
    match data {
        Value::Array(items) => Ok(items),
        Value::Object(_) => Ok(vec![data]),
        _ => Err(BotError::Exchange("unexpected MEXC response shape".into())),
    }
}

/// Parse a single ticker dict from REST or WS payloads.
pub fn parse_ticker(raw: &Value) -> TickerSnapshot {
    let last_price = raw
        .get("lastPrice")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let volume24 = raw
        .get("volume24")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let amount24 = raw
        .get("amount24")
        .and_then(|v| v.as_f64())
        .unwrap_or(volume24);
    TickerSnapshot {
        symbol: raw
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        last_price,
        volume24,
        amount24,
        rise_fall_rate: raw
            .get("riseFallRate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        fair_price: raw
            .get("fairPrice")
            .and_then(|v| v.as_f64())
            .unwrap_or(last_price),
        high24: raw
            .get("high24Price")
            .or_else(|| raw.get("high24"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        low24: raw
            .get("lower24Price")
            .or_else(|| raw.get("low24"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        timestamp: chrono::Utc::now(),
    }
}

pub fn parse_klines(symbol: &str, data: &Value) -> Vec<KlineBar> {
    let times = data.get("time").and_then(|v| v.as_array());
    let opens = data.get("open").and_then(|v| v.as_array());
    let highs = data.get("high").and_then(|v| v.as_array());
    let lows = data.get("low").and_then(|v| v.as_array());
    let closes = data.get("close").and_then(|v| v.as_array());
    let vols = data.get("vol").and_then(|v| v.as_array());
    let amounts = data.get("amount").and_then(|v| v.as_array());

    let Some(times) = times else {
        return vec![];
    };

    let mut bars = Vec::with_capacity(times.len());
    for (i, ts) in times.iter().enumerate() {
        let Some(timestamp) = ts.as_i64() else {
            continue;
        };
        bars.push(KlineBar {
            symbol: symbol.to_string(),
            open: idx_f64(opens, i),
            high: idx_f64(highs, i),
            low: idx_f64(lows, i),
            close: idx_f64(closes, i),
            volume: idx_f64(vols, i),
            amount: idx_f64(amounts, i),
            timestamp,
        });
    }
    bars
}

fn idx_f64(arr: Option<&Vec<Value>>, i: usize) -> f64 {
    arr.and_then(|a| a.get(i))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_ticker_from_rest_payload() {
        let raw = json!({
            "symbol": "BTC_USDT",
            "lastPrice": 65000.5,
            "volume24": 1.2e9,
            "amount24": 1.1e9,
            "riseFallRate": 0.02,
            "fairPrice": 65001.0,
            "high24Price": 66000.0,
            "lower24Price": 64000.0
        });
        let t = parse_ticker(&raw);
        assert_eq!(t.symbol, "BTC_USDT");
        assert!((t.last_price - 65000.5).abs() < f64::EPSILON);
        assert!((t.rise_fall_rate - 0.02).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_klines_columnar() {
        let data = json!({
            "time": [1000, 1060],
            "open": [1.0, 2.0],
            "high": [1.5, 2.5],
            "low": [0.9, 1.9],
            "close": [1.2, 2.2],
            "vol": [100.0, 200.0],
            "amount": [120.0, 240.0]
        });
        let bars = parse_klines("ETH_USDT", &data);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].timestamp, 1000);
        assert!((bars[1].close - 2.2).abs() < f64::EPSILON);
    }
}
