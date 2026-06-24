//! MEXC Futures WebSocket client — port of `pump_chaser/data/mexc_ws.py`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

use crate::config::MexcConfig;
use crate::exchange::rest::parse_ticker;
use crate::exchange::TickerSnapshot;

pub type TickerHandler = mpsc::Sender<Vec<TickerSnapshot>>;

pub struct MexcWebSocketClient {
    config: Arc<MexcConfig>,
    running: Arc<AtomicBool>,
    reconnect_delay_sec: f64,
    handle: Option<JoinHandle<()>>,
}

impl MexcWebSocketClient {
    pub fn new(config: Arc<MexcConfig>, reconnect_delay_sec: f64) -> Self {
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            reconnect_delay_sec,
            handle: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Start the reconnect loop; ticker batches are sent on `tx`.
    pub fn start(&mut self, tx: TickerHandler) {
        if self.handle.is_some() {
            return;
        }
        self.running.store(true, Ordering::SeqCst);
        let config = self.config.clone();
        let running = self.running.clone();
        let delay = self.reconnect_delay_sec;

        self.handle = Some(tokio::spawn(async move {
            run_ws_loop(config, running, delay, tx).await;
        }));
    }

    pub async fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

async fn run_ws_loop(
    config: Arc<MexcConfig>,
    running: Arc<AtomicBool>,
    reconnect_delay_sec: f64,
    tx: TickerHandler,
) {
    let mut attempts: u32 = 0;
    while running.load(Ordering::SeqCst) {
        match connect_async(&config.ws_url).await {
            Ok((ws, _)) => {
                attempts = 0;
                info!("MEXC WebSocket connected");
                let (mut write, mut read) = ws.split();

                let sub = serde_json::json!({
                    "method": "sub.tickers",
                    "param": {},
                    "gzip": false
                });
                if write
                    .send(Message::Text(sub.to_string().into()))
                    .await
                    .is_err()
                {
                    continue;
                }

                while running.load(Ordering::SeqCst) {
                    match read.next().await {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text) {
                                if payload.get("channel").and_then(|v| v.as_str())
                                    == Some("push.tickers")
                                {
                                    let tickers = parse_ticker_batch(&payload);
                                    if !tickers.is_empty() && tx.send(tickers).await.is_err() {
                                        running.store(false, Ordering::SeqCst);
                                        break;
                                    }
                                }
                            }
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = write.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Err(exc)) => {
                            warn!("MEXC WS read error: {exc}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(exc) => {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                attempts += 1;
                let backoff = (reconnect_delay_sec * 2f64.powi(attempts.saturating_sub(1).min(4) as i32))
                    .min(60.0);
                warn!("MEXC WS connect error: {exc} — reconnect in {backoff:.1}s");
                tokio::time::sleep(Duration::from_secs_f64(backoff)).await;
            }
        }
    }
    info!("MEXC WebSocket stopped");
}

fn parse_ticker_batch(payload: &serde_json::Value) -> Vec<TickerSnapshot> {
    let data = payload.get("data");
    let items: Vec<&serde_json::Value> = match data {
        Some(serde_json::Value::Array(arr)) => arr.iter().collect(),
        Some(obj @ serde_json::Value::Object(_)) => vec![obj],
        _ => return vec![],
    };
    items
        .into_iter()
        .filter(|d| d.get("symbol").is_some())
        .map(parse_ticker)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_ticker_batch_array() {
        let payload = json!({
            "channel": "push.tickers",
            "data": [
                {"symbol": "BTC_USDT", "lastPrice": 1.0, "volume24": 2.0, "riseFallRate": 0.01}
            ]
        });
        let tickers = parse_ticker_batch(&payload);
        assert_eq!(tickers.len(), 1);
        assert_eq!(tickers[0].symbol, "BTC_USDT");
    }
}
