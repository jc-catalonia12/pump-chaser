//! Confluence signal engine — port of `signals/confluence.py`.

use chrono::Utc;

use crate::config::SharedAppConfig;
use crate::risk::filters::passes_risk_filters;
use crate::signals::indicators::{
    ewma, liquidity_score, momentum_score, oi_proxy_score, price_change_pct, volume_surge_ratio,
    zscore,
};
use crate::signals::liquidity_grab::detect_liquidity_grab;
use crate::signals::state::{Side, SymbolState};
use crate::signals::zones::{
    build_zones, market_structure_supports, structure_aligned, zone_confluence_score,
};
use crate::signals::{PumpSignal, SignalStrength};

/// Outcome of analyzing a symbol without opening a trade.
#[derive(Debug, Clone)]
pub struct ScanDiagnosis {
    pub action: String,
    pub message: String,
    pub composite_score: Option<f64>,
    pub confluence_count: Option<u32>,
    pub side: Option<String>,
}

pub struct ConfluenceEngine {
    config: SharedAppConfig,
}

/// Which composite-score band qualifies for signal emission.
enum ScoreBand {
    /// Full pass — composite >= min_composite_score.
    Pass,
    /// Near miss — composite in [min - margin, min).
    NearMiss(f64),
}

impl ConfluenceEngine {
    pub fn new(config: SharedAppConfig) -> Self {
        Self { config }
    }

    pub fn in_cooldown(&self, state: &SymbolState) -> bool {
        let Some(last) = state.last_confluence_at else {
            return false;
        };
        let elapsed = Utc::now().signed_duration_since(last).num_seconds().max(0) as u64;
        elapsed < self.config.read().unwrap().confluence.alert_cooldown_sec
    }

    pub fn evaluate(
        &self,
        state: &SymbolState,
        funding_rate: Option<f64>,
        focus_mode: bool,
    ) -> Option<PumpSignal> {
        self.try_setup(state, funding_rate, focus_mode, ScoreBand::Pass)
    }

    /// Build a shadow candidate when composite score is within `margin` of the
    /// minimum threshold but did not pass confluence entry rules.
    pub fn evaluate_near_miss(
        &self,
        state: &SymbolState,
        funding_rate: Option<f64>,
        focus_mode: bool,
        margin: f64,
    ) -> Option<PumpSignal> {
        self.try_setup(state, funding_rate, focus_mode, ScoreBand::NearMiss(margin))
    }

