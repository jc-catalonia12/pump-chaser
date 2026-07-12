//! Bar-level feature engineering — must match `training/schema.py` (V2.0.0).
//!
//! Feature layout (FEATURE_DIM = 24):
//!   EMA ratios, RSI, MACD, ATR%, ADX, VWAP distance, Bollinger width,
//!   volume MA ratio, volatility, candle anatomy, returns/momentum,
//!   trend strength, cyclical hour / day-of-week.
//!
//! Signal context / LLM / sentiment remain available via `MlFeatureContext`
//! for the decision engine, but are **not** model inputs.

use chrono::{Datelike, Timelike, TimeZone, Utc};
use serde_json::Value;

use crate::exchange::KlineBar;

pub const FEATURE_COLUMNS: [&str; 24] = [
    "ema20_ratio",
    "ema50_ratio",
    "ema100_ratio",
    "ema200_ratio",
    "rsi_14",
    "macd_hist",
    "atr_pct",
    "adx_14",
    "vwap_dist",
    "bb_width",
    "volume_ma_ratio",
    "volatility",
    "body_pct",
    "upper_wick_pct",
    "lower_wick_pct",
    "return_1",
    "return_5",
    "return_20",
    "momentum_10",
    "trend_strength",
    "hour_sin",
    "hour_cos",
    "dow_sin",
    "dow_cos",
];

pub const FEATURE_DIM: usize = FEATURE_COLUMNS.len(); // 24

/// Minimum bars required for a meaningful feature vector (EMA200 warm-up).
pub const MIN_BARS_FOR_FEATURES: usize = 200;

/// Extra context for decision-engine gates (not ONNX inputs).
#[derive(Debug, Clone, Default)]
pub struct MlFeatureContext {
    pub btc_htf_move_pct: f64,
    pub global_sentiment: f64,
    pub symbol_htf_move_pct: f64,
    pub funding_rate: f64,
    pub symbol_sentiment: f64,
    pub regime: MarketRegime,
}

#[derive(Debug, Clone, Default)]
pub struct MarketRegime {
    pub trending: bool,
    pub chop: bool,
    pub high_vol: bool,
    pub risk_off: bool,
    pub btc_bias: f64,
    pub confidence: f64,
}

pub fn normalize_feature_vector(features: Option<&[f64]>, dim: usize) -> Vec<f64> {
    let mut vec = match features {
        None | Some([]) => vec![0.0; dim],
        Some(f) => f.iter().take(dim).copied().collect(),
    };
    if vec.len() < dim {
        vec.resize(dim, 0.0);
    }
    vec
}

fn ema(values: &[f64], span: usize) -> Vec<f64> {
    if values.is_empty() {
        return vec![];
    }
    let alpha = 2.0 / (span as f64 + 1.0);
    let mut out = vec![values[0]];
    for &v in &values[1..] {
        let prev = *out.last().unwrap();
        out.push(alpha * v + (1.0 - alpha) * prev);
    }
    out
}

fn rolling_mean(values: &[f64], window: usize, min_periods: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        let start = i.saturating_sub(window - 1);
        let slice = &values[start..=i];
        if slice.len() >= min_periods {
            out[i] = slice.iter().sum::<f64>() / slice.len() as f64;
        }
    }
    out
}

fn rolling_std(values: &[f64], window: usize, min_periods: usize) -> Vec<f64> {
    let means = rolling_mean(values, window, min_periods);
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        let start = i.saturating_sub(window - 1);
        let slice = &values[start..=i];
        if slice.len() >= min_periods {
            let m = means[i];
            if m.is_finite() {
                let var = slice.iter().map(|v| (v - m).powi(2)).sum::<f64>() / slice.len() as f64;
                out[i] = var.sqrt();
            }
        }
    }
    out
}

fn pct_change(values: &[f64], periods: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    for i in periods..n {
        let prev = values[i - periods];
        if prev != 0.0 {
            out[i] = (values[i] - prev) / prev;
        }
    }
    out
}

fn nan_to_zero(x: f64) -> f64 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

fn rsi_series(close: &[f64], period: usize) -> Vec<f64> {
    let n = close.len();
    let mut rsi = vec![f64::NAN; n];
    if n < period + 1 {
        return rsi;
    }
    let deltas: Vec<f64> = close.windows(2).map(|w| w[1] - w[0]).collect();
    if deltas.len() < period {
        return rsi;
    }
    let mut avg_gain = deltas[..period].iter().map(|&d| d.max(0.0)).sum::<f64>() / period as f64;
    let mut avg_loss = deltas[..period]
        .iter()
        .map(|&d| (-d).max(0.0))
        .sum::<f64>()
        / period as f64;
    for (j, &d) in deltas[period..].iter().enumerate() {
        avg_gain = (avg_gain * (period as f64 - 1.0) + d.max(0.0)) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + (-d).max(0.0)) / period as f64;
        let rs = if avg_loss == 0.0 {
            100.0
        } else {
            avg_gain / avg_loss
        };
        rsi[j + period + 1] = 100.0 - 100.0 / (1.0 + rs);
    }
    rsi
}

