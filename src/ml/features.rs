//! Technical feature engineering — mirrors Python `ml/features.py` (pandas-only path).
//!
//! Feature layout (FEATURE_DIM = 15):
//!   0  rsi_14           — RSI(14), range ~0..100
//!   1  ema_ratio        — ema9/ema21 - 1 (scale-independent trend direction)
//!   2  ema_slope_pct    — (ema9_now - ema9_5bars_ago) / close (momentum %)
//!   3  macd_hist        — MACD histogram (12/26/9)
//!   4  atr_pct          — ATR(14) / close
//!   5  bb_width         — (upper-lower) / mid Bollinger
//!   6  volume_z         — volume Z-score vs 20-bar rolling mean
//!   7  return_1         — 1-bar % return
//!   8  return_5         — 5-bar % return
//!   9  hl_range_pct     — (high-low) / close
//!  10  composite_score  — signal composite score 0..1 (normalised from 0..100)
//!  11  zone_score       — signal zone score 0..1
//!  12  volume_surge     — volume_surge_ratio / 10.0, capped at 2.0
//!  13  side_long        — 1.0 = long, 0.0 = short
//!  14  price_chg_abs    — |price_change_pct|, capped at 0.10

use crate::exchange::KlineBar;

pub const FEATURE_COLUMNS: [&str; 15] = [
    "rsi_14",
    "ema_ratio",
    "ema_slope_pct",
    "macd_hist",
    "atr_pct",
    "bb_width",
    "volume_z",
    "return_1",
    "return_5",
    "hl_range_pct",
    "composite_score",
    "zone_score",
    "volume_surge",
    "side_long",
    "price_chg_abs",
];

pub const FEATURE_DIM: usize = FEATURE_COLUMNS.len(); // 15

/// Older ONNX exports (pre v15 features) expect 10 technical columns with
/// absolute `ema_9` / `ema_21` at indices 1–2 instead of `ema_ratio` / `ema_slope_pct`.
pub const LEGACY_ONNX_FEATURE_DIM: usize = 10;

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
    if x.is_finite() { x } else { 0.0 }
}

pub struct TechnicalFeatureBuilder;

