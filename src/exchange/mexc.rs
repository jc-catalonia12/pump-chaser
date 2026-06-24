//! Unified MEXC exchange facade (REST + optional WebSocket stream).

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex, RwLock};

use crate::config::AppConfig;
use crate::error::Result;
use crate::exchange::rest::MexcRestClient;
use crate::exchange::symbols::{discover_symbols, rank_by_liquidity};
use crate::exchange::types::{ContractInfo, KlineBar, TickerSnapshot};
use crate::exchange::ws::MexcWebSocketClient;

#[derive(Clone)]
pub struct MexcClient {
    config: Arc<AppConfig>,
    rest: Arc<MexcRestClient>,
    ws: Arc<Mutex<MexcWebSocketClient>>,
    latest_tickers: Arc<RwLock<Vec<TickerSnapshot>>>,
}

impl MexcClient {
    pub fn new(config: Arc<AppConfig>) -> Result<Self> {
        let mexc = Arc::new(config.mexc.clone());
        Ok(Self {
            config: config.clone(),
            rest: Arc::new(MexcRestClient::new(mexc.clone())?),
            ws: Arc::new(Mutex::new(MexcWebSocketClient::new(mexc, 5.0))),
            latest_tickers: Arc::new(RwLock::new(Vec::new())),
        })
    }

    pub async fn ping(&self) -> bool {
        self.rest.ping().await
    }

    pub async fn health_check(&self) -> Result<bool> {
        Ok(self.ping().await)
    }

    pub async fn get_tickers(&self) -> Result<Vec<TickerSnapshot>> {
        self.rest.get_tickers().await
    }

    pub async fn get_klines(&self, symbol: &str, interval: &str) -> Result<Vec<KlineBar>> {
        self.rest.get_klines(symbol, interval, None, None).await
    }

    /// Historical klines bounded by a unix-second `start`/`end` window. Used to
    /// shadow-resolve past signals against the price action that followed them.
    pub async fn get_klines_range(
        &self,
        symbol: &str,
        interval: &str,
        start: i64,
        end: i64,
    ) -> Result<Vec<KlineBar>> {
        self.rest
            .get_klines(symbol, interval, Some(start), Some(end))
            .await
    }

    pub async fn discover_contracts(&self) -> Result<Vec<ContractInfo>> {
        discover_symbols(self.rest.as_ref(), &self.config.scanner).await
    }

    pub async fn rank_symbols(&self, symbols: &[String]) -> Result<Vec<String>> {
        let (top, _) = rank_by_liquidity(self.rest.as_ref(), symbols, &self.config.scanner).await?;
        Ok(top)
    }

    pub async fn get_symbols(&self) -> Result<Vec<String>> {
        let contracts = self.discover_contracts().await?;
        let symbols: Vec<String> = contracts.into_iter().map(|c| c.symbol).collect();
        self.rank_symbols(&symbols).await
    }

    /// Start WebSocket ticker stream; updates cache and forwards batches to `forward_tx` if set.
    pub async fn start_ticker_stream(
        &self,
        forward_tx: Option<tokio::sync::mpsc::Sender<Vec<TickerSnapshot>>>,
    ) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<Vec<TickerSnapshot>>(32);
        let cache = self.latest_tickers.clone();
        tokio::spawn(async move {
            while let Some(batch) = rx.recv().await {
                {
                    let mut w = cache.write().await;
                    *w = batch.clone();
                }
                if let Some(fwd) = &forward_tx {
                    let _ = fwd.send(batch).await;
                }
            }
        });

        let mut ws = self.ws.lock().await;
        ws.start(tx);
        Ok(())
    }

    pub async fn stop_ticker_stream(&self) {
        let mut ws = self.ws.lock().await;
        ws.stop().await;
    }

    pub async fn cached_tickers(&self) -> Vec<TickerSnapshot> {
        self.latest_tickers.read().await.clone()
    }

    pub async fn is_ws_running(&self) -> bool {
        self.ws.lock().await.is_running()
    }
}