    fn try_setup(
        &self,
        state: &SymbolState,
        funding_rate: Option<f64>,
        focus_mode: bool,
        band: ScoreBand,
    ) -> Option<PumpSignal> {
        let app = self.config.read().unwrap();
        let cfg = &app.confluence;
        if !cfg.enabled || !app.zones.enabled {
            return None;
        }
        let ticker = state.last_ticker.as_ref()?;
        if state.klines.len() < 15 || state.prices.len() < 8 {
            return None;
        }

        let mut pct = price_change_pct(&state.prices, 6);
        if pct == 0.0 {
            let closes: Vec<f64> = state.klines.iter().map(|b| b.close).collect();
            pct = price_change_pct(&closes, 6);
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
            focus_mode,
        ) {
            return None;
        }

        let min_move = cfg.min_move_pct * if focus_mode { 0.85 } else { 1.0 };
        if move_pct < min_move || move_pct > cfg.max_move_pct {
            return None;
        }

        let volumes = &state.volumes;
        let current_vol = *volumes.last().unwrap_or(&0.0);
        let baseline = if volumes.len() > 1 {
            ewma(&volumes[..volumes.len() - 1], 20)
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

        let turnover = if ticker.amount24 > 0.0 {
            ticker.amount24
        } else {
            ticker.volume24 * ticker.last_price
        };
        let liq = liquidity_score(turnover, app.scanner.min_24h_turnover_usdt, 50_000_000.0);
        let mom = momentum_score(move_pct, cfg.max_move_pct);
        let _oi = oi_proxy_score(ticker.amount24, ticker.volume24, ticker.last_price);

        let sd_zones = build_zones(&state.klines, &app.zones);
        let (zone_score, zone_msg) =
            zone_confluence_score(ticker.last_price, side, &sd_zones, app.zones.proximity_pct);
        if zone_score < cfg.min_zone_score {
            return None;
        }
        if cfg.require_inside_zone && !zone_msg.starts_with("At ") {
            return None;
        }

        let structure_ok = structure_aligned(&state.klines, side);
        if cfg.require_structure && !structure_ok {
            return None;
        }

        let bias_ok = market_structure_supports(
            &state.klines,
            side,
            cfg.market_structure_lookback_bars as usize,
        );
        if cfg.require_market_structure_bias && !bias_ok {
            return None;
        }

        // Higher-timeframe structural bias gate (15m/30m).
        // If HTF bars are available and htf_enabled, the HTF trend must align
        // with the trade direction; misaligned setups are blocked regardless of
        // what the 1m chart shows.
        if cfg.htf_enabled && !state.htf_klines.is_empty() {
            let htf_bias = market_structure_supports(
                &state.htf_klines,
                side,
                state.htf_klines.len(),
            );
            if !htf_bias {
                return None;
            }
        }

        // 15m liquidity grab: sweep of pooled liquidity + reclaim in trade direction.
        let mut liq_grab_ok = false;
        let mut liq_grab_msg = String::new();
        let mut liq_grab_score = 0.0;
        if cfg.liquidity_grab_enabled {
            if state.htf_klines.is_empty() {
                if cfg.require_liquidity_grab {
                    return None;
                }
            } else {
                let grab = detect_liquidity_grab(
                    &state.htf_klines,
                    side,
                    cfg.liquidity_grab_lookback_bars as usize,
                    cfg.liquidity_grab_max_age_bars as usize,
                    cfg.liquidity_grab_sweep_pct,
                    cfg.liquidity_grab_min_rejection,
                );
                liq_grab_ok = grab.detected;
                liq_grab_msg = grab.message;
                liq_grab_score = grab.score;
                if cfg.require_liquidity_grab && !liq_grab_ok {
                    return None;
                }
            }
        }

        let mut confluences = Vec::new();
        if surge >= cfg.volume_surge_multiplier || vol_z >= cfg.volume_zscore_threshold {
            confluences.push("volume".into());
        }
        if mom >= 40.0 {
            confluences.push("momentum".into());
        }
        if zone_score >= cfg.min_zone_score {
            confluences.push("zone".into());
        }
        if structure_ok {
            confluences.push("structure".into());
        }
        if bias_ok {
            confluences.push("market_bias".into());
        }
        if liq >= 25.0 {
            confluences.push("liquidity".into());
        }
        if liq_grab_ok {
            confluences.push("liq_grab".into());
        }

        if confluences.len() < cfg.min_confluences as usize {
            return None;
        }

        let vol_component =
            (surge / cfg.volume_surge_multiplier.max(0.1) * 40.0 + vol_z.min(4.0) * 10.0).min(100.0);
        let mut composite = vol_component * 0.22
            + mom * 0.22
            + zone_score * 0.30
            + liq * 0.14
            + if structure_ok { 20.0 } else { 0.0 } * 0.12;
        composite += (confluences.len() as f64 * 2.0).min(8.0);
        if liq_grab_ok {
            composite = (composite + liq_grab_score * 0.12).min(100.0);
        }

        let min_score = cfg.min_composite_score - if focus_mode { 2.0 } else { 0.0 };
        let score_ok = match band {
            ScoreBand::Pass => composite >= min_score,
            ScoreBand::NearMiss(margin) => composite >= min_score - margin && composite < min_score,
        };
        if !score_ok {
            return None;
        }

        let (strength, tier, leverage, risk_pct) = sizing_tier(composite, confluences.len(), cfg);
        let direction = if pct > 0.0 { "long" } else { "short" };
        let sl_pct = cfg.default_sl_pct;
        let (projected_sl, projected_tps) = projected_levels(ticker.last_price, side, sl_pct, cfg);

        let setup_prob = estimate_setup_probability(
            composite,
            confluences.len(),
            strength,
            zone_score,
            funding_is_favorable(direction, funding_rate),
        );

        let funding_ok = funding_is_favorable(direction, funding_rate);
        let details = confluence_details(
            vol_component,
            pct,
            surge,
            vol_z,
            mom,
            zone_score,
            &zone_msg,
            structure_ok,
            bias_ok,
            liq,
            liq_grab_ok,
            &liq_grab_msg,
            liq_grab_score,
            funding_rate,
            funding_ok,
            cfg.ema_trend_span,
        );
        let conf_label = confluences.join(", ");
        let n_conf = confluences.len();
        let message = format!(
            "Confluence {direction} [{tier}]: {conf_label} | {zone_msg} ({n_conf} factors, ~{setup_prob:.0}% est., risk {risk_pct:.2}%, {leverage}x)"
        );

        Some(PumpSignal {
            symbol: state.symbol.clone(),
            strategy: "confluence".into(),
            composite_score: composite,
            strength,
            last_price: ticker.last_price,
            price_change_pct: pct,
            volume_surge_ratio: surge,
            confluence_count: n_conf as u32,
            confluences,
            confluence_details: details,
            setup_probability_pct: setup_prob,
            suggested_risk_pct: risk_pct,
            suggested_leverage: leverage,
            zone_score,
            zone_message: zone_msg,
            sizing_tier: tier,
            message,
            generated_at: Utc::now(),
            signal_id: None,
            projected_stop_loss: projected_sl,
            projected_take_profits: projected_tps,
            tp_close_fractions: cfg.tp_close_fractions.clone(),
            ml_features: Vec::new(),
            entry_mode: "market".to_string(),
            limit_entry_price: 0.0,
        })
    }

    /// Explain why a symbol did not produce a signal (for live scan feed).
    pub fn diagnose(
        &self,
        state: &SymbolState,
        funding_rate: Option<f64>,
        focus_mode: bool,
    ) -> ScanDiagnosis {
        let app = self.config.read().unwrap();
        let cfg = &app.confluence;
        if !cfg.enabled || !app.zones.enabled {
            return ScanDiagnosis {
                action: "skipped".into(),
                message: "Confluence engine disabled".into(),
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
        if state.klines.len() < 15 || state.prices.len() < 8 {
            return ScanDiagnosis {
                action: "warming".into(),
                message: format!(
                    "Collecting data — klines {}/15, ticks {}/8",
                    state.klines.len(),
                    state.prices.len()
                ),
                composite_score: None,
                confluence_count: None,
                side: None,
            };
        }

        let mut pct = price_change_pct(&state.prices, 6);
        if pct == 0.0 {
            let closes: Vec<f64> = state.klines.iter().map(|b| b.close).collect();
            pct = price_change_pct(&closes, 6);
        }
        let move_pct = pct.abs();
        let side = if pct > 0.0 { Side::Long } else { Side::Short };
        let side_s = if side == Side::Long { "long" } else { "short" }.to_string();

        if !passes_risk_filters(
            &state.symbol,
            ticker,
            &state.klines,
            &app.risk,
            &app.scanner,
            funding_rate,
            side,
            focus_mode,
        ) {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: "Failed risk filters (liquidity, funding, or exposure)".into(),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s),
            };
        }

        let min_move = cfg.min_move_pct * if focus_mode { 0.85 } else { 1.0 };
        if move_pct < min_move {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: format!("Move too small ({move_pct:.2}% < {min_move:.2}%)"),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s),
            };
        }
        if move_pct > cfg.max_move_pct {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: format!("Move too large ({move_pct:.2}% > {:.2}%)", cfg.max_move_pct),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s),
            };
        }

        let volumes = &state.volumes;
        let current_vol = *volumes.last().unwrap_or(&0.0);
        let baseline = if volumes.len() > 1 {
            ewma(&volumes[..volumes.len() - 1], 20)
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

        let turnover = if ticker.amount24 > 0.0 {
            ticker.amount24
        } else {
            ticker.volume24 * ticker.last_price
        };
        let liq = liquidity_score(turnover, app.scanner.min_24h_turnover_usdt, 50_000_000.0);
        let mom = momentum_score(move_pct, cfg.max_move_pct);

        let sd_zones = build_zones(&state.klines, &app.zones);
        let (zone_score, zone_msg) =
            zone_confluence_score(ticker.last_price, side, &sd_zones, app.zones.proximity_pct);
        if zone_score < cfg.min_zone_score {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: format!("Zone score too low ({zone_score:.1} < {:.1})", cfg.min_zone_score),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s),
            };
        }

        let structure_ok = structure_aligned(&state.klines, side);
        if cfg.require_structure && !structure_ok {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: "Structure not aligned".into(),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s),
            };
        }

        let bias_ok = market_structure_supports(
            &state.klines,
            side,
            cfg.market_structure_lookback_bars as usize,
        );
        if cfg.require_market_structure_bias && !bias_ok {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: "Market structure bias not aligned".into(),
                composite_score: None,
                confluence_count: None,
                side: Some(side_s),
            };
        }

        if cfg.htf_enabled && !state.htf_klines.is_empty() {
            let htf_bias =
                market_structure_supports(&state.htf_klines, side, state.htf_klines.len());
            if !htf_bias {
                return ScanDiagnosis {
                    action: "rejected".into(),
                    message: format!(
                        "HTF ({}) structure opposes {} direction",
                        cfg.htf_interval, side_s
                    ),
                    composite_score: None,
                    confluence_count: None,
                    side: Some(side_s),
                };
            }
        }

        if cfg.liquidity_grab_enabled && cfg.require_liquidity_grab {
            if state.htf_klines.is_empty() {
                return ScanDiagnosis {
                    action: "rejected".into(),
                    message: "Waiting for 15m HTF bars (liquidity grab)".into(),
                    composite_score: None,
                    confluence_count: None,
                    side: Some(side_s),
                };
            }
            let grab = detect_liquidity_grab(
                &state.htf_klines,
                side,
                cfg.liquidity_grab_lookback_bars as usize,
                cfg.liquidity_grab_max_age_bars as usize,
                cfg.liquidity_grab_sweep_pct,
                cfg.liquidity_grab_min_rejection,
            );
            if !grab.detected {
                return ScanDiagnosis {
                    action: "rejected".into(),
                    message: grab.message,
                    composite_score: None,
                    confluence_count: None,
                    side: Some(side_s),
                };
            }
        }

        let mut confluences = Vec::new();
        if surge >= cfg.volume_surge_multiplier || vol_z >= cfg.volume_zscore_threshold {
            confluences.push("volume");
        }
        if mom >= 40.0 {
            confluences.push("momentum");
        }
        if zone_score >= cfg.min_zone_score {
            confluences.push("zone");
        }
        if structure_ok {
            confluences.push("structure");
        }
        if bias_ok {
            confluences.push("market_bias");
        }
        if liq >= 25.0 {
            confluences.push("liquidity");
        }

        let n_conf = confluences.len();
        if n_conf < cfg.min_confluences as usize {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: format!(
                    "Not enough confluences ({n_conf}/{} required)",
                    cfg.min_confluences
                ),
                composite_score: None,
                confluence_count: Some(n_conf as u32),
                side: Some(side_s),
            };
        }

        let vol_component =
            (surge / cfg.volume_surge_multiplier.max(0.1) * 40.0 + vol_z.min(4.0) * 10.0).min(100.0);
        let mut composite = vol_component * 0.22
            + mom * 0.22
            + zone_score * 0.30
            + liq * 0.14
            + if structure_ok { 20.0 } else { 0.0 } * 0.12;
        composite += (n_conf as f64 * 2.0).min(8.0);

        let min_score = cfg.min_composite_score - if focus_mode { 2.0 } else { 0.0 };
        if composite < min_score {
            return ScanDiagnosis {
                action: "rejected".into(),
                message: format!("Score below threshold ({composite:.1} < {min_score:.1})"),
                composite_score: Some(composite),
                confluence_count: Some(n_conf as u32),
                side: Some(side_s),
            };
        }

        ScanDiagnosis {
            action: "rejected".into(),
            message: format!("Setup passed filters but blocked — {zone_msg}"),
            composite_score: Some(composite),
            confluence_count: Some(n_conf as u32),
            side: Some(side_s),
        }
    }
}

