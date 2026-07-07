//! Adaptive trailing stop — widens as profit grows so runners aren't cut on pullbacks.

use crate::config::RiskConfig;

#[derive(Debug, Clone, Default)]
pub struct TrailingTrack {
    pub stop: f64,
    /// Best favorable price since entry (high for long, low for short).
    pub peak_favorable: f64,
}

impl TrailingTrack {
    pub fn seed(initial_sl: f64) -> Self {
        Self {
            stop: initial_sl,
            peak_favorable: 0.0,
        }
    }
}

/// Trail distance for the current unrealized move (wider on bigger pumps/dumps).
pub fn effective_trail_pct(cfg: &RiskConfig, move_pct: f64) -> f64 {
    if move_pct >= cfg.trailing_runner_activation_pct {
        cfg.trailing_runner_stop_pct
    } else if move_pct >= cfg.trailing_extended_activation_pct {
        cfg.trailing_extended_stop_pct
    } else {
        cfg.trailing_stop_pct
    }
}

/// Ratchet stop-loss from peak favorable price; returns the active stop level.
pub fn update_trailing(
    side: &str,
    entry: f64,
    price: f64,
    track: &mut TrailingTrack,
    cfg: &RiskConfig,
) -> f64 {
    if entry <= 0.0 || price <= 0.0 {
        return track.stop;
    }

    if track.peak_favorable <= 0.0 {
        track.peak_favorable = entry;
    }
    if side == "long" {
        track.peak_favorable = track.peak_favorable.max(price);
    } else {
        track.peak_favorable = track.peak_favorable.min(price);
    }

    let move_pct = if side == "long" {
        (track.peak_favorable - entry) / entry
    } else {
        (entry - track.peak_favorable) / entry
    };

    if move_pct < cfg.trailing_activation_pct {
        return track.stop;
    }

    let trail_pct = effective_trail_pct(cfg, move_pct);
    let new_trail = if side == "long" {
        track.peak_favorable * (1.0 - trail_pct)
    } else {
        track.peak_favorable * (1.0 + trail_pct)
    };

    track.stop = if track.stop <= 0.0 {
        new_trail
    } else if side == "long" {
        new_trail.max(track.stop)
    } else {
        new_trail.min(track.stop)
    };
    track.stop
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RiskConfig;

    fn test_cfg() -> RiskConfig {
        serde_json::from_value(serde_json::json!({
            "trailing_stop_pct": 0.01,
            "trailing_activation_pct": 0.015,
            "trailing_extended_activation_pct": 0.03,
            "trailing_extended_stop_pct": 0.015,
            "trailing_runner_activation_pct": 0.06,
            "trailing_runner_stop_pct": 0.025,
        }))
        .expect("test cfg")
    }

    #[test]
    fn trail_widens_on_large_move() {
        let cfg = test_cfg();
        assert_eq!(effective_trail_pct(&cfg, 0.02), 0.01);
        assert_eq!(effective_trail_pct(&cfg, 0.04), 0.015);
        assert_eq!(effective_trail_pct(&cfg, 0.08), 0.025);
    }

    #[test]
    fn long_trail_ratchet_from_peak() {
        let cfg = test_cfg();
        let mut track = TrailingTrack::seed(95.0);
        let sl = update_trailing("long", 100.0, 106.0, &mut track, &cfg);
        assert!(sl > 95.0);
        let sl2 = update_trailing("long", 100.0, 104.0, &mut track, &cfg);
        assert!(sl2 >= sl);
    }
}
