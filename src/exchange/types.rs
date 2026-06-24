use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickerSnapshot {
    pub symbol: String,
    pub last_price: f64,
    pub volume24: f64,
    #[serde(default)]
    pub amount24: f64,
    pub rise_fall_rate: f64,
    #[serde(default)]
    pub fair_price: f64,
    #[serde(default)]
    pub high24: f64,
    #[serde(default)]
    pub low24: f64,
    #[serde(skip, default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KlineBar {
    pub symbol: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub amount: f64,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractInfo {
    pub symbol: String,
    pub base_coin: String,
    pub quote_coin: String,
    pub contract_size: f64,
    pub state: i32,
    pub api_allowed: bool,
    pub taker_fee_rate: f64,
    #[serde(default)]
    pub is_hidden: bool,
    #[serde(default = "default_price_scale")]
    pub price_scale: i32,
    #[serde(default)]
    pub vol_scale: i32,
    #[serde(default = "default_min_vol")]
    pub min_vol: f64,
    #[serde(default)]
    pub max_vol: f64,
    #[serde(default = "default_vol_unit")]
    pub vol_unit: f64,
    #[serde(default = "default_price_unit")]
    pub price_unit: f64,
    #[serde(default = "default_max_leverage")]
    pub max_leverage: u32,
}

fn default_price_scale() -> i32 {
    5
}

fn default_min_vol() -> f64 {
    1.0
}

fn default_vol_unit() -> f64 {
    1.0
}

fn default_price_unit() -> f64 {
    0.00001
}

fn default_max_leverage() -> u32 {
    10
}