fn sizing_tier(
    composite: f64,
    n_conf: usize,
    cfg: &crate::config::ConfluenceConfig,
) -> (SignalStrength, String, u32, f64) {
    if composite >= 80.0 && n_conf >= 5 {
        (
            SignalStrength::Strong,
            "strong".into(),
            cfg.strong_leverage,
            cfg.max_risk_per_trade * 100.0,
        )
    } else if composite >= 74.0 && n_conf >= 4 {
        (
            SignalStrength::Moderate,
            "moderate".into(),
            cfg.moderate_leverage,
            cfg.max_risk_per_trade * 100.0 * 0.95,
        )
    } else {
        (
            SignalStrength::Weak,
            "base".into(),
            cfg.base_leverage,
            cfg.max_risk_per_trade * 100.0 * 0.85,
        )
    }
}

fn projected_levels(
    price: f64,
    side: Side,
    sl_pct: f64,
    cfg: &crate::config::ConfluenceConfig,
) -> (f64, Vec<f64>) {
    match side {
        Side::Long => (
            price * (1.0 - sl_pct),
            cfg.tp_levels_pct.iter().map(|p| price * (1.0 + p)).collect(),
        ),
        Side::Short => (
            price * (1.0 + sl_pct),
            cfg.tp_levels_pct.iter().map(|p| price * (1.0 - p)).collect(),
        ),
    }
}