impl TechnicalFeatureBuilder {
    /// Compute the full 15-feature vector from OHLCV bars.
    /// Features 10-14 (signal context) default to 0.0 when bars-only.
    pub fn feature_vector(bars: &[KlineBar], idx: Option<usize>) -> Vec<f64> {
        if bars.len() < 10 {
            return vec![0.0; FEATURE_DIM];
        }

        let close: Vec<f64> = bars.iter().map(|b| b.close).collect();
        let high: Vec<f64> = bars.iter().map(|b| b.high).collect();
        let low: Vec<f64> = bars.iter().map(|b| b.low).collect();
        let volume: Vec<f64> = bars.iter().map(|b| b.volume).collect();
        let n = close.len();

        // RSI-14
        let rsi_14: Vec<f64> = {
            let deltas: Vec<f64> = close.windows(2).map(|w| w[1] - w[0]).collect();
            let mut rsi = vec![f64::NAN; n];
            if deltas.len() >= 14 {
                let mut avg_gain = deltas[..14].iter().map(|&d| d.max(0.0)).sum::<f64>() / 14.0;
                let mut avg_loss = deltas[..14].iter().map(|&d| (-d).max(0.0)).sum::<f64>() / 14.0;
                for (j, &d) in deltas[14..].iter().enumerate() {
                    avg_gain = (avg_gain * 13.0 + d.max(0.0)) / 14.0;
                    avg_loss = (avg_loss * 13.0 + (-d).max(0.0)) / 14.0;
                    let rs = if avg_loss == 0.0 { 100.0 } else { avg_gain / avg_loss };
                    rsi[j + 15] = 100.0 - 100.0 / (1.0 + rs);
                }
            }
            rsi
        };

        // EMA 9 and 21
        let ema_9 = ema(&close, 9);
        let ema_21 = ema(&close, 21);

        // ema_ratio = ema9/ema21 - 1 (scale-independent trend direction)
        let ema_ratio: Vec<f64> = ema_9
            .iter()
            .zip(ema_21.iter())
            .map(|(&e9, &e21)| if e21 != 0.0 { e9 / e21 - 1.0 } else { 0.0 })
            .collect();

        // ema_slope_pct = (ema9_now - ema9_5bars_ago) / close_now  (momentum %)
        let ema_slope_pct: Vec<f64> = (0..n)
            .map(|i| {
                if i < 5 {
                    return f64::NAN;
                }
                let c = close[i];
                let e_now = ema_9[i];
                let e_prev = ema_9[i - 5];
                if c != 0.0 { (e_now - e_prev) / c } else { 0.0 }
            })
            .collect();

        // MACD histogram (12/26/9)
        let ema12 = ema(&close, 12);
        let ema26 = ema(&close, 26);
        let macd_line: Vec<f64> = ema12.iter().zip(ema26.iter()).map(|(&a, &b)| a - b).collect();
        let macd_sig = ema(&macd_line, 9);
        let macd_hist: Vec<f64> = macd_line.iter().zip(macd_sig.iter()).map(|(&m, &s)| m - s).collect();

        // ATR-14
        let tr: Vec<f64> = (1..n)
            .map(|i| {
                let hl = high[i] - low[i];
                let hc = (high[i] - close[i - 1]).abs();
                let lc = (low[i] - close[i - 1]).abs();
                hl.max(hc).max(lc)
            })
            .collect();
        let atr_raw = rolling_mean(&tr, 14, 5);
        let atr_pct: Vec<f64> = (0..n)
            .map(|i| {
                if i == 0 {
                    return f64::NAN;
                }
                let atr = atr_raw[i - 1];
                let c = close[i];
                if c != 0.0 && atr.is_finite() { atr / c } else { f64::NAN }
            })
            .collect();

        // Bollinger Bands width
        let mid = rolling_mean(&close, 20, 5);
        let std_v = rolling_std(&close, 20, 5);
        let bb_width: Vec<f64> = (0..n)
            .map(|i| {
                let m = mid[i];
                let s = std_v[i];
                if m.is_finite() && m != 0.0 && s.is_finite() {
                    (4.0 * s) / m
                } else {
                    f64::NAN
                }
            })
            .collect();

        // Volume Z-score
        let vol_mean = rolling_mean(&volume, 20, 5);
        let vol_std = rolling_std(&volume, 20, 5);
        let volume_z: Vec<f64> = volume
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let m = vol_mean[i];
                let s = vol_std[i];
                if m.is_finite() && s.is_finite() && s != 0.0 {
                    (v - m) / s
                } else {
                    f64::NAN
                }
            })
            .collect();

        let return_1 = pct_change(&close, 1);
        let return_5 = pct_change(&close, 5);
        let hl_range_pct: Vec<f64> = (0..n)
            .map(|i| {
                let c = close[i];
                if c != 0.0 { (high[i] - low[i]) / c } else { f64::NAN }
            })
            .collect();

        let i = idx.unwrap_or(n - 1);
        vec![
            nan_to_zero(rsi_14[i]),
            nan_to_zero(ema_ratio[i]),
            nan_to_zero(ema_slope_pct[i]),
            nan_to_zero(macd_hist[i]),
            nan_to_zero(atr_pct[i]),
            nan_to_zero(bb_width[i]),
            nan_to_zero(volume_z[i]),
            nan_to_zero(return_1[i]),
            nan_to_zero(return_5[i]),
            nan_to_zero(hl_range_pct[i]),
            // Features 10-14: signal context (0 when bars-only)
            0.0, // composite_score
            0.0, // zone_score
            0.0, // volume_surge
            0.0, // side_long
            0.0, // price_chg_abs
        ]
    }

    /// 10-feature vector matching legacy ONNX exports (absolute EMA prices at idx 1–2).
    pub fn legacy_onnx_feature_vector(bars: &[KlineBar], idx: Option<usize>) -> Vec<f64> {
        if bars.len() < 10 {
            return vec![0.0; LEGACY_ONNX_FEATURE_DIM];
        }
        let mut vec = Self::feature_vector(bars, idx);
        let close: Vec<f64> = bars.iter().map(|b| b.close).collect();
        let ema_9 = ema(&close, 9);
        let ema_21 = ema(&close, 21);
        let i = idx.unwrap_or(close.len() - 1);
        if vec.len() >= 3 {
            vec[1] = nan_to_zero(ema_9[i]);
            vec[2] = nan_to_zero(ema_21[i]);
        }
        normalize_feature_vector(Some(&vec), LEGACY_ONNX_FEATURE_DIM)
    }

    /// Build a 15-feature vector incorporating both bar technicals and signal context.
    /// Features 0-9: technical indicators from klines (or zeros if klines unavailable).
    /// Features 10-14: signal-level context (composite/zone scores, volume surge, side, move %).
    pub fn signal_features(
        bars: Option<&[KlineBar]>,
        composite_score: f64,
        zone_score: f64,
        volume_surge_ratio: f64,
        price_change_pct: f64,
        side_long: bool,
    ) -> Vec<f64> {
        let mut tech = if let Some(b) = bars {
            if b.len() >= 10 {
                let fv = Self::feature_vector(b, None);
                if !fv.is_empty() && fv.iter().any(|&v| v != 0.0) {
                    fv
                } else {
                    vec![0.0; FEATURE_DIM]
                }
            } else {
                vec![0.0; FEATURE_DIM]
            }
        } else {
            vec![0.0; FEATURE_DIM]
        };

        if tech.len() < FEATURE_DIM {
            tech.resize(FEATURE_DIM, 0.0);
        }

        // Overwrite context features (10-14) with signal-level values.
        tech[10] = (composite_score / 100.0).clamp(0.0, 1.0);
        tech[11] = (zone_score / 100.0).clamp(0.0, 1.0);
        tech[12] = (volume_surge_ratio / 10.0).clamp(0.0, 2.0);
        tech[13] = if side_long { 1.0 } else { 0.0 };
        tech[14] = price_change_pct.abs().clamp(0.0, 0.10);

        tech
    }
}

