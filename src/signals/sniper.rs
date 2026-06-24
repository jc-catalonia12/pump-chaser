//! 1m sniper-entry logic.
//!
//! After the 15m ConfluenceEngine confirms a setup, the scanner parks a
//! `PendingSetup` in `SymbolState`.  On every subsequent 1m bar the sniper
//! checks for a valid entry trigger (pin-bar / pullback-to-zone) and, when
//! found, enriches the stored signal with a precise limit-entry price before
//! emitting it.
//!
//! Flow
//! ────
//! 1.  15m setup fires → scanner calls `record_setup(state, signal)`
//! 2.  Each 1m tick   → scanner calls `check_trigger(state, klines, cfg)`
//! 3a. Trigger found  → returns `TriggerResult::Fire { signal, limit_price }`
//! 3b. Still waiting  → returns `TriggerResult::Waiting`
//! 3c. TTL expired    → returns `TriggerResult::Expired`

use chrono::{DateTime, Utc};

use crate::{
    config::SniperConfig,
    exchange::KlineBar,
    signals::{state::Side, types::PumpSignal},
};

// ── Pending setup ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PendingSetup {
    pub signal: PumpSignal,
    pub created_at: DateTime<Utc>,
    pub side: Side,
}

// ── Result type ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum TriggerResult {
    /// A clean 1m trigger was found — fire the limit order at `limit_price`.
    Fire { signal: PumpSignal, limit_price: f64 },
    /// Not yet triggered; keep monitoring.
    Waiting,
    /// The setup has expired without a clean trigger.
    Expired,
}

// ── Public helpers ────────────────────────────────────────────────────────────

/// Store a confirmed HTF setup as pending.
pub fn record_setup(signal: &PumpSignal) -> PendingSetup {
    let side = if signal.price_change_pct >= 0.0 {
        Side::Long
    } else {
        Side::Short
    };
    PendingSetup {
        signal: signal.clone(),
        created_at: Utc::now(),
        side,
    }
}

/// Evaluate whether recent 1m `klines` provide a clean sniper trigger for the
/// stored `pending` setup.
pub fn check_trigger(
    pending: &PendingSetup,
    klines: &[KlineBar],
    cfg: &SniperConfig,
) -> TriggerResult {
    let age_sec = (Utc::now() - pending.created_at).num_seconds().max(0) as u64;
    if age_sec > cfg.htf_setup_expiry_sec {
        return TriggerResult::Expired;
    }

    let lookback = cfg.sniper_lookback_bars as usize;
    if klines.len() < lookback.max(2) {
        return TriggerResult::Waiting;
    }

    let recent = &klines[klines.len().saturating_sub(lookback)..];
    let entry_price = pending.signal.last_price;
    let sl = pending.signal.projected_stop_loss;
    let sl_dist = (entry_price - sl).abs();

    for bar in recent.iter().rev() {
        if let Some(limit) = sniper_trigger(bar, pending.side, sl_dist, entry_price, cfg) {
            let mut sig = pending.signal.clone();
            sig.entry_mode = "sniper".to_string();
            sig.limit_entry_price = limit;
            return TriggerResult::Fire { signal: sig, limit_price: limit };
        }
    }

    TriggerResult::Waiting
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Inspect a single 1m bar for a valid sniper trigger.
/// Returns the limit entry price if triggered, `None` otherwise.
fn sniper_trigger(
    bar: &KlineBar,
    side: Side,
    sl_dist: f64,
    setup_price: f64,
    cfg: &SniperConfig,
) -> Option<f64> {
    let range = bar.high - bar.low;
    if range < 1e-12 { return None; }

    let rejection = match side {
        Side::Long => {
            // Bullish pin: long lower wick, close near top
            let lower_wick = (bar.open.min(bar.close) - bar.low).max(0.0);
            lower_wick / range
        }
        Side::Short => {
            // Bearish pin: long upper wick, close near bottom
            let upper_wick = (bar.high - bar.open.max(bar.close)).max(0.0);
            upper_wick / range
        }
    };

    if rejection < cfg.sniper_min_wick_rejection {
        return None;
    }

    // Pullback must not be too deep into the SL distance
    let pullback = match side {
        Side::Long => (setup_price - bar.low).max(0.0),
        Side::Short => (bar.high - setup_price).max(0.0),
    };
    if sl_dist > 0.0 && pullback / sl_dist > cfg.sniper_max_pullback_pct {
        return None;
    }

    // Entry price: opposite end of the trigger candle ± offset for a small edge
    let limit = match side {
        Side::Long => {
            // Enter slightly above the close of the pin-bar (filled on next push)
            bar.close * (1.0 + cfg.limit_offset_pct)
        }
        Side::Short => {
            bar.close * (1.0 - cfg.limit_offset_pct)
        }
    };

    Some(limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::KlineBar;

    fn bar(open: f64, high: f64, low: f64, close: f64) -> KlineBar {
        KlineBar { symbol: String::new(), open, high, low, close, volume: 100.0, amount: 0.0, timestamp: 0 }
    }

    fn cfg() -> SniperConfig {
        SniperConfig {
            entry_mode: crate::config::EntryMode::Sniper,
            limit_offset_pct: 0.001,
            limit_ttl_sec: 30,
            sniper_lookback_bars: 3,
            sniper_min_wick_rejection: 0.4,
            sniper_max_pullback_pct: 0.6,
            htf_setup_expiry_sec: 300,
        }
    }

    #[test]
    fn detects_bullish_pin() {
        let c = cfg();
        // Strong bullish pin: open/close near top (98-99), long lower wick to 80.
        // lower_wick = min(open,close) - low = 98 - 80 = 18; range = 100-80 = 20
        // rejection = 18/20 = 0.90 > 0.4 ✓
        // pullback = setup(98) - low(80) = 18; sl_dist = 50 → 18/50 = 0.36 < 0.6 ✓
        let b = bar(98.0, 100.0, 80.0, 99.0);
        let result = sniper_trigger(&b, Side::Long, 50.0, 98.0, &c);
        assert!(result.is_some(), "expected bullish pin trigger");
    }

    #[test]
    fn detects_bearish_pin() {
        let c = cfg();
        // Strong bearish pin: open/close near bottom (101-102), long upper wick to 120.
        // upper_wick = high - max(open,close) = 120 - 102 = 18; range = 120-100 = 20
        // rejection = 18/20 = 0.90 > 0.4 ✓
        // pullback = high(120) - setup(102) = 18; sl_dist = 50 → 18/50 = 0.36 < 0.6 ✓
        let b = bar(102.0, 120.0, 100.0, 101.0);
        let result = sniper_trigger(&b, Side::Short, 50.0, 102.0, &c);
        assert!(result.is_some(), "expected bearish pin trigger");
    }

    #[test]
    fn rejects_non_pin() {
        let c = cfg();
        // Full body candle (open 100 → close 100.8), tiny wicks → low rejection
        let b = bar(100.0, 101.0, 99.5, 100.8);
        assert!(sniper_trigger(&b, Side::Long, 5.0, 100.0, &c).is_none());
    }
}
