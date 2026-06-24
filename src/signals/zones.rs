//! Supply/demand zones — port of `signals/zones.py`.

use crate::config::ZonesConfig;
use crate::exchange::KlineBar;
use crate::signals::Side;

#[derive(Debug, Clone)]
pub struct Zone {
    pub kind: String,
    pub low: f64,
    pub high: f64,
    pub mid: f64,
    pub strength: f64,
}

pub fn build_zones(klines: &[KlineBar], cfg: &ZonesConfig) -> Vec<Zone> {
    if klines.len() < (cfg.pivot_left + cfg.pivot_right + 3) as usize {
        return vec![];
    }
    let start = klines.len().saturating_sub(cfg.lookback_bars as usize);
    let bars = &klines[start..];
    let lows: Vec<f64> = bars.iter().map(|b| b.low).collect();
    let highs: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let width = cfg.zone_width_pct / 100.0;

    let mut zones = Vec::new();
    for idx in pivot_indices(&lows, cfg.pivot_left as usize, cfg.pivot_right as usize, true) {
        let low = lows[idx];
        zones.push(Zone {
            kind: "demand".into(),
            low,
            high: low * (1.0 + width),
            mid: low * (1.0 + width / 2.0),
            strength: 1.0,
        });
    }
    for idx in pivot_indices(&highs, cfg.pivot_left as usize, cfg.pivot_right as usize, false) {
        let high = highs[idx];
        zones.push(Zone {
            kind: "supply".into(),
            low: high * (1.0 - width),
            high,
            mid: high * (1.0 - width / 2.0),
            strength: 1.0,
        });
    }
    merge_zones(zones, width)
}

fn pivot_indices(values: &[f64], left: usize, right: usize, find_min: bool) -> Vec<usize> {
    let mut pivots = Vec::new();
    let n = values.len();
    for i in left..n.saturating_sub(right) {
        let window = &values[i - left..=i + right];
        if find_min && values[i] <= window.iter().cloned().fold(f64::INFINITY, f64::min) {
            pivots.push(i);
        } else if !find_min && values[i] >= window.iter().cloned().fold(f64::NEG_INFINITY, f64::max) {
            pivots.push(i);
        }
    }
    pivots
}

fn merge_zones(zones: Vec<Zone>, width: f64) -> Vec<Zone> {
    if zones.is_empty() {
        return zones;
    }
    let mut sorted = zones;
    sorted.sort_by(|a, b| a.mid.partial_cmp(&b.mid).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged = vec![sorted[0].clone()];
    for zone in sorted.into_iter().skip(1) {
        let prev = merged.last_mut().unwrap();
        if zone.kind == prev.kind && zone.low <= prev.high * (1.0 + width * 0.5) {
            prev.low = prev.low.min(zone.low);
            prev.high = prev.high.max(zone.high);
            prev.mid = (prev.low + prev.high) / 2.0;
            prev.strength += 1.0;
        } else {
            merged.push(zone);
        }
    }
    merged
}

pub fn zone_confluence_score(price: f64, side: Side, zones: &[Zone], proximity_pct: f64) -> (f64, String) {
    let kind = match side {
        Side::Long => "demand",
        Side::Short => "supply",
    };
    let candidates: Vec<&Zone> = zones.iter().filter(|z| z.kind == kind).collect();
    if candidates.is_empty() {
        return (0.0, "No matching zone".into());
    }
    let nearest = candidates
        .iter()
        .min_by(|a, b| {
            (price - a.mid)
                .abs()
                .partial_cmp(&(price - b.mid).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();
    let dist_pct = (price - nearest.mid).abs() / price.max(1e-12) * 100.0;
    if dist_pct > proximity_pct {
        return (
            0.0,
            format!("Nearest {kind} zone {:.4} too far ({dist_pct:.2}% away)", nearest.mid),
        );
    }
    let score = (100.0 - dist_pct / proximity_pct.max(0.01) * 40.0).clamp(40.0, 100.0);
    (
        score,
        format!("At {kind} zone {:.4}–{:.4}", nearest.low, nearest.high),
    )
}

pub fn structure_aligned(klines: &[KlineBar], side: Side) -> bool {
    if klines.len() < 6 {
        return false;
    }
    let lows: Vec<f64> = klines.iter().map(|b| b.low).collect();
    let highs: Vec<f64> = klines.iter().map(|b| b.high).collect();
    match side {
        Side::Long => lows[lows.len() - 1] >= lows[lows.len() - 3],
        Side::Short => highs[highs.len() - 1] <= highs[highs.len() - 3],
    }
}

pub fn market_structure_supports(klines: &[KlineBar], side: Side, lookback: usize) -> bool {
    let n = klines.len().min(lookback);
    if n < 10 {
        return true;
    }
    let slice = &klines[klines.len() - n..];
    let closes: Vec<f64> = slice.iter().map(|b| b.close).collect();
    let mid = closes.len() / 2;
    let first_avg = closes[..mid].iter().sum::<f64>() / mid.max(1) as f64;
    let second_avg = closes[mid..].iter().sum::<f64>() / (closes.len() - mid).max(1) as f64;
    match side {
        Side::Long => second_avg >= first_avg * 0.998,
        Side::Short => second_avg <= first_avg * 1.002,
    }
}

/// True when the latest bar closes beyond the prior `lookback` range with volume confirmation.
pub fn breakout_confirmed(
    klines: &[KlineBar],
    side: Side,
    lookback: usize,
    min_pct: f64,
    vol_mult: f64,
    ewma_span: usize,
) -> bool {
    let n = lookback.max(3);
    if klines.len() < n + 1 {
        return false;
    }
    let prior = &klines[klines.len() - n - 1..klines.len() - 1];
    let last = &klines[klines.len() - 1];
    let volumes: Vec<f64> = klines.iter().map(|b| b.volume).collect();
    let baseline = if volumes.len() > 1 {
        crate::signals::indicators::ewma(&volumes[..volumes.len() - 1], ewma_span.max(2))
    } else {
        last.volume
    };
    if last.volume < baseline * vol_mult {
        return false;
    }
    match side {
        Side::Long => {
            let range_high = prior.iter().map(|b| b.high).fold(f64::NEG_INFINITY, f64::max);
            let threshold = range_high * (1.0 + min_pct);
            last.close > threshold
        }
        Side::Short => {
            let range_low = prior.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
            let threshold = range_low * (1.0 - min_pct);
            last.close < threshold
        }
    }
}

/// 1m structure aligned plus directional half-window bias (market shift).
pub fn market_shift_confirmed(klines: &[KlineBar], side: Side, lookback: usize) -> bool {
    structure_aligned(klines, side) && market_structure_supports(klines, side, lookback)
}
