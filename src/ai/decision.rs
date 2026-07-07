//! Unified trade decision & sizing authority — Phase 5.
//!
//! This is the single go/no-go authority in the AI pipeline. It sits *after*
//! the ML core has produced a calibrated win probability and Kelly base size,
//! and *before* the preserved `RiskManager` safety net. It fuses four inputs
//! into one decision:
//!
//!   1. ML win probability `p` (the statistical edge)
//!   2. Expected value in R: `EV = p*reward_risk - (1-p)` (the payoff math)
//!   3. LLM regime alignment (does the market regime agree with the trade?)
//!   4. Sentiment (directional news bias)
//!
//! Output: approve/reject + `size_scale` / `leverage_scale` multipliers +
//! a human-readable reason. Sizing multipliers are applied on top of the ML
//! Kelly base so the decision layer expresses *conviction* (regime/sentiment
//! alignment) without discarding the statistically-optimal base size.
//!
//! Graceful degradation: when the LLM regime is neutral (Ollama offline →
//! `confidence == 0`, all flags false) every regime term multiplies to zero,
//! so the decision collapses to a pure EV / reward-risk gate on the ML edge.

use crate::config::DecisionConfig;
use crate::ml::MarketRegime;

/// Everything the decision engine needs about a single candidate.
#[derive(Debug, Clone)]
pub struct DecisionInputs {
    /// ML calibrated win probability, 0..1.
    pub win_prob: f64,
    pub side_long: bool,
    pub entry: f64,
    pub stop_loss: f64,
    /// Primary (first) take-profit target.
    pub take_profit: f64,
    pub regime: MarketRegime,
    /// Global news sentiment, -1..1.
    pub global_sentiment: f64,
    /// Per-symbol news sentiment, -1..1.
    pub symbol_sentiment: f64,
}

/// The decision engine's verdict for one candidate.
#[derive(Debug, Clone)]
pub struct TradeDecision {
    pub approved: bool,
    pub reason: String,
    /// Expected value in R multiples.
    pub expected_value_r: f64,
    /// Reward:risk ratio (first TP distance / SL distance).
    pub reward_risk: f64,
    /// Confidence-weighted regime alignment, -1..1 (+ = regime favors trade).
    pub regime_alignment: f64,
    /// Multiplier applied to `suggested_risk_pct`.
    pub size_scale: f64,
    /// Multiplier applied to `suggested_leverage`.
    pub leverage_scale: f64,
}

pub struct DecisionEngine;

impl DecisionEngine {
    /// Evaluate a candidate. Pure function of `(cfg, inputs)` — deterministic
    /// and side-effect free, which is what makes it unit-testable and usable
    /// as-is inside the backtest replay (Phase 6).
    pub fn decide(cfg: &DecisionConfig, inp: &DecisionInputs) -> TradeDecision {
        let reward_risk = reward_risk_ratio(inp.entry, inp.stop_loss, inp.take_profit);
        let p = inp.win_prob.clamp(0.0, 1.0);
        let expected_value_r = p * reward_risk - (1.0 - p);

        let alignment = Self::regime_alignment(inp);

        // --- Hard gates (go/no-go) ---
        if reward_risk < cfg.min_reward_risk {
            return TradeDecision {
                approved: false,
                reason: format!(
                    "reject: reward:risk {reward_risk:.2} < min {:.2}",
                    cfg.min_reward_risk
                ),
                expected_value_r,
                reward_risk,
                regime_alignment: alignment,
                size_scale: cfg.min_size_scale,
                leverage_scale: 1.0,
            };
        }
        if expected_value_r < cfg.min_expected_value {
            return TradeDecision {
                approved: false,
                reason: format!(
                    "reject: EV {expected_value_r:+.2}R < min {:+.2}R (p={:.0}%, rr={reward_risk:.2})",
                    cfg.min_expected_value,
                    p * 100.0
                ),
                expected_value_r,
                reward_risk,
                regime_alignment: alignment,
                size_scale: cfg.min_size_scale,
                leverage_scale: 1.0,
            };
        }
        if inp.regime.confidence >= cfg.regime_veto_confidence
            && alignment <= -cfg.regime_veto_alignment
        {
            return TradeDecision {
                approved: false,
                reason: format!(
                    "reject: regime opposes trade (align {alignment:+.2}, conf {:.0}%)",
                    inp.regime.confidence * 100.0
                ),
                expected_value_r,
                reward_risk,
                regime_alignment: alignment,
                size_scale: cfg.min_size_scale,
                leverage_scale: 1.0,
            };
        }

        // --- Approved: compute conviction-based sizing ---
        let (size_scale, leverage_scale) = Self::sizing(cfg, inp, alignment);

        TradeDecision {
            approved: true,
            reason: format!(
                "approve: EV {expected_value_r:+.2}R, rr {reward_risk:.2}, p {:.0}%, align {alignment:+.2}, size x{size_scale:.2}",
                p * 100.0
            ),
            expected_value_r,
            reward_risk,
            regime_alignment: alignment,
            size_scale,
            leverage_scale,
        }
    }