/// Convenience wrapper for legacy ONNX inference (10-feature layout).
pub fn legacy_onnx_feature_vector(bars: &[KlineBar], idx: Option<usize>) -> Vec<f64> {
    TechnicalFeatureBuilder::legacy_onnx_feature_vector(bars, idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_pads_and_truncates() {
        let v = normalize_feature_vector(Some(&[1.0, 2.0]), 4);
        assert_eq!(v, vec![1.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn feature_vector_from_bars() {
        let bars: Vec<KlineBar> = (0..30)
            .map(|i| KlineBar {
                symbol: "BTC_USDT".into(),
                timestamp: i as i64,
                open: 100.0 + i as f64,
                high: 101.0 + i as f64,
                low: 99.0 + i as f64,
                close: 100.5 + i as f64,
                volume: 1000.0 + i as f64 * 10.0,
                amount: 0.0,
            })
            .collect();
        let fv = TechnicalFeatureBuilder::feature_vector(&bars, None);
        assert_eq!(fv.len(), FEATURE_DIM);
    }

    #[test]
    fn signal_features_appends_context() {
        let bars: Vec<KlineBar> = (0..30)
            .map(|i| KlineBar {
                symbol: "X_USDT".into(),
                timestamp: i as i64,
                open: 10.0,
                high: 10.5,
                low: 9.5,
                close: 10.0,
                volume: 1000.0,
                amount: 0.0,
            })
            .collect();
        let fv = TechnicalFeatureBuilder::signal_features(Some(&bars), 75.0, 65.0, 3.5, 0.02, true);
        assert_eq!(fv.len(), FEATURE_DIM);
        // composite_score = 75/100 = 0.75
        assert!((fv[10] - 0.75).abs() < 1e-9);
        // zone_score = 65/100 = 0.65
        assert!((fv[11] - 0.65).abs() < 1e-9);
        // volume_surge = 3.5/10 = 0.35
        assert!((fv[12] - 0.35).abs() < 1e-9);
        // side_long = 1.0
        assert_eq!(fv[13], 1.0);
        // price_chg_abs = 0.02
        assert!((fv[14] - 0.02).abs() < 1e-9);
    }
}
