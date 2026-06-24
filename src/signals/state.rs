use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::{
    exchange::{KlineBar, TickerSnapshot},
    signals::sniper::PendingSetup,
};

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
    /// Higher-timeframe klines (e.g. 15m/30m) for structural bias checks.
    pub htf_klines: Vec<KlineBar>,
    pub last_ticker: Option<TickerSnapshot>,
    pub last_confluence_at: Option<DateTime<Utc>>,
    pub last_scanned_at: Option<DateTime<Utc>>,
    pub last_pump_at: Option<DateTime<Utc>>,
    /// HTF setup waiting for a 1m sniper trigger before firing.
    pub pending_setup: Option<PendingSetup>,
}

impl SymbolState {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            prices: Vec::new(),
            volumes: Vec::new(),
            klines: Vec::new(),
            htf_klines: Vec::new(),
            last_ticker: None,
            last_confluence_at: None,
            last_scanned_at: None,
            last_pump_at: None,
            pending_setup: None,
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

    pub fn update_htf_klines(&mut self, bars: Vec<KlineBar>) {
        self.htf_klines = bars;
    }
}

pub type SymbolStates = HashMap<String, SymbolState>;
