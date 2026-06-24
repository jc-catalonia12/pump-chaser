//! Volume-pump strategy — abnormal 1m volume + universe rank, fast limit entry.

use chrono::Utc;
use serde_json::json;

use crate::config::{EntryMode, PumpConfig, SharedAppConfig};
use crate::risk::filters::passes_risk_filters;
use crate::signals::confluence::ScanDiagnosis;
use crate::signals::indicators::{
    ewma, liquidity_score, momentum_score, oi_proxy_score, price_change_pct, volume_surge_ratio,
    zscore,
};
use crate::signals::state::{Side, SymbolState};
use crate::signals::{PumpSignal, SignalStrength};

pub struct VolumePumpEngine {
    config: SharedAppConfig,
}

impl VolumePumpEngine {
    pub fn new(config: SharedAppConfig) -> Self {
        Self { config }
    }

    pub fn in_cooldown(&self, state: &SymbolState) -> bool {
        let Some(last) = state.last_pump_at else {
            return false;
        };
        let elapsed = Utc::now().signed_duration_since(last).num_seconds().max(0) as u64;
        elapsed < self.config.read().unwrap().pump.alert_cooldown_sec
    }

    pub fn evaluate(
        &self,
        state: &SymbolState,
        universe_rank: Option<u32>,
        funding_rate: Option<f64>,
    ) -> Option<PumpSignal> {
        self.try_setup(state, universe_rank, funding_rate, false)
    }

