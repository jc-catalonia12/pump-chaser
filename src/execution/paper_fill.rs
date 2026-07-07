//! Realistic paper fill prices with configurable slippage.

/// Apply adverse slippage to a paper fill.
pub fn apply_paper_slippage(price: f64, side: &str, is_entry: bool, slippage_pct: f64) -> f64 {
    if price <= 0.0 || slippage_pct <= 0.0 {
        return price;
    }
    let slip = slippage_pct.clamp(0.0, 0.05);
    match (side, is_entry) {
        ("long", true) | ("short", false) => price * (1.0 + slip),
        ("long", false) | ("short", true) => price * (1.0 - slip),
        _ => price,
    }
}

/// Net PnL for a paper close chunk including slippage and round-trip taker fees.
pub fn paper_chunk_pnl(
    entry: f64,
    raw_exit: f64,
    qty: f64,
    side: &str,
    slippage_pct: f64,
    fee_rate: f64,
) -> (f64, f64) {
    let exit = apply_paper_slippage(raw_exit, side, false, slippage_pct);
    let gross = if side == "long" {
        (exit - entry) * qty
    } else {
        (entry - exit) * qty
    };
    let fees = (entry * qty + exit * qty) * fee_rate;
    (exit, gross - fees)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_entry_slips_up() {
        let p = apply_paper_slippage(100.0, "long", true, 0.001);
        assert!(p > 100.0);
    }

    #[test]
    fn long_exit_slips_down() {
        let p = apply_paper_slippage(100.0, "long", false, 0.001);
        assert!(p < 100.0);
    }
}
