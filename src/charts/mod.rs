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

    let take_profits = take_profits_from_position(position)
        .unwrap_or_else(|| take_profits_from_signal(signal));

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
    // Try a fresh pull with a short timeout; fall back to scanner cache on slow networks.
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

/// Load klines anchored from a signal/entry timestamp through now so resolved
/// exit markers stay inside the chart window as live candles update.
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

/// When a signal resolved without a linked closed position, infer where price
/// hit TP/SL (or expired) so the overlay can pin an EXIT marker.
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
        let exit_bar = after.iter().copied().filter(|b| b.timestamp <= end_ts).last()?;
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
}
