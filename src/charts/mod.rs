//! Chart payloads for dashboard overlays.

use serde_json::{json, Value};

use crate::config::AppConfig;
use crate::exchange::KlineBar;
use crate::signals::zones::{build_zones, Zone};

pub const DATA_SOURCE: &str = "mexc_perpetual";

const CHART_KLINE_TIMEOUT_SECS: u64 = 8;

pub fn kline_interval_secs(interval: &str) -> i64 {
    match interval {
        "Min1" => 60,
        "Min5" => 300,
        "Min15" => 900,
        "Min30" => 1800,
        "Min60" | "Hour1" => 3600,
        "Hour4" => 14_400,
        "Day1" => 86_400,
        _ => 60,
    }
}

/// Clamp chart start so we never request more than `max_bars` of history.
pub fn clamp_chart_start_ts(start_ts: i64, max_bars: usize, interval: &str) -> i64 {
    if start_ts <= 0 {
        return start_ts;
    }
    let bar_sec = kline_interval_secs(interval);
    let max_window = max_bars as i64 * bar_sec;
    let earliest = chrono::Utc::now().timestamp() - max_window;
    start_ts.max(earliest)
}

pub fn bars_to_chart_payload(bars: &[KlineBar]) -> Vec<Value> {
    bars.iter()
        .map(|b| {
            json!({
                "timestamp": b.timestamp,
                "open": b.open,
                "high": b.high,
                "low": b.low,
                "close": b.close,
                "volume": b.volume,
            })
        })
        .collect()
}

pub fn zones_to_json(zones: &[Zone]) -> Vec<Value> {
    zones
        .iter()
        .map(|z| {
            json!({
                "kind": z.kind,
                "low": z.low,
                "high": z.high,
                "mid": z.mid,
            })
        })
        .collect()
}

pub fn build_chart_zones(bars: &[KlineBar], config: &AppConfig) -> Vec<Value> {
    zones_to_json(&build_zones(bars, &config.zones))
}

