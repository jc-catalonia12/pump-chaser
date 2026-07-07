//! Soft training labels derived from R-multiple (PnL / initial risk).

/// Initial risk in price units for a position.
pub fn initial_risk_usd(entry: f64, sl: f64, size: f64, side_long: bool) -> f64 {
    if entry <= 0.0 || sl <= 0.0 || size <= 0.0 {
        return 0.0;
    }
    let dist = if side_long {
        (entry - sl).max(0.0)
    } else {
        (sl - entry).max(0.0)
    };
    dist * size
}

/// R-multiple = realized PnL / initial risk at entry.
pub fn compute_r_multiple(pnl: f64, entry: f64, sl: f64, size: f64, side_long: bool) -> f64 {
    let risk = initial_risk_usd(entry, sl, size, side_long);
    if risk <= 1e-9 {
        return if pnl > 0.0 { 1.0 } else if pnl < 0.0 { -1.0 } else { 0.0 };
    }
    pnl / risk
}

/// Map R-multiple to a soft label in [0, 1] for online learning.
pub fn soft_label_from_r(r: f64) -> f64 {
    if r >= 1.5 {
        1.0
    } else if r >= 0.5 {
        0.85
    } else if r >= 0.1 {
        0.70
    } else if r >= -0.1 {
        0.55
    } else if r >= -0.5 {
        0.35
    } else if r >= -1.0 {
        0.15
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_stop_is_zero_label() {
        let r = compute_r_multiple(-10.0, 100.0, 98.0, 5.0, true);
        assert!(r < -0.9);
        assert!(soft_label_from_r(r) <= 0.15);
    }

    #[test]
    fn big_win_is_high_label() {
        assert!(soft_label_from_r(2.0) >= 0.99);
    }
}