    /// Confidence-weighted alignment of the market regime with the trade side.
    /// Positive = regime favors the trade, negative = opposes. Zero when the
    /// LLM is neutral/offline (confidence 0).
    fn regime_alignment(inp: &DecisionInputs) -> f64 {
        let side_sign = if inp.side_long { 1.0 } else { -1.0 };
        // BTC bias: positive = bullish, aligns with longs.
        let mut raw = inp.regime.btc_bias * side_sign;
        // Risk-off favors shorts / flat: penalize longs, reward shorts.
        if inp.regime.risk_off {
            raw -= 0.5 * side_sign;
        }
        (raw.clamp(-1.0, 1.0) * inp.regime.confidence).clamp(-1.0, 1.0)
    }

    fn sizing(cfg: &DecisionConfig, inp: &DecisionInputs, alignment: f64) -> (f64, f64) {
        let mut size = 1.0;
        // Regime alignment: scale size up when the regime agrees, down when it
        // disagrees (but not to a veto — that was handled above).
        size *= 1.0 + cfg.regime_size_boost * alignment;
        // High-volatility regime: trim size regardless of direction.
        if inp.regime.high_vol {
            size *= 1.0 - cfg.high_vol_size_haircut;
        }
        // Directional sentiment nudge (blend global + symbol).
        let sentiment = 0.5 * inp.global_sentiment + 0.5 * inp.symbol_sentiment;
        let side_sign = if inp.side_long { 1.0 } else { -1.0 };
        size *= 1.0 + cfg.sentiment_size_weight * sentiment * side_sign;
        let size = size.clamp(cfg.min_size_scale, cfg.max_size_scale);

        // Leverage is scaled more conservatively than notional size: it only
        // reacts (at half strength) to regime alignment and the high-vol
        // haircut, and is clamped tightly around 1.0 so a hot regime never
        // balloons liquidation risk.
        let mut leverage = 1.0 + 0.5 * cfg.regime_size_boost * alignment;
        if inp.regime.high_vol {
            leverage *= 1.0 - cfg.high_vol_size_haircut;
        }
        let leverage = leverage.clamp(0.5, 1.15);

        (size, leverage)
    }
}