    fn try_setup(
        &self,
        state: &SymbolState,
        universe_rank: Option<u32>,
        funding_rate: Option<f64>,
        diagnose_only: bool,
    ) -> Option<PumpSignal> {
        let app = self.config.read().unwrap();
        let cfg = &app.pump;
        if !cfg.enabled {
            return None;
        }
        let ticker = state.last_ticker.as_ref()?;
        if state.klines.len() < 10 {
            return None;
        }

        let turnover = if ticker.amount24 > 0.0 {
            ticker.amount24
        } else {
            ticker.volume24 * ticker.last_price
        };
        if turnover < cfg.min_24h_turnover_usdt || turnover > cfg.max_24h_turnover_usdt {
            return None;
        }

        let mut pct = price_change_pct(&state.prices, 5);
        if pct == 0.0 {
            let closes: Vec<f64> = state.klines.iter().map(|b| b.close).collect();
            pct = price_change_pct(&closes, 5);
        }
        let move_pct = pct.abs();
        let side = if pct > 0.0 { Side::Long } else { Side::Short };

        if !passes_risk_filters(
            &state.symbol,
            ticker,
            &state.klines,
            &app.risk,
            &app.scanner,
            funding_rate,
            side,
            false,
        ) {
            return None;
        }

        if move_pct < cfg.price_change_pct_min || move_pct > cfg.price_change_pct_max {
            return None;
        }

        let volumes: Vec<f64> = state.klines.iter().map(|b| b.volume).collect();
        let current_vol = *volumes.last().unwrap_or(&0.0);
        let baseline = if volumes.len() > 1 {
            ewma(&volumes[..volumes.len() - 1], cfg.ewma_span as usize)
        } else {
            current_vol
        };
        let hist = if volumes.len() > 1 {
            &volumes[..volumes.len() - 1]
        } else {
            volumes.as_slice()
        };
        let vol_z = zscore(current_vol, hist);
        let surge = volume_surge_ratio(current_vol, baseline);

        if surge < cfg.volume_surge_multiplier && vol_z < cfg.volume_zscore_threshold {
            return None;
        }

        if let Some(rank) = universe_rank {
            if rank > cfg.universe_rank_max {
                return None;
            }
        }

        let turnover_accel = turnover_velocity(&state.klines);
        let vol_component = (surge / cfg.volume_surge_multiplier.max(0.1) * 50.0
            + vol_z.min(5.0) * 10.0)
            .min(100.0);
        let mom = momentum_score(move_pct, cfg.price_change_pct_max);
        let liq = liquidity_score(turnover, cfg.min_24h_turnover_usdt, cfg.max_24h_turnover_usdt);
        let _oi = oi_proxy_score(ticker.amount24, ticker.volume24, ticker.last_price);

        let rank_bonus = universe_rank
            .map(|r| ((cfg.universe_rank_max + 1 - r.min(cfg.universe_rank_max)) as f64) * 12.0)
            .unwrap_or(50.0);
        let accel_score = (turnover_accel * 40.0).clamp(0.0, 100.0);

        let mut composite =
            vol_component * 0.40 + mom * 0.30 + accel_score * 0.15 + rank_bonus * 0.15;
        composite = composite.min(100.0);

        if composite < cfg.min_composite_score {
            return None;
        }

        if diagnose_only {
            return None;
        }

        let strength = if composite >= 80.0 {
            SignalStrength::Strong
        } else if composite >= 70.0 {
            SignalStrength::Moderate
        } else {
            SignalStrength::Weak
        };

        let (leverage, risk_pct, tier) = pump_sizing(composite, strength, cfg);
        let sl_pct = cfg.default_sl_pct;
        let (projected_sl, projected_tps) =
            projected_levels(ticker.last_price, side, sl_pct, cfg);

        let direction = if pct > 0.0 { "long" } else { "short" };
        let rank_msg = universe_rank
            .map(|r| format!("universe rank #{r}"))
            .unwrap_or_else(|| "universe rank n/a".into());
        let message = format!(
            "Volume pump {direction} [{tier}]: surge {surge:.1}x z={vol_z:.1} | {rank_msg} (~{composite:.0} score, {leverage}x)"
        );

        let entry_mode = match cfg.entry_mode {
            EntryMode::Market => "market",
            EntryMode::Limit => "limit",
            EntryMode::Sniper => "limit",
        };

        Some(PumpSignal {
            symbol: state.symbol.clone(),
            strategy: "volume_pump".into(),
            composite_score: composite,
            strength,
            last_price: ticker.last_price,
            price_change_pct: pct,
            volume_surge_ratio: surge,
            confluence_count: 0,
            confluences: vec!["volume".into(), "momentum".into()],
            confluence_details: vec![
                json!({"key":"vol_surge","label":"Volume surge","active":true,"score":vol_component.round(),"detail":format!("{surge:.2}x")}),
                json!({"key":"vol_z","label":"Volume z-score","active":true,"score":(vol_z*20.0).min(100.0).round(),"detail":format!("z={vol_z:.2}")}),
                json!({"key":"turnover_accel","label":"Turnover accel","active":turnover_accel>1.0,"score":accel_score.round(),"detail":format!("{turnover_accel:.2}x")}),
                json!({"key":"universe_rank","label":"Universe rank","active":universe_rank.is_some_and(|r| r<=cfg.universe_rank_max),"score":rank_bonus.round(),"detail":rank_msg}),
                json!({"key":"liquidity","label":"Liquidity","active":liq>0.0,"score":liq.round(),"detail":format!("turnover {:.0}", turnover)}),
            ],
            setup_probability_pct: composite * 0.85,
            suggested_risk_pct: risk_pct,
            suggested_leverage: leverage,
            zone_score: 0.0,
            zone_message: String::new(),
            sizing_tier: tier,
            message,
            generated_at: Utc::now(),
            signal_id: None,
            projected_stop_loss: projected_sl,
            projected_take_profits: projected_tps,
            tp_close_fractions: cfg.tp_close_fractions.clone(),
            ml_features: Vec::new(),
            entry_mode: entry_mode.to_string(),
            limit_entry_price: 0.0,
        })
    }