fn atr_series(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
    let n = close.len();
    let mut tr = vec![f64::NAN; n];
    for i in 1..n {
        let hl = high[i] - low[i];
        let hc = (high[i] - close[i - 1]).abs();
        let lc = (low[i] - close[i - 1]).abs();
        tr[i] = hl.max(hc).max(lc);
    }
    // Skip index 0 NaN for rolling mean of TR
    let tr_vals: Vec<f64> = tr.iter().map(|&v| if v.is_finite() { v } else { 0.0 }).collect();
    rolling_mean(&tr_vals, period, period.max(5) / 2)
}

fn adx_series(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
    let n = close.len();
    let mut plus_dm = vec![0.0; n];
    let mut minus_dm = vec![0.0; n];
    for i in 1..n {
        let up = high[i] - high[i - 1];
        let down = low[i - 1] - low[i];
        plus_dm[i] = if up > down && up > 0.0 { up } else { 0.0 };
        minus_dm[i] = if down > up && down > 0.0 { down } else { 0.0 };
    }
    let atr = atr_series(high, low, close, period);
    let plus_di_raw = ema(&plus_dm, period);
    let minus_di_raw = ema(&minus_dm, period);
    let mut dx = vec![0.0; n];
    for i in 0..n {
        let a = atr[i];
        if !a.is_finite() || a.abs() < 1e-12 {
            continue;
        }
        let pdi = 100.0 * plus_di_raw[i] / a;
        let mdi = 100.0 * minus_di_raw[i] / a;
        let denom = pdi + mdi;
        if denom.abs() > 1e-12 {
            dx[i] = 100.0 * (pdi - mdi).abs() / denom;
        }
    }
    ema(&dx, period)
}

pub struct TechnicalFeatureBuilder;

impl TechnicalFeatureBuilder {
    /// Compute the V2 bar-level feature vector from OHLCV bars.
    pub fn feature_vector(bars: &[KlineBar], idx: Option<usize>) -> Vec<f64> {
        if bars.len() < MIN_BARS_FOR_FEATURES {
            return vec![0.0; FEATURE_DIM];
        }

        let open: Vec<f64> = bars.iter().map(|b| b.open).collect();
        let close: Vec<f64> = bars.iter().map(|b| b.close).collect();
        let high: Vec<f64> = bars.iter().map(|b| b.high).collect();
        let low: Vec<f64> = bars.iter().map(|b| b.low).collect();
        let volume: Vec<f64> = bars.iter().map(|b| b.volume).collect();
        let amount: Vec<f64> = bars
            .iter()
            .map(|b| if b.amount > 0.0 { b.amount } else { b.volume * b.close })
            .collect();
        let n = close.len();
        let i = idx.unwrap_or(n - 1).min(n - 1);

        let ema20 = ema(&close, 20);
        let ema50 = ema(&close, 50);
        let ema100 = ema(&close, 100);
        let ema200 = ema(&close, 200);

        let rsi = rsi_series(&close, 14);
        let ema12 = ema(&close, 12);
        let ema26 = ema(&close, 26);
        let macd_line: Vec<f64> = ema12
            .iter()
            .zip(ema26.iter())
            .map(|(&a, &b)| a - b)
            .collect();
        let macd_sig = ema(&macd_line, 9);
        let macd_hist: Vec<f64> = macd_line
            .iter()
            .zip(macd_sig.iter())
            .map(|(&m, &s)| m - s)
            .collect();

        let atr = atr_series(&high, &low, &close, 14);
        let adx = adx_series(&high, &low, &close, 14);

        let mid = rolling_mean(&close, 20, 5);
        let std_v = rolling_std(&close, 20, 5);

        let mut cum_qv = 0.0;
        let mut cum_v = 0.0;
        let mut vwap = vec![f64::NAN; n];
        for j in 0..n {
            cum_qv += amount[j];
            cum_v += volume[j];
            if cum_v > 1e-12 {
                vwap[j] = cum_qv / cum_v;
            }
        }

        let vol_ma = rolling_mean(&volume, 20, 5);
        let ret1 = pct_change(&close, 1);
        let volatility = rolling_std(&ret1.iter().map(|&v| nan_to_zero(v)).collect::<Vec<_>>(), 20, 5);

        let c = close[i];
        if c.abs() < 1e-12 {
            return vec![0.0; FEATURE_DIM];
        }

        let rng = high[i] - low[i];
        let body = (close[i] - open[i]).abs();
        let upper = high[i] - open[i].max(close[i]);
        let lower = open[i].min(close[i]) - low[i];
        let (body_pct, upper_wick_pct, lower_wick_pct) = if rng > 1e-12 {
            (body / rng, upper / rng, lower / rng)
        } else {
            (0.0, 0.0, 0.0)
        };

        let atr_i = atr[i];
        let trend_strength = if atr_i.is_finite() && atr_i > 1e-12 {
            (ema20[i] - ema50[i]).abs() / atr_i
        } else {
            0.0
        };

        let ts = bars[i].timestamp;
        let dt = Utc
            .timestamp_opt(ts, 0)
            .single()
            .unwrap_or_else(Utc::now);
        let hour = dt.hour() as f64 + dt.minute() as f64 / 60.0;
        let dow = dt.weekday().num_days_from_monday() as f64;
        let hour_angle = 2.0 * std::f64::consts::PI * hour / 24.0;
        let dow_angle = 2.0 * std::f64::consts::PI * dow / 7.0;

        let bb_width = {
            let m = mid[i];
            let s = std_v[i];
            if m.is_finite() && m != 0.0 && s.is_finite() {
                (4.0 * s) / m
            } else {
                0.0
            }
        };

        let vol_ma_i = vol_ma[i];
        let volume_ma_ratio = if vol_ma_i.is_finite() && vol_ma_i > 1e-12 {
            volume[i] / vol_ma_i
        } else {
            0.0
        };

        let vwap_dist = if vwap[i].is_finite() {
            (c - vwap[i]) / c
        } else {
            0.0
        };

        vec![
            nan_to_zero(ema20[i] / c - 1.0),
            nan_to_zero(ema50[i] / c - 1.0),
            nan_to_zero(ema100[i] / c - 1.0),
            nan_to_zero(ema200[i] / c - 1.0),
            nan_to_zero(rsi[i] / 100.0),
            nan_to_zero(macd_hist[i] / c),
            nan_to_zero(if atr_i.is_finite() { atr_i / c } else { f64::NAN }),
            nan_to_zero(adx[i] / 100.0),
            nan_to_zero(vwap_dist),
            nan_to_zero(bb_width),
            nan_to_zero(volume_ma_ratio),
            nan_to_zero(volatility[i]),
            nan_to_zero(body_pct),
            nan_to_zero(upper_wick_pct),
            nan_to_zero(lower_wick_pct),
            nan_to_zero(pct_change(&close, 1)[i]),
            nan_to_zero(pct_change(&close, 5)[i]),
            nan_to_zero(pct_change(&close, 20)[i]),
            nan_to_zero(pct_change(&close, 10)[i]),
            nan_to_zero(trend_strength),
            hour_angle.sin(),
            hour_angle.cos(),
            dow_angle.sin(),
            dow_angle.cos(),
        ]
    }

