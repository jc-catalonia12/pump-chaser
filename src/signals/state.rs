use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::exchange::{KlineBar, TickerSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Long,
    Short,
}

pub struct SymbolState {
    pub symbol: String,
    pub prices: Vec<f64>,
    pub volumes: Vec<f64>,
    pub klines: Vec<KlineBar>,
    pub last_ticker: Option<TickerSnapshot>,
    pub last_confluence_at: Option<DateTime<Utc>>,
    pub last_scanned_at: Option<DateTime<Utc>>,
}

impl SymbolState {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            prices: Vec::new(),
            volumes: Vec::new(),
            klines: Vec::new(),
            last_ticker: None,
            last_confluence_at: None,
            last_scanned_at: None,
        }
    }

    pub fn update_ticker(&mut self, ticker: &TickerSnapshot) {
        self.last_ticker = Some(ticker.clone());
        self.prices.push(ticker.last_price);
        self.volumes.push(ticker.volume24.max(0.0));
        if self.prices.len() > 120 {
            self.prices.drain(0..self.prices.len() - 120);
        }
        if self.volumes.len() > 120 {
            self.volumes.drain(0..self.volumes.len() - 120);
        }
    }

    pub fn update_klines(&mut self, bars: Vec<KlineBar>) {
        self.klines = bars;
    }
}

pub type SymbolStates = HashMap<String, SymbolState>;