    pub fn diagnose(
        &self,
        state: &SymbolState,
        universe_rank: Option<u32>,
        funding_rate: Option<f64>,
    ) -> ScanDiagnosis {
        let app = self.config.read().unwrap();
        let cfg = &app.pump;
        if !cfg.enabled {
            return ScanDiagnosis {
                action: "skipped".into(),
                message: "Volume pump engine disabled".into(),
                composite_score: None,
                confluence_count: None,
                side: None,
            };
        }
        let Some(ticker) = state.last_ticker.as_ref() else {
            return ScanDiagnosis {
                action: "warming".into(),
                message: "Waiting for live ticker".into(),
                composite_score: None,
                confluence_count: None,
                side: None,
            };
        };
        if state.klines.len() < 10 {
            return ScanDiagnosis {
                action: "warming".into(),
                message: format!("Collecting klines {}/10", state.klines.len()),
                composite_score: None,
                confluence_count: None,
                side: None,
            };
        }

        let turnover = if ticker.amount24 > 0.0 {
            ticker.amount24
        } else {
            ticker.volume24 * ticker.last_price
        };
        if turnover < cfg.min_24h_turnover_usdt {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: "24h turnover below pump minimum".into(),
                composite_score: None,
                confluence_count: None,
                side: None,
            };
        }
        if turnover > cfg.max_24h_turnover_usdt {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: "24h turnover too high (large-cap filter)".into(),
                composite_score: None,
                confluence_count: None,
                side: None,
            };
        }

        let mut pct = price_change_pct(&state.prices, 5);
        if pct == 0.0 {
            let closes: Vec<f64> = state.klines.iter().map(|b| b.close).collect();
            pct = price_change_pct(&closes, 5);
        }
        let side_s = if pct > 0.0 { "long" } else { "short" };
        let move_pct = pct.abs();

        if move_pct < cfg.price_change_pct_min {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: format!("Move {move_pct:.2}% below pump minimum {:.1}%", cfg.price_change_pct_min),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s.into()),
            };
        }
        if move_pct > cfg.price_change_pct_max {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: "Move too extended for pump entry".into(),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s.into()),
            };
        }

        let volumes: Vec<f64> = state.klines.iter().map(|b| b.volume).collect();
        let current_vol = *volumes.last().unwrap_or(&0.0);
        let baseline = if volumes.len() > 1 {
            ewma(&volumes[..volumes.len() - 1], cfg.ewma_span as usize)
        } else {
            current_vol
        };
        let hist = if volumes.len() > 1 {
            &volumes[..volumes.len() - 1]
        } else {
            volumes.as_slice()
        };
        let vol_z = zscore(current_vol, hist);
        let surge = volume_surge_ratio(current_vol, baseline);

        if surge < cfg.volume_surge_multiplier && vol_z < cfg.volume_zscore_threshold {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: format!("No volume burst (surge {surge:.1}x, z={vol_z:.1})"),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s.into()),
            };
        }

        if let Some(rank) = universe_rank {
            if rank > cfg.universe_rank_max {
                return ScanDiagnosis {
                    action: "rejected".into(),
                    message: format!("Universe rank #{rank} outside top {}", cfg.universe_rank_max),
                    composite_score: None,
                    confluence_count: None,
                    side: Some(side_s.into()),
                };
            }
        }

        if self.try_setup(state, universe_rank, funding_rate, false).is_some() {
            return ScanDiagnosis {
                action: "signal".into(),
                message: "Volume pump setup ready".into(),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s.into()),
            };
        }

        ScanDiagnosis {
            action: "rejected".into(),
            message: "Composite score below pump threshold".into(),
            composite_score: None,
            confluence_count: None,
            side: Some(side_s.into()),
        }
    }
}

/// 5-bar amount sum / prior 20-bar baseline.
pub fn turnover_velocity(klines: &[crate::exchange::KlineBar]) -> f64 {
    if klines.len() < 25 {
        return 1.0;
    }
    let n = klines.len();
    let recent: f64 = klines[n - 5..n].iter().map(|b| b.amount.max(b.volume * b.close)).sum();
    let prior: f64 = klines[n - 25..n - 5]
        .iter()
        .map(|b| b.amount.max(b.volume * b.close))
        .sum();
    if prior <= 0.0 {
        return 1.0;
    }
    (recent / 5.0) / (prior / 20.0)
}

fn projected_levels(price: f64, side: Side, sl_pct: f64, cfg: &PumpConfig) -> (f64, Vec<f64>) {
    let sl = match side {
        Side::Long => price * (1.0 - sl_pct),
        Side::Short => price * (1.0 + sl_pct),
    };
    let tps: Vec<f64> = cfg
        .tp_levels_pct
        .iter()
        .map(|tp| match side {
            Side::Long => price * (1.0 + tp),
            Side::Short => price * (1.0 - tp),
        })
        .collect();
    (sl, tps)
}