/// EMA / RSI / MACD series + human-readable reasons for why a setup looked tradeable.
pub fn build_ta_overlay(bars: &[KlineBar], signal: Option<&Value>, side_hint: &str) -> Value {
    if bars.is_empty() {
        return json!({
            "series": {},
            "snapshot": {},
            "reasons": [],
        });
    }

    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let highs: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let lows: Vec<f64> = bars.iter().map(|b| b.low).collect();
    let volumes: Vec<f64> = bars.iter().map(|b| b.volume).collect();

    let ema20 = ema_series(&closes, 20);
    let ema50 = ema_series(&closes, 50);
    let ema200 = ema_series(&closes, 200);
    let rsi = rsi_series(&closes, 14);
    let (macd_line, macd_signal, macd_hist) = macd_series(&closes);
    let atr = atr_series(&highs, &lows, &closes, 14);
    let adx = adx_series(&highs, &lows, &closes, 14);
    let vol_ma = sma_series(&volumes, 20);

    let series = json!({
        "ema20": points(bars, &ema20),
        "ema50": points(bars, &ema50),
        "ema200": points(bars, &ema200),
        "rsi": points(bars, &rsi),
        "macd_hist": points(bars, &macd_hist),
        "macd": points(bars, &macd_line),
        "macd_signal": points(bars, &macd_signal),
    });

    let i = bars.len() - 1;
    let close = closes[i];
    let e20 = ema20[i];
    let e50 = ema50[i];
    let e200 = ema200[i];
    let rsi_v = rsi[i];
    let macd_h = macd_hist[i];
    let adx_v = adx[i];
    let atr_pct = if close > 1e-12 { atr[i] / close * 100.0 } else { 0.0 };
    let vol_ratio = if vol_ma[i] > 1e-12 {
        volumes[i] / vol_ma[i]
    } else {
        1.0
    };

    let side = signal
        .and_then(|s| s.get("side").and_then(|v| v.as_str()))
        .map(str::to_lowercase)
        .unwrap_or_else(|| side_hint.to_lowercase());
    let side_long = side == "long";

    let mut reasons: Vec<String> = Vec::new();

    if side_long {
        if close > e20 && e20 > e50 {
            reasons.push("Bullish EMA stack: price > EMA20 > EMA50".into());
        } else if close > e20 {
            reasons.push("Price above EMA20 (short-term bullish)".into());
        } else {
            reasons.push("Long setup without clean EMA stack (momentum / other filters)".into());
        }
        if e50 > e200 {
            reasons.push("EMA50 above EMA200 (broader uptrend bias)".into());
        }
    } else {
        if close < e20 && e20 < e50 {
            reasons.push("Bearish EMA stack: price < EMA20 < EMA50".into());
        } else if close < e20 {
            reasons.push("Price below EMA20 (short-term bearish)".into());
        } else {
            reasons.push("Short setup without clean EMA stack (momentum / other filters)".into());
        }
        if e50 < e200 {
            reasons.push("EMA50 below EMA200 (broader downtrend bias)".into());
        }
    }

    if rsi_v.is_finite() {
        if side_long && (45.0..=70.0).contains(&rsi_v) {
            reasons.push(format!("RSI {rsi_v:.0} — momentum up, not extreme overbought"));
        } else if !side_long && (30.0..=55.0).contains(&rsi_v) {
            reasons.push(format!("RSI {rsi_v:.0} — momentum down, not extreme oversold"));
        } else if rsi_v > 70.0 {
            reasons.push(format!("RSI {rsi_v:.0} overbought — higher reversal risk"));
        } else if rsi_v < 30.0 {
            reasons.push(format!("RSI {rsi_v:.0} oversold — higher bounce risk"));
        } else {
            reasons.push(format!("RSI {rsi_v:.0}"));
        }
    }

    if macd_h.is_finite() {
        if (side_long && macd_h > 0.0) || (!side_long && macd_h < 0.0) {
            reasons.push(format!(
                "MACD histogram {} — aligned with {}",
                if macd_h > 0.0 { "positive" } else { "negative" },
                side
            ));
        } else {
            reasons.push("MACD histogram not aligned with trade side".into());
        }
    }

    if adx_v.is_finite() {
        if adx_v >= 25.0 {
            reasons.push(format!("ADX {adx_v:.0} — trending market (favors continuation)"));
        } else if adx_v >= 18.0 {
            reasons.push(format!("ADX {adx_v:.0} — moderate trend strength"));
        } else {
            reasons.push(format!("ADX {adx_v:.0} — choppy / weak trend"));
        }
    }

    if vol_ratio >= 1.4 {
        reasons.push(format!("Volume {vol_ratio:.1}× vs 20-bar average (participation)"));
    }

    if atr_pct.is_finite() && atr_pct > 0.0 {
        reasons.push(format!("ATR ≈ {atr_pct:.2}% of price (volatility for SL sizing)"));
    }

    if let Some(sig) = signal {
        if let Some(p) = sig
            .get("setup_probability_pct")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                sig.get("setup_probability")
                    .and_then(|v| v.as_f64())
                    .map(|x| if x <= 1.0 { x * 100.0 } else { x })
            })
        {
            reasons.push(format!("ML setup confidence ≈ {p:.0}%"));
        }
        if let Some(score) = sig.get("composite_score").and_then(|v| v.as_f64()) {
            reasons.push(format!("Composite setup score {score:.1}"));
        }
        if let Some(arr) = sig.get("confluences").and_then(|v| v.as_array()) {
            for c in arr.iter().take(4) {
                if let Some(s) = c.as_str() {
                    if !s.is_empty() {
                        reasons.push(s.to_string());
                    }
                }
            }
        }
        if let Some(msg) = sig.get("zone_message").and_then(|v| v.as_str()) {
            if !msg.is_empty() {
                reasons.push(msg.to_string());
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    reasons.retain(|r| seen.insert(r.clone()));

    json!({
        "series": series,
        "snapshot": {
            "close": close,
            "ema20": e20,
            "ema50": e50,
            "ema200": e200,
            "rsi": rsi_v,
            "macd_hist": macd_h,
            "adx": adx_v,
            "atr_pct": atr_pct,
            "volume_ma_ratio": vol_ratio,
            "side": side,
        },
        "reasons": reasons,
    })
}

fn points(bars: &[KlineBar], values: &[f64]) -> Vec<Value> {
    bars.iter()
        .zip(values.iter())
        .filter_map(|(b, &v)| {
            if !v.is_finite() {
                return None;
            }
            Some(json!({ "timestamp": b.timestamp, "value": v }))
        })
        .collect()
}

fn ema_series(values: &[f64], span: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    if n == 0 || span == 0 {
        return out;
    }
    let alpha = 2.0 / (span as f64 + 1.0);
    out[0] = values[0];
    for i in 1..n {
        out[i] = alpha * values[i] + (1.0 - alpha) * out[i - 1];
    }
    let warm = span.saturating_sub(1).min(n);
    for v in out.iter_mut().take(warm) {
        *v = f64::NAN;
    }
    out
}

fn sma_series(values: &[f64], period: usize) -> Vec<f64> {
    let n = values.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 || n < period {
        return out;
    }
    let mut sum = 0.0;
    for i in 0..n {
        sum += values[i];
        if i >= period {
            sum -= values[i - period];
        }
        if i + 1 >= period {
            out[i] = sum / period as f64;
        }
    }
    out
}

fn rsi_series(close: &[f64], period: usize) -> Vec<f64> {
    let n = close.len();
    let mut out = vec![f64::NAN; n];
    if n < period + 1 {
        return out;
    }
    let mut gains = 0.0;
    let mut losses = 0.0;
    for i in 1..=period {
        let d = close[i] - close[i - 1];
        if d >= 0.0 {
            gains += d;
        } else {
            losses -= d;
        }
    }
    let mut avg_gain = gains / period as f64;
    let mut avg_loss = losses / period as f64;
    out[period] = if avg_loss < 1e-12 {
        100.0
    } else {
        100.0 - 100.0 / (1.0 + avg_gain / avg_loss)
    };
    for i in (period + 1)..n {
        let d = close[i] - close[i - 1];
        let (g, l) = if d >= 0.0 { (d, 0.0) } else { (0.0, -d) };
        avg_gain = (avg_gain * (period as f64 - 1.0) + g) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + l) / period as f64;
        out[i] = if avg_loss < 1e-12 {
            100.0
        } else {
            100.0 - 100.0 / (1.0 + avg_gain / avg_loss)
        };
    }
    out
}

