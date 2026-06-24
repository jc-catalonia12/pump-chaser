//! Pre-trade risk filters — port of `risk/filters.py`.

use crate::config::{RiskConfig, ScannerConfig};
use crate::exchange::{KlineBar, TickerSnapshot};
use crate::signals::indicators::atr_pct;
use crate::signals::Side;

pub fn passes_risk_filters(
    _symbol: &str,
    ticker: &TickerSnapshot,
    klines: &[KlineBar],
    risk: &RiskConfig,
    scanner: &ScannerConfig,
    funding_rate: Option<f64>,
    side: Side,
    skip_turnover: bool,
) -> bool {
    if ticker.last_price < scanner.min_price_usdt {
        return false;
    }
    if !skip_turnover {
        let turnover = if ticker.amount24 > 0.0 {
            ticker.amount24
        } else {
            ticker.volume24 * ticker.last_price
        };
        if turnover < scanner.min_24h_turnover_usdt {
            return false;
        }
    }
    if let Some(rate) = funding_rate {
        if !funding_allows_side(side, rate, risk) {
            return false;
        }
    }
    if klines.len() >= 15 {
        let bars: Vec<(f64, f64, f64)> = klines.iter().map(|b| (b.high, b.low, b.close)).collect();
        if atr_pct(&bars) > risk.max_atr_pct {
            return false;
        }
    }
    true
}

fn funding_allows_side(side: Side, rate: f64, risk: &RiskConfig) -> bool {
    if rate.abs() > risk.max_funding_rate_abs {
        return false;
    }
    match side {
        Side::Long => rate <= risk.max_funding_rate_abs,
        Side::Short => rate >= -risk.max_funding_rate_abs,
    }
}