/// Reward:risk from entry / stop / target. 0.0 when the stop distance is
/// degenerate (caller then fails the `min_reward_risk` gate).
pub fn reward_risk_ratio(entry: f64, stop_loss: f64, take_profit: f64) -> f64 {
    let risk = (entry - stop_loss).abs();
    let reward = (take_profit - entry).abs();
    if risk > 1e-12 {
        reward / risk
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn neutral_regime() -> MarketRegime {
        MarketRegime::default()
    }

    fn base_inputs() -> DecisionInputs {
        // Long, entry 100, SL 98 (2 risk), TP 104 (4 reward) => rr 2.0.
        DecisionInputs {
            win_prob: 0.6,
            side_long: true,
            entry: 100.0,
            stop_loss: 98.0,
            take_profit: 104.0,
            regime: neutral_regime(),
            global_sentiment: 0.0,
            symbol_sentiment: 0.0,
        }
    }

    #[test]
    fn reward_risk_is_computed() {
        assert!((reward_risk_ratio(100.0, 98.0, 104.0) - 2.0).abs() < 1e-9);
        assert_eq!(reward_risk_ratio(100.0, 100.0, 104.0), 0.0);
    }

    #[test]
    fn positive_ev_neutral_regime_approves_at_base_size() {
        let cfg = DecisionConfig::default();
        let d = DecisionEngine::decide(&cfg, &base_inputs());
        assert!(d.approved, "{}", d.reason);
        // EV = 0.6*2 - 0.4 = 0.8R
        assert!((d.expected_value_r - 0.8).abs() < 1e-9);
        // Neutral regime + zero sentiment => no scaling.
        assert!((d.size_scale - 1.0).abs() < 1e-9);
        assert!((d.leverage_scale - 1.0).abs() < 1e-9);
    }

    #[test]
    fn negative_ev_is_rejected() {
        let cfg = DecisionConfig::default();
        let mut inp = base_inputs();
        // Low prob + tight reward => negative EV.
        inp.win_prob = 0.2;
        inp.take_profit = 101.0; // rr = 0.5 -> also below min_reward_risk
        let d = DecisionEngine::decide(&cfg, &inp);
        assert!(!d.approved);
    }

    #[test]
    fn low_reward_risk_is_rejected_even_with_high_prob() {
        let cfg = DecisionConfig::default();
        let mut inp = base_inputs();
        inp.win_prob = 0.95;
        inp.take_profit = 100.5; // rr = 0.25 < 0.8
        let d = DecisionEngine::decide(&cfg, &inp);
        assert!(!d.approved);
        assert!(d.reason.contains("reward:risk"));
    }

    #[test]
    fn strongly_opposing_confident_regime_vetoes() {
        let cfg = DecisionConfig::default();
        let mut inp = base_inputs();
        // Long trade, confident bearish regime.
        inp.regime = MarketRegime {
            btc_bias: -1.0,
            confidence: 0.9,
            risk_off: true,
            ..Default::default()
        };
        let d = DecisionEngine::decide(&cfg, &inp);
        assert!(!d.approved, "{}", d.reason);
        assert!(d.reason.contains("regime opposes"));
        assert!(d.regime_alignment < 0.0);
    }

    #[test]
    fn aligned_confident_regime_boosts_size() {
        let cfg = DecisionConfig::default();
        let mut inp = base_inputs();
        inp.regime = MarketRegime {
            btc_bias: 1.0,
            confidence: 1.0,
            trending: true,
            ..Default::default()
        };
        let d = DecisionEngine::decide(&cfg, &inp);
        assert!(d.approved, "{}", d.reason);
        assert!(d.size_scale > 1.0, "expected boost, got {}", d.size_scale);
        assert!(d.regime_alignment > 0.0);
    }

    #[test]
    fn high_vol_regime_trims_size() {
        let cfg = DecisionConfig::default();
        let mut inp = base_inputs();
        inp.regime = MarketRegime {
            high_vol: true,
            confidence: 0.5,
            ..Default::default()
        };
        let d = DecisionEngine::decide(&cfg, &inp);
        assert!(d.approved, "{}", d.reason);
        assert!(d.size_scale < 1.0, "expected haircut, got {}", d.size_scale);
    }

    #[test]
    fn size_scale_is_clamped() {
        let mut cfg = DecisionConfig::default();
        cfg.regime_size_boost = 5.0; // absurd boost to test clamp
        let mut inp = base_inputs();
        inp.regime = MarketRegime {
            btc_bias: 1.0,
            confidence: 1.0,
            ..Default::default()
        };
        let d = DecisionEngine::decide(&cfg, &inp);
        assert!(d.size_scale <= cfg.max_size_scale + 1e-9);
    }

    #[test]
    fn short_trade_aligns_with_bearish_regime() {
        let cfg = DecisionConfig::default();
        let mut inp = base_inputs();
        // Short: entry 100, SL 102, TP 96 => rr 2.0.
        inp.side_long = false;
        inp.stop_loss = 102.0;
        inp.take_profit = 96.0;
        inp.regime = MarketRegime {
            btc_bias: -0.8,
            confidence: 0.9,
            ..Default::default()
        };
        let d = DecisionEngine::decide(&cfg, &inp);
        assert!(d.approved, "{}", d.reason);
        assert!(d.regime_alignment > 0.0, "short should align with bearish regime");
    }
}