fn macd_series(close: &[f64]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let ema12 = ema_series(close, 12);
    let ema26 = ema_series(close, 26);
    let line: Vec<f64> = ema12
        .iter()
        .zip(ema26.iter())
        .map(|(&a, &b)| {
            if a.is_finite() && b.is_finite() {
                a - b
            } else {
                f64::NAN
            }
        })
        .collect();
    let filled: Vec<f64> = line.iter().map(|&v| if v.is_finite() { v } else { 0.0 }).collect();
    let signal = ema_series(&filled, 9);
    let hist: Vec<f64> = line
        .iter()
        .zip(signal.iter())
        .map(|(&m, &s)| {
            if m.is_finite() && s.is_finite() {
                m - s
            } else {
                f64::NAN
            }
        })
        .collect();
    (line, signal, hist)
}

fn atr_series(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
    let n = close.len();
    let mut tr = vec![0.0; n];
    if n == 0 {
        return tr;
    }
    tr[0] = high[0] - low[0];
    for i in 1..n {
        tr[i] = (high[i] - low[i])
            .max((high[i] - close[i - 1]).abs())
            .max((low[i] - close[i - 1]).abs());
    }
    let mut out = vec![f64::NAN; n];
    if n < period {
        return out;
    }
    let sum: f64 = tr.iter().take(period).sum();
    out[period - 1] = sum / period as f64;
    for i in period..n {
        out[i] = (out[i - 1] * (period as f64 - 1.0) + tr[i]) / period as f64;
    }
    out
}

fn adx_series(high: &[f64], low: &[f64], close: &[f64], period: usize) -> Vec<f64> {
    let n = close.len();
    if n < period + 2 {
        return vec![f64::NAN; n];
    }
    let atr = atr_series(high, low, close, period);
    let mut plus_dm = vec![0.0; n];
    let mut minus_dm = vec![0.0; n];
    for i in 1..n {
        let up = high[i] - high[i - 1];
        let down = low[i - 1] - low[i];
        plus_dm[i] = if up > down && up > 0.0 { up } else { 0.0 };
        minus_dm[i] = if down > up && down > 0.0 { down } else { 0.0 };
    }
    let plus_sm = ema_series(&plus_dm, period);
    let minus_sm = ema_series(&minus_dm, period);
    let mut dx = vec![0.0; n];
    for i in 0..n {
        let a = atr[i];
        if !a.is_finite() || a.abs() < 1e-12 {
            continue;
        }
        let pdi = 100.0 * plus_sm[i] / a;
        let mdi = 100.0 * minus_sm[i] / a;
        let denom = pdi + mdi;
        if denom.abs() > 1e-12 {
            dx[i] = 100.0 * (pdi - mdi).abs() / denom;
        }
    }
    ema_series(&dx, period)
}

