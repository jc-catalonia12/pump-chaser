//! 15m-style liquidity grab detection: sweep of pooled highs/lows + reclaim.

use crate::exchange::KlineBar;
use crate::signals::Side;

#[derive(Debug, Clone)]
pub struct LiquidityGrabResult {
    pub detected: bool,
    pub score: f64,
    pub message: String,
    pub swept_level: f64,
}

/// Detect a liquidity grab on HTF bars (e.g. 15m).
///
/// **Long:** wick sweeps below a prior swing/equal-low pool, then closes back above it.
/// **Short:** wick sweeps above a prior swing/equal-high pool, then closes back below it.
pub fn detect_liquidity_grab(
    klines: &[KlineBar],
    side: Side,
    lookback: usize,
    max_age_bars: usize,
    sweep_pct: f64,
    min_rejection: f64,
) -> LiquidityGrabResult {
    let none = |msg: &str| LiquidityGrabResult {
        detected: false,
        score: 0.0,
        message: msg.into(),
        swept_level: 0.0,
    };

    let min_bars = lookback.saturating_add(max_age_bars).saturating_add(2);
    if klines.len() < min_bars {
        return none("Insufficient HTF bars for liquidity grab");
    }

    let n = klines.len();
    let pool_end = n.saturating_sub(max_age_bars);
    let pool_start = pool_end.saturating_sub(lookback);
    if pool_end <= pool_start {
        return none("Liquidity pool window too small");
    }

    let pool = &klines[pool_start..pool_end];
    let recent = &klines[pool_end..n];
    let sweep_frac = sweep_pct / 100.0;

    match side {
        Side::Long => {
            let pool_level = pool
                .iter()
                .map(|b| b.low)
                .fold(f64::INFINITY, f64::min);
            if !pool_level.is_finite() || pool_level <= 0.0 {
                return none("No swing-low pool found");
            }
            let sweep_line = pool_level * (1.0 - sweep_frac);

            for bar in recent {
                let swept = bar.low < sweep_line;
                let reclaimed = bar.close > pool_level;
                let rejection = wick_rejection_ratio(bar, side);
                if swept && reclaimed && rejection >= min_rejection {
                    let depth_pct = ((pool_level - bar.low) / pool_level * 100.0).max(0.0);
                    let score = (55.0 + depth_pct * 8.0 + rejection * 25.0).min(100.0);
                    return LiquidityGrabResult {
                        detected: true,
                        score,
                        message: format!(
                            "15m bullish grab: swept {:.4}, reclaimed above pool (wick {:.0}%)",
                            bar.low,
                            rejection * 100.0
                        ),
                        swept_level: pool_level,
                    };
                }
            }
            none("No bullish liquidity grab on HTF")
        }
        Side::Short => {
            let pool_level = pool
                .iter()
                .map(|b| b.high)
                .fold(f64::NEG_INFINITY, f64::max);
            if !pool_level.is_finite() || pool_level <= 0.0 {
                return none("No swing-high pool found");
            }
            let sweep_line = pool_level * (1.0 + sweep_frac);

            for bar in recent {
                let swept = bar.high > sweep_line;
                let reclaimed = bar.close < pool_level;
                let rejection = wick_rejection_ratio(bar, side);
                if swept && reclaimed && rejection >= min_rejection {
                    let depth_pct = ((bar.high - pool_level) / pool_level * 100.0).max(0.0);
                    let score = (55.0 + depth_pct * 8.0 + rejection * 25.0).min(100.0);
                    return LiquidityGrabResult {
                        detected: true,
                        score,
                        message: format!(
                            "15m bearish grab: swept {:.4}, reclaimed below pool (wick {:.0}%)",
                            bar.high,
                            rejection * 100.0
                        ),
                        swept_level: pool_level,
                    };
                }
            }
            none("No bearish liquidity grab on HTF")
        }
    }
}

fn wick_rejection_ratio(bar: &KlineBar, side: Side) -> f64 {
    let range = bar.high - bar.low;
    if range <= f64::EPSILON {
        return 0.0;
    }
    match side {
        Side::Long => (bar.close - bar.low) / range,
        Side::Short => (bar.high - bar.close) / range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::KlineBar;

    fn bar(o: f64, h: f64, l: f64, c: f64) -> KlineBar {
        KlineBar {
            symbol: "TEST_USDT".into(),
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
            amount: 1.0,
            timestamp: 0,
        }
    }

    #[test]
    fn detects_bullish_liquidity_grab() {
        let mut klines = Vec::new();
        for low in [100.0, 100.1, 99.9, 100.0, 100.2, 100.1, 99.95, 100.0] {
            klines.push(bar(low + 0.5, low + 1.0, low, low + 0.6));
        }
        klines.push(bar(100.0, 100.5, 99.5, 100.3));

        let r = detect_liquidity_grab(&klines, Side::Long, 5, 2, 0.05, 0.45);
        assert!(r.detected, "{}", r.message);
    }

    #[test]
    fn detects_bearish_liquidity_grab() {
        let mut klines = Vec::new();
        for high in [101.0, 101.2, 101.1, 101.3, 101.0, 101.15, 101.05, 101.1] {
            klines.push(bar(high - 0.5, high, high - 1.0, high - 0.4));
        }
        klines.push(bar(101.0, 101.8, 100.8, 100.9));

        let r = detect_liquidity_grab(&klines, Side::Short, 5, 2, 0.05, 0.45);
        assert!(r.detected, "{}", r.message);
    }
}
