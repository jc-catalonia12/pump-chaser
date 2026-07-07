//! BTC/ETH higher-timeframe macro gate (reusable market-context math).

use crate::exchange::KlineBar;
use crate::signals::state::Side;
use crate::signals::zones::market_structure_supports;

/// BTC + ETH higher-timeframe bars for macro gate.
#[derive(Debug, Clone, Default)]
pub struct MacroHtfState {
    pub btc_klines: Vec<KlineBar>,
    pub eth_klines: Vec<KlineBar>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroAsset {
    Btc,
    Eth,
}

/// Macro filter settings (callers supply their own values).
#[derive(Debug, Clone, Copy)]
pub struct MacroFilterConfig {
    pub enabled: bool,
    pub lookback_bars: u32,
    pub min_move_pct: f64,
}

/// Which macro assets to check for a given symbol.
pub fn macro_assets_for_symbol(symbol: &str) -> Vec<MacroAsset> {
    match symbol {
        "BTC_USDT" => vec![MacroAsset::Eth],
        "ETH_USDT" => vec![MacroAsset::Btc],
        _ => vec![MacroAsset::Btc, MacroAsset::Eth],
    }
}

#[derive(Debug, Clone)]
pub struct MacroAssetEval {
    pub allows: bool,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct MacroGateResult {
    pub allows: bool,
    pub btc_ok: bool,
    pub eth_ok: bool,
    pub btc_detail: String,
    pub eth_detail: String,
    pub block_reason: Option<String>,
}

pub fn macro_asset_allows(
    side: Side,
    klines: &[KlineBar],
    cfg: &MacroFilterConfig,
    label: &str,
) -> MacroAssetEval {
    if klines.len() < 10 {
        return MacroAssetEval {
            allows: true,
            detail: format!("{label} HTF warming"),
        };
    }
    let lookback = cfg.lookback_bars as usize;
    let move_pct = htf_move_pct(klines, lookback.min(klines.len().saturating_sub(1)).max(2));
    let bearish = market_structure_supports(klines, Side::Short, lookback)
        || move_pct <= -cfg.min_move_pct;
    let bullish = market_structure_supports(klines, Side::Long, lookback)
        || move_pct >= cfg.min_move_pct;

    let allows = match side {
        Side::Long => !bearish,
        Side::Short => !bullish,
    };
    let detail = format!("{label} HTF {move_pct:+.2}%");
    MacroAssetEval { allows, detail }
}

pub fn macro_allows_for_symbol(
    side: Side,
    symbol: &str,
    macro_htf: &MacroHtfState,
    cfg: &MacroFilterConfig,
) -> MacroGateResult {
    if !cfg.enabled {
        return MacroGateResult {
            allows: true,
            btc_ok: true,
            eth_ok: true,
            btc_detail: String::new(),
            eth_detail: String::new(),
            block_reason: None,
        };
    }

    let btc = macro_asset_allows(side, &macro_htf.btc_klines, cfg, "BTC");
    let eth = macro_asset_allows(side, &macro_htf.eth_klines, cfg, "ETH");
    let assets = macro_assets_for_symbol(symbol);

    let btc_required = assets.contains(&MacroAsset::Btc);
    let eth_required = assets.contains(&MacroAsset::Eth);
    let btc_ok = !btc_required || btc.allows;
    let eth_ok = !eth_required || eth.allows;
    let allows = btc_ok && eth_ok;

    let block_reason = if allows {
        None
    } else if !btc_ok && btc_required {
        Some(format!(
            "{} macro blocks {} ({})",
            btc.detail,
            side_label(side),
            symbol
        ))
    } else if !eth_ok && eth_required {
        Some(format!(
            "{} macro blocks {} ({})",
            eth.detail,
            side_label(side),
            symbol
        ))
    } else {
        Some("BTC/ETH macro".into())
    };

    MacroGateResult {
        allows,
        btc_ok,
        eth_ok,
        btc_detail: btc.detail,
        eth_detail: eth.detail,
        block_reason,
    }
}

pub fn macro_allows(side: Side, macro_htf: &MacroHtfState, cfg: &MacroFilterConfig) -> bool {
    macro_allows_for_symbol(side, "ALT_USDT", macro_htf, cfg).allows
}

fn side_label(side: Side) -> &'static str {
    match side {
        Side::Long => "long",
        Side::Short => "short",
    }
}

pub fn htf_move_pct(klines: &[KlineBar], bars: usize) -> f64 {
    if klines.len() < bars + 1 {
        return 0.0;
    }
    let start = klines[klines.len() - bars - 1].close;
    let end = klines.last().map(|b| b.close).unwrap_or(start);
    if start <= 0.0 {
        return 0.0;
    }
    (end - start) / start * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::KlineBar;

    fn bearish_klines(n: usize, start: f64, step: f64) -> Vec<KlineBar> {
        (0..n)
            .map(|i| {
                let close = start - step * i as f64;
                KlineBar {
                    symbol: "ETH_USDT".into(),
                    open: close,
                    high: close * 1.001,
                    low: close * 0.999,
                    close,
                    volume: 1000.0,
                    amount: 1000.0 * close,
                    timestamp: i as i64,
                }
            })
            .collect()
    }

    #[test]
    fn btc_long_checks_eth_only() {
        let cfg = MacroFilterConfig {
            enabled: true,
            lookback_bars: 20,
            min_move_pct: 0.5,
        };
        let macro_htf = MacroHtfState {
            btc_klines: bearish_klines(30, 100.0, 0.5),
            eth_klines: bearish_klines(30, 50.0, 0.2),
        };
        let result = macro_allows_for_symbol(Side::Long, "BTC_USDT", &macro_htf, &cfg);
        assert!(!result.allows);
        assert!(result.btc_ok);
        assert!(!result.eth_ok);
    }

    #[test]
    fn eth_long_checks_btc_only() {
        let cfg = MacroFilterConfig {
            enabled: true,
            lookback_bars: 20,
            min_move_pct: 0.5,
        };
        let macro_htf = MacroHtfState {
            btc_klines: bearish_klines(30, 100.0, 0.2),
            eth_klines: bearish_klines(30, 50.0, 0.5),
        };
        let result = macro_allows_for_symbol(Side::Long, "ETH_USDT", &macro_htf, &cfg);
        assert!(!result.allows);
        assert!(!result.btc_ok);
        assert!(result.eth_ok);
    }

    #[test]
    fn alt_checks_both_macro_assets() {
        let assets = macro_assets_for_symbol("SOL_USDT");
        assert_eq!(assets.len(), 2);
        let cfg = MacroFilterConfig {
            enabled: true,
            lookback_bars: 20,
            min_move_pct: 0.5,
        };
        let macro_htf = MacroHtfState {
            btc_klines: bearish_klines(30, 100.0, 0.2),
            eth_klines: vec![],
        };
        let result = macro_allows_for_symbol(Side::Long, "SOL_USDT", &macro_htf, &cfg);
        assert!(!result.allows);
    }
}
