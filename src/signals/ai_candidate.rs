//! AI candidate generator (Phase 1 of the AI-first pipeline).
//!
//! Emits broad long/short candidates with only *sanity* hard filters (price,
//! liquidity, ATR, funding via `risk::filters`). All the old strategy-quality
//! gates (confluence counts, composite thresholds, structure requirements) are
//! gone — their signals are attached as soft metrics for the ML decision core,
//! which is the actual quality gate.

use chrono::Utc;
use serde_json::json;

use crate::config::AppConfig;
use crate::exchange::KlineBar;
use crate::risk::filters::passes_risk_filters;
use crate::signals::indicators::{atr_pct, ewma, momentum_score, price_change_pct, volume_surge_ratio, zscore};
use crate::signals::state::{Side, SymbolState};
use crate::signals::types::{PumpSignal, SignalStrength};
use crate::signals::zones::{build_zones, structure_aligned, zone_confluence_score};

pub struct AiCandidateEngine;

impl AiCandidateEngine {
    /// Evaluate one symbol. Returns a candidate signal or `None` when the
    /// symbol is warming up, flat, in cooldown, or fails a sanity filter.
    pub fn evaluate(cfg: &AppConfig, state: &SymbolState) -> Option<PumpSignal> {
        let ai = &cfg.ai;
        let ticker = state.last_ticker.as_ref()?;

        // Warm-up: enough data to compute ATR/zones and a directional move.
        if state.klines.len() < 15 || state.prices.len() < ai.move_lookback_ticks + 1 {
            return None;
        }

        // Per-symbol rate limiter — dedupe, not a quality gate.
        if let Some(last) = state.last_signal_at {
            let elapsed = (Utc::now() - last).num_seconds();
            if elapsed < ai.signal_cooldown_sec as i64 {
                return None;
            }
        }

        // Direction from the recent tick move; skip dead/flat symbols.
        let move_pct = price_change_pct(&state.prices, ai.move_lookback_ticks);
        if move_pct.abs() < ai.min_move_pct {
            return None;
        }
        let side = if move_pct >= 0.0 { Side::Long } else { Side::Short };

        // Sanity filters: min price, 24h turnover, ATR ceiling. Funding is not
        // polled per tick, so it is left to the execution layer.
        if !passes_risk_filters(
            &state.symbol,
            ticker,
            &state.klines,
            &cfg.risk,
            &cfg.scanner,
            None,
            side,
            false,
        ) {
            return None;
        }

        let last_price = ticker.last_price;
        let bars: Vec<(f64, f64, f64)> =
            state.klines.iter().map(|b| (b.high, b.low, b.close)).collect();
        let atr = atr_pct(&bars);

        // ── Soft metrics (features for the ML core, never gates) ────────────
        let (vol_surge, vol_z) = volume_metrics(&state.klines);
        let momentum = momentum_score(move_pct.abs(), 3.5);
        let zones = build_zones(&state.klines, &cfg.zones);
        let (zone_score, zone_message) =
            zone_confluence_score(last_price, side, &zones, cfg.zones.proximity_pct);
        let structure_ok = structure_aligned(&state.klines, side);

        let mut confluences: Vec<String> = Vec::new();
        if vol_surge >= 1.5 || vol_z >= 1.0 {
            confluences.push("volume".into());
        }
        if momentum >= 40.0 {
            confluences.push("momentum".into());
        }
        if zone_score > 0.0 {
            confluences.push("zone".into());
        }
        if structure_ok {
            confluences.push("structure".into());
        }

        // Informational blend, not a threshold. The ML pipeline overwrites the
        // effective score and probability downstream.
        let vol_component = ((vol_surge / 3.0 * 100.0).min(100.0)).max((vol_z / 3.0 * 100.0).clamp(0.0, 100.0));
        let composite = (vol_component * 0.30
            + momentum * 0.30
            + zone_score * 0.25
            + if structure_ok { 100.0 } else { 0.0 } * 0.15)
            .clamp(0.0, 100.0);

        let strength = if composite >= 70.0 {
            SignalStrength::Strong
        } else if composite >= 55.0 {
            SignalStrength::Moderate
        } else {
            SignalStrength::Weak
        };

        // ── Stops & targets: ATR-based, floored by the configured default ───
        let sl_pct = ((atr / 100.0) * ai.atr_sl_mult).max(cfg.risk.default_sl_pct);
        let sl_dist = last_price * sl_pct;
        let (projected_stop_loss, projected_take_profits) = match side {
            Side::Long => (
                last_price - sl_dist,
                ai.tp_r_multiples.iter().map(|r| last_price + sl_dist * r).collect::<Vec<f64>>(),
            ),
            Side::Short => (
                last_price + sl_dist,
                ai.tp_r_multiples.iter().map(|r| last_price - sl_dist * r).collect::<Vec<f64>>(),
            ),
        };

        let side_str = match side {
            Side::Long => "long",
            Side::Short => "short",
        };
        let confluence_details = vec![
            json!({ "name": "volume", "surge_ratio": round2(vol_surge), "zscore": round2(vol_z) }),
            json!({ "name": "momentum", "move_pct": round2(move_pct), "score": round2(momentum) }),
            json!({ "name": "zone", "score": round2(zone_score), "detail": zone_message }),
            json!({ "name": "structure", "aligned": structure_ok }),
            json!({ "name": "volatility", "atr_pct": round2(atr) }),
        ];

        Some(PumpSignal {
            symbol: state.symbol.clone(),
            strategy: "ai".into(),
            composite_score: round2(composite),
            strength,
            last_price,
            price_change_pct: move_pct,
            volume_surge_ratio: round2(vol_surge),
            confluence_count: confluences.len() as u32,
            confluences,
            confluence_details,
            setup_probability_pct: 0.0, // ML pipeline fills this in
            suggested_risk_pct: cfg.risk.max_risk_per_trade,
            suggested_leverage: ai.base_leverage,
            zone_score: round2(zone_score),
            zone_message,
            sizing_tier: "base".into(),
            message: format!(
                "AI candidate {side_str} {} · move {move_pct:.2}% · vol x{vol_surge:.1} · atr {atr:.1}%",
                state.symbol
            ),
            generated_at: Utc::now(),
            signal_id: None,
            projected_stop_loss,
            projected_take_profits,
            tp_close_fractions: ai.tp_close_fractions.clone(),
            ml_features: Vec::new(),
            entry_mode: "market".into(),
            limit_entry_price: 0.0,
            expected_value_r: 0.0,
            reward_risk: 0.0,
            decision_reason: String::new(),
        })
    }
}