    /// Live inference features: bar-level only (matches historical training).
    /// `ctx` / signal fields are accepted for API compatibility but ignored
    /// for the ONNX input vector.
    pub fn signal_features(
        bars: Option<&[KlineBar]>,
        _composite_score: f64,
        _zone_score: f64,
        _volume_surge_ratio: f64,
        _price_change_pct: f64,
        _side_long: bool,
        _strategy: &str,
        _ctx: &MlFeatureContext,
    ) -> Vec<f64> {
        if let Some(b) = bars {
            if b.len() >= MIN_BARS_FOR_FEATURES {
                return Self::feature_vector(b, None);
            }
        }
        vec![0.0; FEATURE_DIM]
    }

    /// No-op retained for scanner shadow backfill compatibility.
    pub fn enrich_context_from_payload(_features: &mut [f64], _sig: &Value) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_bars(n: usize) -> Vec<KlineBar> {
        (0..n)
            .map(|i| {
                let base = 100.0 + (i as f64) * 0.05;
                KlineBar {
                    symbol: "BTC_USDT".into(),
                    timestamp: 1_700_000_000 + (i as i64) * 900,
                    open: base,
                    high: base + 0.5,
                    low: base - 0.5,
                    close: base + 0.1,
                    volume: 1000.0 + i as f64,
                    amount: (1000.0 + i as f64) * base,
                }
            })
            .collect()
    }

    #[test]
    fn normalize_pads_and_truncates() {
        let v = normalize_feature_vector(Some(&[1.0, 2.0]), 4);
        assert_eq!(v, vec![1.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn feature_vector_from_bars() {
        let bars = synth_bars(250);
        let fv = TechnicalFeatureBuilder::feature_vector(&bars, None);
        assert_eq!(fv.len(), FEATURE_DIM);
        assert!(fv.iter().any(|&v| v != 0.0));
    }

    #[test]
    fn short_history_returns_zeros() {
        let bars = synth_bars(50);
        let fv = TechnicalFeatureBuilder::feature_vector(&bars, None);
        assert_eq!(fv, vec![0.0; FEATURE_DIM]);
    }

    #[test]
    fn signal_features_matches_bar_features() {
        let bars = synth_bars(250);
        let ctx = MlFeatureContext::default();
        let fv = TechnicalFeatureBuilder::signal_features(
            Some(&bars),
            75.0,
            65.0,
            3.5,
            0.02,
            true,
            "ai",
            &ctx,
        );
        assert_eq!(fv.len(), FEATURE_DIM);
        let bar_only = TechnicalFeatureBuilder::feature_vector(&bars, None);
        assert_eq!(fv, bar_only);
    }
}