pub fn mexc_to_tradingview_symbol(symbol: &str) -> String {
    let pair = symbol.replace('_', "").to_uppercase();
    format!("MEXC:{pair}.P")
}

/// Merge signal setup levels with an opened position (if any) for chart overlays.
pub fn build_trade_overlay(signal: Option<&Value>, position: Option<&Value>) -> Value {
    let side = position
        .and_then(|p| p.get("side").and_then(|v| v.as_str()))
        .map(str::to_lowercase)
        .or_else(|| signal.as_ref().map(|s| signal_side(s)))
        .unwrap_or_else(|| "long".into());

    let entry = position
        .and_then(|p| p.get("entry_price").and_then(|v| v.as_f64()))
        .or_else(|| signal.as_ref().and_then(|s| s.get("last_price").and_then(|v| v.as_f64())))
        .unwrap_or(0.0);

    let stop_loss = position
        .and_then(|p| p.get("stop_loss").and_then(|v| v.as_f64()))
        .or_else(|| {
            signal
                .as_ref()
                .and_then(|s| s.get("projected_stop_loss").and_then(|v| v.as_f64()))
        })
        .unwrap_or(0.0);

    let take_profits =
        take_profits_from_position(position).unwrap_or_else(|| take_profits_from_signal(signal));

    let leverage = position
        .and_then(|p| p.get("leverage").cloned())
        .or_else(|| signal.as_ref().and_then(|s| s.get("suggested_leverage").cloned()));

    let status = position
        .and_then(|p| p.get("status").and_then(|v| v.as_str()))
        .unwrap_or("");
    let is_closed = status == "closed";
    let exit_price = position.and_then(|p| p.get("exit_price").and_then(|v| v.as_f64()));
    let exit_reason = position
        .and_then(|p| p.get("exit_reason").and_then(|v| v.as_str()))
        .map(str::to_string);
    let closed_at = position
        .and_then(|p| p.get("closed_at").and_then(|v| v.as_str()))
        .map(str::to_string);
    let realized_pnl = position.and_then(|p| p.get("realized_pnl").and_then(|v| v.as_f64()));
    let realized_pnl_pct = position.and_then(|p| p.get("realized_pnl_pct").and_then(|v| v.as_f64()));
    let unrealized_pnl = position.and_then(|p| p.get("unrealized_pnl").and_then(|v| v.as_f64()));
    let unrealized_pnl_pct = position.and_then(|p| p.get("unrealized_pnl_pct").and_then(|v| v.as_f64()));
    let unrealized_roi_pct = position.and_then(|p| p.get("unrealized_roi_pct").and_then(|v| v.as_f64()));
    let mark_price = position.and_then(|p| p.get("mark_price").and_then(|v| v.as_f64()));

    let leverage_num = leverage.as_ref().and_then(|v| v.as_f64());
    let roi_pct = match (realized_pnl_pct, leverage_num) {
        (Some(pct), Some(lev)) if lev > 0.0 => Some((pct * lev * 100.0).round() / 100.0),
        _ => None,
    };
    let outcome = if is_closed {
        match realized_pnl {
            Some(p) if p > 0.0 => "win",
            Some(p) if p < 0.0 => "loss",
            Some(_) => "breakeven",
            None => "closed",
        }
    } else {
        signal
            .as_ref()
            .and_then(|s| s.get("outcome").and_then(|v| v.as_str()))
            .unwrap_or("pending")
    };

    let signal_time = position
        .and_then(|p| p.get("opened_at").and_then(|v| v.as_str()))
        .or_else(|| {
            signal
                .as_ref()
                .and_then(|s| s.get("generated_at").or_else(|| s.get("created_at")))
                .and_then(|v| v.as_str())
        });

    json!({
        "side": side,
        "entry_price": entry,
        "stop_loss": stop_loss,
        "take_profits": take_profits,
        "leverage": leverage,
        "strategy": signal.as_ref().and_then(|s| s.get("strategy")).or_else(|| position.and_then(|p| p.get("strategy"))),
        "composite_score": signal.as_ref().and_then(|s| s.get("composite_score")),
        "setup_probability_pct": signal.as_ref().and_then(|s| s.get("setup_probability_pct")),
        "suggested_risk_pct": signal.as_ref().and_then(|s| s.get("suggested_risk_pct")),
        "zone_score": signal.as_ref().and_then(|s| s.get("zone_score")),
        "zone_message": signal.as_ref().and_then(|s| s.get("zone_message")),
        "confluences": signal.as_ref().and_then(|s| s.get("confluences")),
        "confluence_count": signal.as_ref().and_then(|s| s.get("confluence_count")),
        "signal_time": signal_time,
        "outcome": outcome,
        "is_closed": is_closed,
        "exit_price": exit_price,
        "exit_reason": exit_reason,
        "closed_at": closed_at,
        "realized_pnl": realized_pnl,
        "realized_pnl_pct": realized_pnl_pct,
        "roi_pct": roi_pct,
        "mark_price": mark_price,
        "unrealized_pnl": unrealized_pnl,
        "unrealized_pnl_pct": unrealized_pnl_pct,
        "unrealized_roi_pct": unrealized_roi_pct,
        "has_position": position.is_some(),
        "position": position,
    })
}