/// Volume surge ratio (last bar vs EWMA baseline of prior bars) and z-score.
fn volume_metrics(klines: &[KlineBar]) -> (f64, f64) {
    if klines.len() < 5 {
        return (0.0, 0.0);
    }
    let volumes: Vec<f64> = klines.iter().map(|b| b.volume).collect();
    let (history, last) = volumes.split_at(volumes.len() - 1);
    let current = last[0];
    let baseline = ewma(history, 20);
    (
        volume_surge_ratio(current, baseline),
        zscore(current, history),
    )
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::TickerSnapshot;

    fn make_state(symbol: &str, base_price: f64, tick_move: f64, n_klines: usize) -> SymbolState {
        let mut state = SymbolState::new(symbol);
        for i in 0..n_klines {
            let p = base_price * (1.0 + 0.001 * i as f64);
            state.klines.push(KlineBar {
                symbol: symbol.into(),
                timestamp: 1_700_000_000 + (i as i64) * 60,
                open: p,
                high: p * 1.002,
                low: p * 0.998,
                close: p,
                volume: 1000.0 + if i == n_klines - 1 { 3000.0 } else { 0.0 },
                amount: 0.0,
            });
        }
        let last_close = state.klines.last().unwrap().close;
        for i in 0..10 {
            state
                .prices
                .push(last_close * (1.0 + tick_move / 100.0 * (i as f64 / 9.0)));
        }
        let final_price = *state.prices.last().unwrap();
        state.last_ticker = Some(TickerSnapshot {
            symbol: symbol.into(),
            last_price: final_price,
            volume24: 5_000_000.0,
            amount24: 5_000_000.0,
            rise_fall_rate: tick_move / 100.0,
            fair_price: final_price,
            high24: final_price * 1.05,
            low24: final_price * 0.95,
            timestamp: Utc::now(),
        });
        state
    }

    fn test_cfg() -> AppConfig {
        // Every section field carries a serde default, so empty maps hydrate a
        // complete config without touching config/settings.yaml.
        let yaml = "mexc: {}\nscanner: {}\nzones: {}\ntrading: {}\nrisk: {}\nexecution: {}\nstorage: {}\nml: {}\nserver: {}\n";
        let mut cfg: AppConfig = serde_yaml::from_str(yaml).expect("test config");
        cfg.scanner.min_24h_turnover_usdt = 100_000.0;
        cfg.scanner.min_price_usdt = 0.01;
        cfg
    }

    #[test]
    fn emits_long_candidate_on_upward_move() {
        let cfg = test_cfg();
        let state = make_state("TEST_USDT", 1.0, 0.8, 30);
        let sig = AiCandidateEngine::evaluate(&cfg, &state).expect("candidate expected");
        assert_eq!(sig.strategy, "ai");
        assert!(sig.price_change_pct > 0.0);
        assert!(sig.projected_stop_loss < sig.last_price);
        assert_eq!(sig.projected_take_profits.len(), 3);
        assert!(sig.projected_take_profits[0] > sig.last_price);
    }

    #[test]
    fn emits_short_candidate_on_downward_move() {
        let cfg = test_cfg();
        let state = make_state("TEST_USDT", 1.0, -0.8, 30);
        let sig = AiCandidateEngine::evaluate(&cfg, &state).expect("candidate expected");
        assert!(sig.price_change_pct < 0.0);
        assert!(sig.projected_stop_loss > sig.last_price);
        assert!(sig.projected_take_profits[0] < sig.last_price);
    }

    #[test]
    fn flat_symbol_is_skipped() {
        let cfg = test_cfg();
        let state = make_state("TEST_USDT", 1.0, 0.0, 30);
        assert!(AiCandidateEngine::evaluate(&cfg, &state).is_none());
    }

    #[test]
    fn warming_symbol_is_skipped() {
        let cfg = test_cfg();
        let state = make_state("TEST_USDT", 1.0, 0.8, 5);
        assert!(AiCandidateEngine::evaluate(&cfg, &state).is_none());
    }

    #[test]
    fn cooldown_blocks_repeat_candidates() {
        let cfg = test_cfg();
        let mut state = make_state("TEST_USDT", 1.0, 0.8, 30);
        state.last_signal_at = Some(Utc::now());
        assert!(AiCandidateEngine::evaluate(&cfg, &state).is_none());
    }
}