fn estimate_setup_probability(
    composite: f64,
    confluence_count: usize,
    strength: SignalStrength,
    zone_score: f64,
    funding_favorable: bool,
) -> f64 {
    let mut prob = composite * 0.52;
    prob += (confluence_count.saturating_sub(3) as f64) * 4.5;
    prob += match strength {
        SignalStrength::Weak => 0.0,
        SignalStrength::Moderate => 7.0,
        SignalStrength::Strong => 14.0,
    };
    prob += (zone_score * 0.12).min(12.0);
    if !funding_favorable {
        prob -= 10.0;
    }
    prob.clamp(20.0, 85.0).round()
}

fn funding_is_favorable(direction: &str, funding_rate: Option<f64>) -> bool {
    let Some(rate) = funding_rate else {
        return true;
    };
    if direction == "long" {
        rate <= 0.0005
    } else {
        rate >= -0.0005
    }
}

fn confluence_details(
    vol_component: f64,
    pct: f64,
    surge: f64,
    vol_z: f64,
    mom: f64,
    zone_score: f64,
    zone_msg: &str,
    structure_ok: bool,
    bias_ok: bool,
    liq: f64,
    liq_grab_ok: bool,
    liq_grab_msg: &str,
    liq_grab_score: f64,
    funding_rate: Option<f64>,
    funding_ok: bool,
    ema_span: u32,
) -> Vec<serde_json::Value> {
    use serde_json::json;
    vec![
        json!({"key":"volume","label":"Volume","active":true,"score":vol_component.round(),"detail":format!("surge {surge:.1}x · z={vol_z:.1}")}),
        json!({"key":"momentum","label":"Momentum","active":mom>=40.0,"score":mom.round(),"detail":format!("{pct:+.2}% move")}),
        json!({"key":"zone","label":"Supply / Demand","active":zone_score>=55.0,"score":zone_score.round(),"detail":zone_msg}),
        json!({"key":"structure","label":"Structure","active":structure_ok,"score":if structure_ok {100.0}else{0.0},"detail":"Structure aligned"}),
        json!({"key":"market_bias","label":"Market bias","active":bias_ok,"score":if bias_ok {100.0}else{0.0},"detail":"Market structure bias"}),
        json!({"key":"liquidity","label":"Liquidity","active":liq>=25.0,"score":liq.round(),"detail":format!("24h turnover score {liq:.0}")}),
        json!({"key":"liq_grab","label":"15m Liq Grab","active":liq_grab_ok,"score":if liq_grab_ok {liq_grab_score.round()} else {0.0},"detail":if liq_grab_ok {liq_grab_msg} else {"No HTF grab detected"}}),
        json!({"key":"ema_trend","label":"EMA trend","active":true,"score":100.0,"detail":format!("{ema_span}-bar trend")}),
        json!({"key":"funding","label":"Funding","active":funding_ok,"score":if funding_ok {100.0}else{30.0},"detail":funding_rate.map(|r|format!("{:.4}%/period", r*100.0)).unwrap_or_else(||"not checked".into())}),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AppConfig;
    use crate::exchange::{KlineBar, TickerSnapshot};

    fn sample_state() -> SymbolState {
        let mut state = SymbolState::new("BTC_USDT");
        state.last_ticker = Some(TickerSnapshot {
            symbol: "BTC_USDT".into(),
            last_price: 100.0,
            volume24: 1e9,
            amount24: 1e9,
            rise_fall_rate: 0.5,
            fair_price: 100.0,
            high24: 101.0,
            low24: 99.0,
            timestamp: Utc::now(),
        });
        for i in 0..20 {
            state.prices.push(100.0 + i as f64 * 0.05);
            state.volumes.push(1000.0 + i as f64 * 50.0);
        }
        let mut klines = Vec::new();
        for i in 0..30 {
            let base = 100.0 + i as f64 * 0.1;
            klines.push(KlineBar {
                symbol: "BTC_USDT".into(),
                open: base,
                high: base + 0.2,
                low: base - 0.1,
                close: base + 0.05,
                volume: 1000.0,
                amount: 1000.0,
                timestamp: i,
            });
        }
        state.klines = klines;
        state
    }

    #[test]
    fn evaluate_returns_signal_with_relaxed_config() {
        let cfg_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/settings.yaml");
        std::env::set_var("MEXC_BOT_CONFIG", cfg_path.to_str().unwrap());
        let mut cfg = AppConfig::load().expect("config");
        cfg.confluence.require_structure = false;
        cfg.confluence.require_market_structure_bias = false;
        cfg.confluence.min_composite_score = 30.0;
        cfg.confluence.min_zone_score = 0.0;
        cfg.confluence.min_confluences = 1;
        cfg.confluence.min_move_pct = 0.01;
        cfg.confluence.liquidity_grab_enabled = false;
        cfg.confluence.require_liquidity_grab = false;
        cfg.confluence.htf_enabled = false;
        cfg.confluence.require_inside_zone = false;
        cfg.zones.proximity_pct = 50.0;
        let engine = ConfluenceEngine::new(Arc::new(std::sync::RwLock::new(cfg)));
        let state = sample_state();
        let sig = engine.evaluate(&state, Some(0.0001), false);
        assert!(sig.is_some(), "expected confluence signal from sample state");
    }
}