fn take_profits_from_position(position: Option<&Value>) -> Option<Vec<Value>> {
    let levels = position?
        .get("take_profit_levels")?
        .as_array()
        .filter(|a| !a.is_empty())?;
    Some(
        levels
            .iter()
            .enumerate()
            .filter_map(|(i, tp)| {
                let price = tp.get("price").and_then(|v| v.as_f64())?;
                if price <= 0.0 {
                    return None;
                }
                let frac = tp
                    .get("close_fraction")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                Some(json!({
                    "level": tp.get("level").and_then(|v| v.as_i64()).unwrap_or((i + 1) as i64),
                    "price": price,
                    "close_fraction": frac,
                    "close_pct": (frac * 1000.0).round() / 10.0,
                    "label": format!("TP{}", i + 1),
                }))
            })
            .collect(),
    )
}

fn take_profits_from_signal(signal: Option<&Value>) -> Vec<Value> {
    let Some(signal) = signal else {
        return vec![];
    };
    let fracs = signal
        .get("tp_close_fractions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let tp_prices = signal
        .get("projected_take_profits")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    tp_prices
        .iter()
        .enumerate()
        .filter_map(|(i, tp)| {
            let price = tp.as_f64()?;
            if price <= 0.0 {
                return None;
            }
            let frac = fracs.get(i).and_then(|f| f.as_f64()).unwrap_or(0.0);
            Some(json!({
                "level": i + 1,
                "price": price,
                "close_fraction": frac,
                "close_pct": (frac * 1000.0).round() / 10.0,
                "label": format!("TP{}", i + 1),
            }))
        })
        .collect()
}

fn signal_side(signal: &Value) -> String {
    if let Some(s) = signal.get("side").and_then(|v| v.as_str()) {
        return s.to_lowercase();
    }
    let pct = signal
        .get("price_change_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if pct < 0.0 {
        "short".into()
    } else {
        "long".into()
    }
}

pub async fn load_chart_bars(
    exchange: &crate::exchange::MexcClient,
    cached: &[KlineBar],
    symbol: &str,
    interval: &str,
    bars: usize,
) -> Vec<KlineBar> {
    let fresh = tokio::time::timeout(
        std::time::Duration::from_secs(CHART_KLINE_TIMEOUT_SECS),
        exchange.get_klines(symbol, interval),
    )
    .await
    .ok()
    .and_then(Result::ok);

    let mut klines = match fresh {
        Some(fetched) if fetched.len() >= 30 || fetched.len() >= cached.len() => fetched,
        _ => cached.to_vec(),
    };

    if klines.len() < 30 && cached.len() > klines.len() {
        klines = cached.to_vec();
    }

    if klines.len() > bars {
        klines = klines[klines.len() - bars..].to_vec();
    }

    klines
}

pub async fn load_chart_bars_from_time(
    exchange: &crate::exchange::MexcClient,
    cached: &[KlineBar],
    symbol: &str,
    interval: &str,
    start_ts: i64,
    max_bars: usize,
) -> Vec<KlineBar> {
    let start_ts = clamp_chart_start_ts(start_ts, max_bars, interval);
    let end_ts = chrono::Utc::now().timestamp();
    let ranged = tokio::time::timeout(
        std::time::Duration::from_secs(CHART_KLINE_TIMEOUT_SECS),
        exchange.get_klines_range(symbol, interval, start_ts, end_ts),
    )
    .await
    .ok()
    .and_then(Result::ok);

    let mut klines = match ranged {
        Some(fetched) if !fetched.is_empty() => fetched,
        _ => cached
            .iter()
            .filter(|b| b.timestamp >= start_ts)
            .cloned()
            .collect(),
    };

    if klines.is_empty() {
        klines = load_chart_bars(exchange, cached, symbol, interval, max_bars).await;
    }

    if klines.len() > max_bars {
        klines = klines[klines.len() - max_bars..].to_vec();
    }

    klines
}

pub fn resolve_signal_exit_from_bars(
    bars: &[KlineBar],
    start_ts: i64,
    side_long: bool,
    sl: f64,
    tp: f64,
    outcome: &str,
    max_hold_sec: i64,
) -> Option<Value> {
    let after: Vec<&KlineBar> = bars.iter().filter(|b| b.timestamp >= start_ts).collect();
    if after.is_empty() {
        return None;
    }

    if outcome == "expired" {
        let end_ts = start_ts + max_hold_sec;
        let exit_bar = after
            .iter()
            .copied()
            .filter(|b| b.timestamp <= end_ts)
            .last()?;
        return Some(json!({
            "exit_price": exit_bar.close,
            "exit_timestamp": exit_bar.timestamp,
            "exit_reason": "expired",
            "is_resolved": true,
        }));
    }

    for bar in after {
        if side_long {
            if bar.low <= sl && outcome == "loss" {
                return Some(json!({
                    "exit_price": sl,
                    "exit_timestamp": bar.timestamp,
                    "exit_reason": "stop_loss",
                    "is_resolved": true,
                }));
            }
            if bar.high >= tp && outcome == "win" {
                return Some(json!({
                    "exit_price": tp,
                    "exit_timestamp": bar.timestamp,
                    "exit_reason": "take_profit",
                    "is_resolved": true,
                }));
            }
        } else {
            if bar.high >= sl && outcome == "loss" {
                return Some(json!({
                    "exit_price": sl,
                    "exit_timestamp": bar.timestamp,
                    "exit_reason": "stop_loss",
                    "is_resolved": true,
                }));
            }
            if bar.low <= tp && outcome == "win" {
                return Some(json!({
                    "exit_price": tp,
                    "exit_timestamp": bar.timestamp,
                    "exit_reason": "take_profit",
                    "is_resolved": true,
                }));
            }
        }
    }

    None
}

pub fn parse_signal_start_ts(signal: &Value) -> Option<i64> {
    let ts_str = signal
        .get("generated_at")
        .or_else(|| signal.get("created_at"))
        .and_then(|v| v.as_str())?;
    chrono::DateTime::parse_from_rfc3339(ts_str)
        .ok()
        .map(|dt| dt.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_chart_start_limits_window() {
        let now = chrono::Utc::now().timestamp();
        let old = now - 7 * 86_400;
        let clamped = clamp_chart_start_ts(old, 120, "Min1");
        assert!(clamped > old);
        assert!(clamped >= now - 120 * 60 - 5);
    }

    #[test]
    fn ta_overlay_has_reasons_on_trending_bars() {
        let mut bars = Vec::new();
        let mut px = 100.0;
        for i in 0..80 {
            px *= 1.002;
            bars.push(KlineBar {
                symbol: "TEST_USDT".into(),
                timestamp: 1_700_000_000 + i * 60,
                open: px * 0.999,
                high: px * 1.002,
                low: px * 0.998,
                close: px,
                volume: 1000.0 + i as f64,
                amount: px * 1000.0,
            });
        }
        let ta = build_ta_overlay(&bars, None, "long");
        let reasons = ta.get("reasons").and_then(|v| v.as_array()).unwrap();
        assert!(!reasons.is_empty());
        let series = ta.get("series").unwrap();
        assert!(series.get("ema20").and_then(|v| v.as_array()).unwrap().len() > 10);
        assert!(series.get("rsi").and_then(|v| v.as_array()).unwrap().len() > 10);
    }
}
