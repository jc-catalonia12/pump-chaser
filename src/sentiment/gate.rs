//! Sentiment gate — block trades against strong adverse news bias.

use crate::config::SentimentConfig;
use crate::signals::state::Side;

#[derive(Debug, Clone)]
pub struct SentimentGateResult {
    pub allows: bool,
    pub global_score: f64,
    pub symbol_score: Option<f64>,
    pub fear_greed: Option<i64>,
    pub block_reason: Option<String>,
}

pub fn sentiment_allows(
    side: Side,
    symbol: &str,
    global_score: f64,
    symbol_score: Option<f64>,
    fear_greed: Option<i64>,
    cfg: &SentimentConfig,
) -> SentimentGateResult {
    if !cfg.enabled {
        return SentimentGateResult {
            allows: true,
            global_score,
            symbol_score,
            fear_greed,
            block_reason: None,
        };
    }

    let base = symbol_base(symbol);
    let sym_score = symbol_score.or_else(|| {
        if base.is_empty() {
            None
        } else {
            None
        }
    });

    // Fear & Greed nudges global score: extreme fear = more negative, greed = positive.
    let mut adj_global = global_score;
    if let Some(fg) = fear_greed {
        adj_global += (fg as f64 - 50.0) / 100.0;
        adj_global = adj_global.clamp(-1.0, 1.0);
    }

    let side_long = matches!(side, Side::Long);
    if side_long && adj_global < cfg.block_long_below {
        return SentimentGateResult {
            allows: false,
            global_score: adj_global,
            symbol_score: sym_score,
            fear_greed,
            block_reason: Some(format!(
                "Global sentiment {:.2} blocks long (threshold {:.2})",
                adj_global, cfg.block_long_below
            )),
        };
    }
    if !side_long && adj_global > cfg.block_short_above {
        return SentimentGateResult {
            allows: false,
            global_score: adj_global,
            symbol_score: sym_score,
            fear_greed,
            block_reason: Some(format!(
                "Global sentiment {:.2} blocks short (threshold {:.2})",
                adj_global, cfg.block_short_above
            )),
        };
    }

    if let Some(ss) = sym_score {
        let thresh = cfg.symbol_block_threshold;
        if side_long && ss < -thresh {
            return SentimentGateResult {
                allows: false,
                global_score: adj_global,
                symbol_score: Some(ss),
                fear_greed,
                block_reason: Some(format!("{base} sentiment {ss:.2} blocks long")),
            };
        }
        if !side_long && ss > thresh {
            return SentimentGateResult {
                allows: false,
                global_score: adj_global,
                symbol_score: Some(ss),
                fear_greed,
                block_reason: Some(format!("{base} sentiment {ss:.2} blocks short")),
            };
        }
    }

    SentimentGateResult {
        allows: true,
        global_score: adj_global,
        symbol_score: sym_score,
        fear_greed,
        block_reason: None,
    }
}

pub fn symbol_base(symbol: &str) -> String {
    symbol.split('_').next().unwrap_or(symbol).to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SentimentConfig;

    #[test]
    fn blocks_long_on_negative_global() {
        let cfg = SentimentConfig::default();
        let r = sentiment_allows(Side::Long, "SOL_USDT", -0.6, None, None, &cfg);
        assert!(!r.allows);
    }
}
