//! Technical indicators — port of `signals/indicators.py`.

pub fn ewma(values: &[f64], span: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let alpha = 2.0 / (span as f64 + 1.0);
    let mut ema = values[0];
    for &v in values.iter().skip(1) {
        ema = alpha * v + (1.0 - alpha) * ema;
    }
    ema
}

pub fn zscore(current: f64, history: &[f64]) -> f64 {
    if history.is_empty() {
        return 0.0;
    }
    let mean = history.iter().sum::<f64>() / history.len() as f64;
    let var = history.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / history.len() as f64;
    let std = var.sqrt();
    if std < f64::EPSILON {
        return 0.0;
    }
    (current - mean) / std
}

pub fn price_change_pct(prices: &[f64], lookback: usize) -> f64 {
    if prices.len() < lookback + 1 {
        return 0.0;
    }
    let start = prices[prices.len() - lookback - 1];
    let end = *prices.last().unwrap();
    if start.abs() < f64::EPSILON {
        return 0.0;
    }
    (end - start) / start * 100.0
}

pub fn volume_surge_ratio(current: f64, baseline: f64) -> f64 {
    if baseline.abs() < f64::EPSILON {
        return 0.0;
    }
    current / baseline
}

pub fn liquidity_score(turnover: f64, min_t: f64, max_t: f64) -> f64 {
    if turnover < min_t {
        return 0.0;
    }
    ((turnover - min_t) / (max_t - min_t) * 100.0).clamp(0.0, 100.0)
}

pub fn momentum_score(move_pct: f64, max_move: f64) -> f64 {
    if max_move <= 0.0 {
        return 0.0;
    }
    (move_pct / max_move * 100.0).clamp(0.0, 100.0)
}

pub fn oi_proxy_score(amount24: f64, volume24: f64, price: f64) -> f64 {
    let turnover = if amount24 > 0.0 { amount24 } else { volume24 * price };
    (turnover.ln_1p() / 20.0 * 100.0).clamp(0.0, 100.0)
}

pub fn atr_pct(bars: &[(f64, f64, f64)]) -> f64 {
    if bars.len() < 2 {
        return 0.0;
    }
    let mut trs = Vec::with_capacity(bars.len() - 1);
    for i in 1..bars.len() {
        let (high, low, prev_close) = (bars[i].0, bars[i].1, bars[i - 1].2);
        let tr = (high - low)
            .max((high - prev_close).abs())
            .max((low - prev_close).abs());
        trs.push(tr);
    }
    let atr = trs.iter().sum::<f64>() / trs.len() as f64;
    let close = bars.last().map(|b| b.2).unwrap_or(1.0);
    if close.abs() < f64::EPSILON {
        return 0.0;
    }
    atr / close * 100.0
}

pub fn ema(values: &[f64], span: usize) -> Vec<f64> {
    if values.is_empty() {
        return vec![];
    }
    let alpha = 2.0 / (span as f64 + 1.0);
    let mut out = Vec::with_capacity(values.len());
    let mut e = values[0];
    for &v in values {
        e = alpha * v + (1.0 - alpha) * e;
        out.push(e);
    }
    out
}