fn pump_sizing(composite: f64, strength: SignalStrength, cfg: &PumpConfig) -> (u32, f64, String) {
    let (tier, leverage) = if composite >= 80.0 || strength == SignalStrength::Strong {
        ("strong", cfg.strong_leverage)
    } else if composite >= 70.0 || strength == SignalStrength::Moderate {
        ("moderate", cfg.moderate_leverage)
    } else {
        ("base", cfg.base_leverage)
    };
    let risk_pct = cfg.max_risk_per_trade * 100.0;
    (leverage, risk_pct, tier.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AppConfig;
    use crate::exchange::KlineBar;
    use crate::exchange::TickerSnapshot;

    fn bar(vol: f64, close: f64, amount: f64) -> KlineBar {
        KlineBar {
            symbol: "TEST_USDT".into(),
            open: close,
            high: close * 1.002,
            low: close * 0.998,
            close,
            volume: vol,
            amount,
            timestamp: 0,
        }
    }

    fn test_config() -> SharedAppConfig {
        let cfg_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/settings.yaml");
        std::env::set_var("MEXC_BOT_CONFIG", cfg_path.to_str().unwrap());
        Arc::new(std::sync::RwLock::new(
            AppConfig::load().expect("config"),
        ))
    }

    #[test]
    fn detects_volume_surge() {
        let cfg = test_config();
        let engine = VolumePumpEngine::new(cfg);
        let mut state = SymbolState::new("PUMP_USDT");
        let mut klines = Vec::new();
        for i in 0..25 {
            klines.push(bar(100.0, 1.0 + i as f64 * 0.001, 100.0));
        }
        klines.push(bar(800.0, 1.05, 800.0));
        state.klines = klines;
        for _ in 0..8 {
            state.prices.push(1.05);
        }
        state.last_ticker = Some(TickerSnapshot {
            symbol: "PUMP_USDT".into(),
            last_price: 1.05,
            volume24: 2_000_000.0,
            amount24: 2_000_000.0,
            rise_fall_rate: 0.05,
            fair_price: 1.05,
            high24: 0.0,
            low24: 0.0,
            timestamp: Utc::now(),
        });
        let sig = engine.evaluate(&state, Some(1), None);
        assert!(sig.is_some(), "expected volume pump signal");
        assert_eq!(sig.unwrap().strategy, "volume_pump");
    }

    #[test]
    fn rejects_low_rank() {
        let cfg = test_config();
        let engine = VolumePumpEngine::new(cfg);
        let mut state = SymbolState::new("PUMP_USDT");
        let mut klines = Vec::new();
        for _ in 0..25 {
            klines.push(bar(100.0, 1.0, 100.0));
        }
        klines.push(bar(800.0, 1.05, 800.0));
        state.klines = klines;
        for _ in 0..8 {
            state.prices.push(1.05);
        }
        state.last_ticker = Some(TickerSnapshot {
            symbol: "PUMP_USDT".into(),
            last_price: 1.05,
            volume24: 2_000_000.0,
            amount24: 2_000_000.0,
            rise_fall_rate: 0.05,
            fair_price: 1.05,
            high24: 0.0,
            low24: 0.0,
            timestamp: Utc::now(),
        });
        assert!(engine.evaluate(&state, Some(10), None).is_none());
    }

    #[test]
    fn rejects_small_price_move() {
        let cfg = test_config();
        let engine = VolumePumpEngine::new(cfg);
        let mut state = SymbolState::new("FLAT_USDT");
        let mut klines = Vec::new();
        for _ in 0..25 {
            klines.push(bar(100.0, 1.0, 100.0));
        }
        klines.push(bar(800.0, 1.001, 800.0));
        state.klines = klines;
        for _ in 0..8 {
            state.prices.push(1.001);
        }
        state.last_ticker = Some(TickerSnapshot {
            symbol: "FLAT_USDT".into(),
            last_price: 1.001,
            volume24: 2_000_000.0,
            amount24: 2_000_000.0,
            rise_fall_rate: 0.001,
            fair_price: 1.001,
            high24: 0.0,
            low24: 0.0,
            timestamp: Utc::now(),
        });
        assert!(engine.evaluate(&state, Some(1), None).is_none());
    }

    #[test]
    fn cooldown_blocks_repeat() {
        let cfg = test_config();
        let engine = VolumePumpEngine::new(cfg.clone());
        let mut state = SymbolState::new("COOL_USDT");
        state.last_pump_at = Some(Utc::now());
        assert!(engine.in_cooldown(&state));
    }

    #[test]
    fn turnover_velocity_spikes() {
        let mut klines = Vec::new();
        for _ in 0..20 {
            klines.push(bar(100.0, 1.0, 100.0));
        }
        for _ in 0..5 {
            klines.push(bar(500.0, 1.0, 500.0));
        }
        assert!(turnover_velocity(&klines) > 2.0);
    }
}
