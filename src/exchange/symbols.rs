//! Symbol discovery and liquidity ranking — port of `pump_chaser/data/symbols.py`.

use serde_json::Value;

use crate::config::ScannerConfig;
use crate::exchange::rest::{parse_ticker, MexcRestClient};
use crate::exchange::types::ContractInfo;
use crate::error::Result;

const EXCLUDED_CONCEPT_ZONE_KEYWORDS: &[&str] = &[
    "mc-trade-zone-stock",
    "mc-trade-zone-tradfi",
    "mc-trade-zone-metals",
    "mc-trade-zone-oil",
    "mc-trade-zone-commodities",
];

pub fn is_usdt_m_crypto_perp(raw: &Value, crypto_only: bool) -> bool {
    if raw.get("quoteCoin").and_then(|v| v.as_str()).unwrap_or("").to_uppercase() != "USDT" {
        return false;
    }
    if raw.get("settleCoin").and_then(|v| v.as_str()).unwrap_or("").to_uppercase() != "USDT" {
        return false;
    }
    if raw.get("state").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
        return false;
    }
    if raw.get("isHidden").and_then(|v| v.as_bool()).unwrap_or(false) {
        return false;
    }
    if !raw.get("apiAllowed").and_then(|v| v.as_bool()).unwrap_or(true) {
        return false;
    }

    if !crypto_only {
        return true;
    }

    if raw.get("type").and_then(|v| v.as_i64()).unwrap_or(1) == 2 {
        return false;
    }

    for plate in normalize_concept_plates(raw) {
        for keyword in EXCLUDED_CONCEPT_ZONE_KEYWORDS {
            if plate.contains(keyword) {
                return false;
            }
        }
    }

    true
}

fn normalize_concept_plates(raw: &Value) -> Vec<String> {
    match raw.get("conceptPlate") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_lowercase()))
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => parse_concept_plate_string(s),
        _ => vec![],
    }
}

fn parse_concept_plate_string(s: &str) -> Vec<String> {
    let trimmed = s.trim();
    if trimmed.starts_with('[') {
        if let Ok(Value::Array(arr)) = serde_json::from_str(trimmed) {
            return arr
                .iter()
                .filter_map(|v| v.as_str().map(|x| x.trim().to_lowercase()))
                .filter(|x| !x.is_empty())
                .collect();
        }
    }
    if trimmed.is_empty() {
        vec![]
    } else {
        vec![trimmed.to_lowercase()]
    }
}

pub fn parse_contract(raw: &Value) -> ContractInfo {
    ContractInfo {
        symbol: raw
            .get("symbol")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        base_coin: raw
            .get("baseCoin")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        quote_coin: raw
            .get("quoteCoin")
            .and_then(|v| v.as_str())
            .unwrap_or("USDT")
            .to_string(),
        contract_size: raw
            .get("contractSize")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0),
        state: raw
            .get("state")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        api_allowed: raw
            .get("apiAllowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
        taker_fee_rate: raw
            .get("takerFeeRate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0002),
        is_hidden: raw
            .get("isHidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        price_scale: raw
            .get("priceScale")
            .and_then(|v| v.as_i64())
            .unwrap_or(5) as i32,
        vol_scale: raw
            .get("volScale")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as i32,
        min_vol: raw
            .get("minVol")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0),
        max_vol: raw
            .get("maxVol")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0),
        vol_unit: raw
            .get("volUnit")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0),
        price_unit: raw
            .get("priceUnit")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.00001),
        max_leverage: parse_contract_max_leverage(raw),
    }
}

fn parse_contract_max_leverage(raw: &Value) -> u32 {
    if let Some(v) = raw.get("maxLeverage").and_then(|v| v.as_u64()) {
        return v.min(125) as u32;
    }
    if let Some(arr) = raw.get("leverage").and_then(|v| v.as_array()) {
        let max = arr
            .iter()
            .filter_map(|v| v.as_u64())
            .max()
            .unwrap_or(10);
        return max.min(125) as u32;
    }
    10
}

pub async fn discover_symbols(
    client: &MexcRestClient,
    cfg: &ScannerConfig,
) -> Result<Vec<ContractInfo>> {
    let raw_contracts = client.get_contracts_raw().await?;
    Ok(raw_contracts
        .iter()
        .filter(|raw| is_usdt_m_crypto_perp(raw, cfg.usdt_m_crypto_only))
        .map(parse_contract)
        .collect())
}

pub async fn rank_by_liquidity(
    client: &MexcRestClient,
    symbols: &[String],
    cfg: &ScannerConfig,
) -> Result<(Vec<String>, std::collections::HashMap<String, f64>)> {
    let tickers = client.get_tickers_raw().await?;
    let ticker_map: std::collections::HashMap<String, &Value> = tickers
        .iter()
        .filter_map(|t| {
            t.get("symbol")
                .and_then(|s| s.as_str())
                .map(|sym| (sym.to_string(), t))
        })
        .collect();

    let mut ranked: Vec<(String, f64)> = Vec::new();
    for symbol in symbols {
        let Some(raw) = ticker_map.get(symbol) else {
            continue;
        };
        let t = parse_ticker(raw);
        if t.last_price < cfg.min_price_usdt {
            continue;
        }
        let turnover = if t.amount24 > 0.0 {
            t.amount24
        } else {
            t.volume24 * t.last_price
        };
        if turnover >= cfg.min_24h_turnover_usdt {
            ranked.push((symbol.clone(), turnover));
        }
    }
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top: Vec<String> = ranked
        .iter()
        .take(cfg.max_symbols_kline_poll as usize)
        .map(|(s, _)| s.clone())
        .collect();
    let turnover_map: std::collections::HashMap<String, f64> = ranked
        .into_iter()
        .take(cfg.max_symbols_kline_poll as usize)
        .collect();
    Ok((top, turnover_map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_stock_zone_contract() {
        let raw = json!({
            "symbol": "TSLA_USDT",
            "quoteCoin": "USDT",
            "settleCoin": "USDT",
            "state": 0,
            "apiAllowed": true,
            "conceptPlate": ["mc-trade-zone-stock"]
        });
        assert!(!is_usdt_m_crypto_perp(&raw, true));
    }

    #[test]
    fn accepts_crypto_perp() {
        let raw = json!({
            "symbol": "BTC_USDT",
            "quoteCoin": "USDT",
            "settleCoin": "USDT",
            "state": 0,
            "apiAllowed": true,
            "type": 1
        });
        assert!(is_usdt_m_crypto_perp(&raw, true));
    }
}
