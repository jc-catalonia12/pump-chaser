//! Volume-pump strategy — abnormal 1m volume + universe rank, confirmation gates.

use chrono::Utc;
use serde_json::json;

use crate::config::{EntryMode, PumpConfig, SharedAppConfig};
use crate::exchange::KlineBar;
use crate::risk::filters::passes_risk_filters;
use crate::signals::confluence::ScanDiagnosis;
use crate::signals::indicators::{
    ewma, liquidity_score, momentum_score, oi_proxy_score, price_change_pct, volume_surge_ratio,
    zscore,
};
use crate::signals::state::{PendingPumpSetup, Side, SymbolState};
use crate::signals::zones::{
    breakout_confirmed, market_shift_confirmed, market_structure_supports, structure_aligned,
};
use crate::signals::{PumpSignal, SignalStrength};

/// BTC + ETH higher-timeframe bars for macro gate.
#[derive(Debug, Clone, Default)]
pub struct MacroHtfState {
    pub btc_klines: Vec<KlineBar>,
    pub eth_klines: Vec<KlineBar>,
}

#[derive(Debug)]
pub enum PumpConfirmResult {
    Fire(PumpSignal),
    Waiting,
    Expired,
}

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

    /// Main entry: respects two-phase confirmation when enabled.
    pub fn evaluate(
        &self,
        state: &SymbolState,
        universe_rank: Option<u32>,
        funding_rate: Option<f64>,
        macro_htf: &MacroHtfState,
    ) -> Option<PumpSignal> {
        let cfg = self.config.read().unwrap();
        if !cfg.pump.confirmation_enabled {
            return self.build_signal(state, universe_rank, funding_rate, macro_htf, None);
        }
        None
    }

    /// Phase 1: volume surge arms a pending setup (does not emit).
    pub fn try_arm(
        &self,
        state: &SymbolState,
        universe_rank: Option<u32>,
        funding_rate: Option<f64>,
    ) -> Option<PendingPumpSetup> {
        let metrics = self.compute_arm_metrics(state, universe_rank, funding_rate)?;
        Some(PendingPumpSetup {
            side: metrics.side,
            armed_at: Utc::now(),
            composite_score: metrics.composite,
            price_change_pct: metrics.pct,
            volume_surge_ratio: metrics.surge,
            vol_z: metrics.vol_z,
            universe_rank,
        })
    }

    /// Phase 2: confirmation gates while armed.
    pub fn check_confirmation(
        &self,
        state: &SymbolState,
        pending: &PendingPumpSetup,
        funding_rate: Option<f64>,
        macro_htf: &MacroHtfState,
    ) -> PumpConfirmResult {
        let cfg = self.config.read().unwrap().pump.clone();
        let ttl = cfg.confirmation_ttl_sec;
        let elapsed = Utc::now()
            .signed_duration_since(pending.armed_at)
            .num_seconds()
            .max(0) as u64;
        if elapsed > ttl {
            return PumpConfirmResult::Expired;
        }

        let gates = confirmation_gates(state, pending.side, &cfg, macro_htf);
        if !gates.all_pass {
            return PumpConfirmResult::Waiting;
        }

        match self.build_signal(
            state,
            pending.universe_rank,
            funding_rate,
            macro_htf,
            Some(pending),
        ) {
            Some(sig) => PumpConfirmResult::Fire(sig),
            None => PumpConfirmResult::Waiting,
        }
    }

    fn build_signal(
        &self,
        state: &SymbolState,
        universe_rank: Option<u32>,
        funding_rate: Option<f64>,
        macro_htf: &MacroHtfState,
        pending: Option<&PendingPumpSetup>,
    ) -> Option<PumpSignal> {
        let metrics = if let Some(p) = pending {
            ArmMetrics {
                side: p.side,
                pct: p.price_change_pct,
                surge: p.volume_surge_ratio,
                vol_z: p.vol_z,
                composite: p.composite_score,
                turnover: state.last_ticker.as_ref().map(|t| {
                    if t.amount24 > 0.0 {
                        t.amount24
                    } else {
                        t.volume24 * t.last_price
                    }
                })?,
                turnover_accel: turnover_velocity(&state.klines),
                rank_bonus: universe_rank
                    .map(|r| {
                        let cfg = self.config.read().unwrap();
                        ((cfg.pump.universe_rank_max + 1 - r.min(cfg.pump.universe_rank_max)) as f64)
                            * 12.0
                    })
                    .unwrap_or(50.0),
            }
        } else {
            self.compute_arm_metrics(state, universe_rank, funding_rate)?
        };

        let app = self.config.read().unwrap();
        let cfg = &app.pump;

        if cfg.confirmation_enabled && pending.is_none() {
            let gates = confirmation_gates(state, metrics.side, cfg, macro_htf);
            if !gates.all_pass {
                return None;
            }
        }

        let ticker = state.last_ticker.as_ref()?;
        let move_pct = metrics.pct.abs();
        let vol_component = (metrics.surge / cfg.volume_surge_multiplier.max(0.1) * 50.0
            + metrics.vol_z.min(5.0) * 10.0)
            .min(100.0);
        let _mom = momentum_score(move_pct, cfg.price_change_pct_max);
        let liq = liquidity_score(
            metrics.turnover,
            cfg.min_24h_turnover_usdt,
            cfg.max_24h_turnover_usdt,
        );
        let accel_score = (metrics.turnover_accel * 40.0).clamp(0.0, 100.0);
        let gates = confirmation_gates(state, metrics.side, cfg, macro_htf);

        let strength = if metrics.composite >= 80.0 {
            SignalStrength::Strong
        } else if metrics.composite >= 70.0 {
            SignalStrength::Moderate
        } else {
            SignalStrength::Weak
        };

        let (leverage, risk_pct, tier) = pump_sizing(metrics.composite, strength, cfg);
        let sl_pct = cfg.default_sl_pct;
        let (projected_sl, projected_tps) =
            projected_levels(ticker.last_price, metrics.side, sl_pct, cfg);

        let direction = if metrics.pct > 0.0 { "long" } else { "short" };
        let rank_msg = universe_rank
            .map(|r| format!("universe rank #{r}"))
            .unwrap_or_else(|| "universe rank n/a".into());
        let confirm_tag = if cfg.confirmation_enabled {
            " confirmed"
        } else {
            ""
        };
        let message = format!(
            "Volume pump {direction}{confirm_tag} [{tier}]: surge {surge:.1}x z={vol_z:.1} | {rank_msg} (~{composite:.0} score, {leverage}x)",
            surge = metrics.surge,
            vol_z = metrics.vol_z,
            composite = metrics.composite,
        );

        let entry_mode = match cfg.entry_mode {
            EntryMode::Market => "market",
            EntryMode::Limit => "limit",
            EntryMode::Sniper => "limit",
        };

        let mut confluences = vec!["volume".into(), "momentum".into()];
        if gates.breakout {
            confluences.push("breakout".into());
        }
        if gates.market_shift {
            confluences.push("market_shift".into());
        }
        if gates.htf_bias {
            confluences.push("htf_bias".into());
        }
        if gates.macro_ok {
            confluences.push("macro_ok".into());
        }

        Some(PumpSignal {
            symbol: state.symbol.clone(),
            strategy: "volume_pump".into(),
            composite_score: metrics.composite,
            strength,
            last_price: ticker.last_price,
            price_change_pct: metrics.pct,
            volume_surge_ratio: metrics.surge,
            confluence_count: confluences.len() as u32,
            confluences,
            confluence_details: vec![
                json!({"key":"vol_surge","label":"Volume surge","active":true,"score":vol_component.round(),"detail":format!("{:.2}x", metrics.surge)}),
                json!({"key":"vol_z","label":"Volume z-score","active":true,"score":(metrics.vol_z*20.0).min(100.0).round(),"detail":format!("z={:.2}", metrics.vol_z)}),
                json!({"key":"turnover_accel","label":"Turnover accel","active":metrics.turnover_accel>1.0,"score":accel_score.round(),"detail":format!("{:.2}x", metrics.turnover_accel)}),
                json!({"key":"universe_rank","label":"Universe rank","active":universe_rank.is_some_and(|r| r<=cfg.universe_rank_max),"score":metrics.rank_bonus.round(),"detail":rank_msg}),
                json!({"key":"liquidity","label":"Liquidity","active":liq>0.0,"score":liq.round(),"detail":format!("turnover {:.0}", metrics.turnover)}),
                json!({"key":"breakout","label":"Breakout","active":gates.breakout,"score":if gates.breakout {100.0}else{0.0},"detail":if gates.breakout {"Range break"} else {"No break"}}),
                json!({"key":"market_shift","label":"Market shift","active":gates.market_shift,"score":if gates.market_shift {100.0}else{0.0},"detail":"Structure + bias"}),
                json!({"key":"structure","label":"Structure","active":gates.structure,"score":if gates.structure {100.0}else{0.0},"detail":"1m alignment"}),
                json!({"key":"htf_bias","label":"HTF bias","active":gates.htf_bias,"score":if gates.htf_bias {100.0}else{0.0},"detail":"Symbol HTF"}),
                json!({"key":"btc_macro","label":"BTC macro","active":gates.btc_ok,"score":if gates.btc_ok {100.0}else{0.0},"detail":gates.btc_detail}),
                json!({"key":"eth_macro","label":"ETH macro","active":gates.eth_ok,"score":if gates.eth_ok {100.0}else{0.0},"detail":gates.eth_detail}),
            ],
            setup_probability_pct: metrics.composite * 0.85,
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

    fn compute_arm_metrics(
        &self,
        state: &SymbolState,
        universe_rank: Option<u32>,
        funding_rate: Option<f64>,
    ) -> Option<ArmMetrics> {
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
        let rank_bonus = universe_rank
            .map(|r| ((cfg.universe_rank_max + 1 - r.min(cfg.universe_rank_max)) as f64) * 12.0)
            .unwrap_or(50.0);
        let accel_score = (turnover_accel * 40.0).clamp(0.0, 100.0);

        let mut composite =
            vol_component * 0.40 + mom * 0.30 + accel_score * 0.15 + rank_bonus * 0.15;
        composite = composite.min(100.0);
        let _ = oi_proxy_score(ticker.amount24, ticker.volume24, ticker.last_price);

        if composite < cfg.min_composite_score {
            return None;
        }

        Some(ArmMetrics {
            side,
            pct,
            surge,
            vol_z,
            composite,
            turnover,
            turnover_accel,
            rank_bonus,
        })
    }

    pub fn diagnose(
        &self,
        state: &SymbolState,
        universe_rank: Option<u32>,
        funding_rate: Option<f64>,
        macro_htf: &MacroHtfState,
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

        if let Some(pending) = &state.pending_pump {
            let side_s = if pending.side == Side::Long {
                "long"
            } else {
                "short"
            };
            let gates = confirmation_gates(state, pending.side, cfg, macro_htf);
            if gates.all_pass {
                return ScanDiagnosis {
                    action: "signal".into(),
                    message: "[pump] armed — confirmation ready".into(),
                    composite_score: Some(pending.composite_score),
                    confluence_count: None,
                    side: Some(side_s.into()),
                };
            }
            let block = gates.first_block_reason();
            return ScanDiagnosis {
                action: "warming".into(),
                message: format!("[pump] armed — waiting {block}"),
                composite_score: Some(pending.composite_score),
                confluence_count: None,
                side: Some(side_s.into()),
            };
        }

        let metrics = match self.compute_arm_metrics(state, universe_rank, funding_rate) {
            Some(m) => m,
            None => {
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
                return ScanDiagnosis {
                    action: "rejected".into(),
                    message: "Volume / score gates not met".into(),
                    composite_score: None,
                    confluence_count: None,
                    side: None,
                };
            }
        };

        let side_s = if metrics.pct > 0.0 { "long" } else { "short" };

        if cfg.confirmation_enabled {
            return ScanDiagnosis {
                action: "warming".into(),
                message: "[pump] volume surge — would arm for confirmation".into(),
                composite_score: Some(metrics.composite),
                confluence_count: None,
                side: Some(side_s.into()),
            };
        }

        if self
            .build_signal(state, universe_rank, funding_rate, macro_htf, None)
            .is_some()
        {
            return ScanDiagnosis {
                action: "signal".into(),
                message: "Volume pump setup ready".into(),
                composite_score: Some(metrics.composite),
                confluence_count: None,
                side: Some(side_s.into()),
            };
        }

        ScanDiagnosis {
            action: "rejected".into(),
            message: "Confirmation gates blocked".into(),
            composite_score: Some(metrics.composite),
            confluence_count: None,
            side: Some(side_s.into()),
        }
    }
}

struct ArmMetrics {
    side: Side,
    pct: f64,
    surge: f64,
    vol_z: f64,
    composite: f64,
    turnover: f64,
    turnover_accel: f64,
    rank_bonus: f64,
}

struct ConfirmationGates {
    breakout: bool,
    market_shift: bool,
    structure: bool,
    bias_1m: bool,
    htf_bias: bool,
    macro_ok: bool,
    btc_ok: bool,
    eth_ok: bool,
    btc_detail: String,
    eth_detail: String,
    all_pass: bool,
}

impl ConfirmationGates {
    fn first_block_reason(&self) -> &'static str {
        if !self.breakout && !self.market_shift {
            return "breakout or market shift";
        }
        if !self.structure {
            return "structure";
        }
        if !self.bias_1m {
            return "1m bias";
        }
        if !self.htf_bias {
            return "HTF bias";
        }
        if !self.macro_ok {
            return "BTC/ETH macro";
        }
        "confirmation"
    }
}

fn confirmation_gates(
    state: &SymbolState,
    side: Side,
    cfg: &PumpConfig,
    macro_htf: &MacroHtfState,
) -> ConfirmationGates {
    let lookback = cfg.breakout_lookback_bars as usize;
    let ms_lookback = cfg.market_structure_lookback_bars as usize;

    let breakout = breakout_confirmed(
        &state.klines,
        side,
        lookback,
        cfg.breakout_min_pct,
        cfg.breakout_vol_mult,
        cfg.ewma_span as usize,
    );
    let market_shift = market_shift_confirmed(&state.klines, side, ms_lookback);
    let structure = !cfg.require_structure || structure_aligned(&state.klines, side);
    let bias_1m =
        !cfg.require_market_structure_bias || market_structure_supports(&state.klines, side, ms_lookback);

    let htf_bias = if cfg.htf_enabled && !state.htf_klines.is_empty() {
        market_structure_supports(&state.htf_klines, side, state.htf_klines.len())
    } else if cfg.htf_enabled {
        false
    } else {
        true
    };

    let btc_eval = macro_asset_allows(side, &macro_htf.btc_klines, cfg, "BTC");
    let eth_eval = macro_asset_allows(side, &macro_htf.eth_klines, cfg, "ETH");
    let macro_ok = if cfg.macro_filter_enabled {
        btc_eval.allows && eth_eval.allows
    } else {
        true
    };

    let shift_or_break = breakout || market_shift;
    let confirm_path = !cfg.require_breakout_or_shift || shift_or_break;

    let all_pass = confirm_path && structure && bias_1m && htf_bias && macro_ok;

    ConfirmationGates {
        breakout,
        market_shift,
        structure,
        bias_1m,
        htf_bias,
        macro_ok,
        btc_ok: btc_eval.allows,
        eth_ok: eth_eval.allows,
        btc_detail: btc_eval.detail,
        eth_detail: eth_eval.detail,
        all_pass,
    }
}

struct MacroAssetEval {
    allows: bool,
    detail: String,
}

fn macro_asset_allows(
    side: Side,
    klines: &[KlineBar],
    cfg: &PumpConfig,
    label: &str,
) -> MacroAssetEval {
    if klines.len() < 10 {
        return MacroAssetEval {
            allows: true,
            detail: format!("{label} HTF warming"),
        };
    }
    let lookback = cfg.macro_htf_lookback_bars as usize;
    let move_pct = htf_move_pct(klines, lookback.min(klines.len().saturating_sub(1)).max(2));
    let bearish = market_structure_supports(klines, Side::Short, lookback)
        || move_pct <= -cfg.macro_min_move_pct;
    let bullish = market_structure_supports(klines, Side::Long, lookback)
        || move_pct >= cfg.macro_min_move_pct;

    let allows = match side {
        Side::Long => !bearish,
        Side::Short => !bullish,
    };
    let detail = format!("{label} HTF {move_pct:+.2}%");
    MacroAssetEval { allows, detail }
}

pub fn macro_allows(side: Side, macro_htf: &MacroHtfState, cfg: &PumpConfig) -> bool {
    if !cfg.macro_filter_enabled {
        return true;
    }
    let btc = macro_asset_allows(side, &macro_htf.btc_klines, cfg, "BTC");
    let eth = macro_asset_allows(side, &macro_htf.eth_klines, cfg, "ETH");
    btc.allows && eth.allows
}

fn htf_move_pct(klines: &[KlineBar], bars: usize) -> f64 {
    if klines.len() < bars + 1 {
        return 0.0;
    }
    let start = klines[klines.len() - bars - 1].close;
    let end = klines.last().map(|b| b.close).unwrap_or(start);
    if start <= 0.0 {
        return 0.0;
    }
    (end - start) / start * 100.0
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

    use chrono::Duration;

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

    fn breakout_bar(vol: f64, close: f64) -> KlineBar {
        KlineBar {
            symbol: "TEST_USDT".into(),
            open: close * 0.999,
            high: close * 1.003,
            low: close * 0.997,
            close,
            volume: vol,
            amount: vol * close,
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

    fn pump_state_with_surge() -> SymbolState {
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
        state
    }

    fn bullish_htf_klines() -> Vec<KlineBar> {
        (0..30)
            .map(|i| bar(100.0, 100.0 + i as f64 * 0.5, 100.0))
            .collect()
    }

    fn bearish_htf_klines() -> Vec<KlineBar> {
        (0..30)
            .map(|i| bar(100.0, 100.0 - i as f64 * 0.5, 100.0))
            .collect()
    }

    #[test]
    fn volume_surge_arms_but_does_not_fire_without_confirmation() {
        let cfg = test_config();
        cfg.write().unwrap().pump.confirmation_enabled = true;
        let engine = VolumePumpEngine::new(cfg);
        let state = pump_state_with_surge();
        let macro_htf = MacroHtfState::default();
        assert!(engine.evaluate(&state, Some(1), None, &macro_htf).is_none());
        let armed = engine.try_arm(&state, Some(1), None);
        assert!(armed.is_some(), "volume surge should arm");
    }

    #[test]
    fn breakout_and_htf_fires_when_confirmed() {
        let cfg = test_config();
        cfg.write().unwrap().pump.confirmation_enabled = true;
        cfg.write().unwrap().pump.macro_filter_enabled = false;
        let engine = VolumePumpEngine::new(cfg);
        let mut state = pump_state_with_surge();
        state.htf_klines = bullish_htf_klines();

        let mut klines = state.klines.clone();
        for _ in 0..20 {
            klines.push(bar(100.0, 1.04, 100.0));
        }
        klines.push(breakout_bar(1200.0, 1.08));
        state.klines = klines;

        let pending = engine.try_arm(&state, Some(1), None).expect("arm");
        let macro_htf = MacroHtfState::default();
        match engine.check_confirmation(&state, &pending, None, &macro_htf) {
            PumpConfirmResult::Fire(sig) => assert_eq!(sig.strategy, "volume_pump"),
            other => panic!("expected fire, got {other:?}"),
        }
    }

    #[test]
    fn btc_bearish_blocks_long_pump() {
        let cfg = test_config();
        cfg.write().unwrap().pump.confirmation_enabled = true;
        cfg.write().unwrap().pump.htf_enabled = false;
        cfg.write().unwrap().pump.require_breakout_or_shift = false;
        cfg.write().unwrap().pump.require_structure = false;
        cfg.write().unwrap().pump.require_market_structure_bias = false;
        let engine = VolumePumpEngine::new(cfg);
        let state = pump_state_with_surge();
        let pending = PendingPumpSetup {
            side: Side::Long,
            armed_at: Utc::now(),
            composite_score: 75.0,
            price_change_pct: 5.0,
            volume_surge_ratio: 8.0,
            vol_z: 3.0,
            universe_rank: Some(1),
        };
        let macro_htf = MacroHtfState {
            btc_klines: bearish_htf_klines(),
            eth_klines: bullish_htf_klines(),
        };
        match engine.check_confirmation(&state, &pending, None, &macro_htf) {
            PumpConfirmResult::Waiting => {}
            other => panic!("BTC bearish should block long, got {other:?}"),
        }
    }

    #[test]
    fn expired_pending_returns_expired() {
        let cfg = test_config();
        let engine = VolumePumpEngine::new(cfg);
        let state = pump_state_with_surge();
        let pending = PendingPumpSetup {
            side: Side::Long,
            armed_at: Utc::now() - Duration::seconds(500),
            composite_score: 75.0,
            price_change_pct: 5.0,
            volume_surge_ratio: 8.0,
            vol_z: 3.0,
            universe_rank: Some(1),
        };
        let macro_htf = MacroHtfState::default();
        assert!(matches!(
            engine.check_confirmation(&state, &pending, None, &macro_htf),
            PumpConfirmResult::Expired
        ));
    }

    #[test]
    fn rejects_low_rank() {
        let cfg = test_config();
        let engine = VolumePumpEngine::new(cfg);
        let state = pump_state_with_surge();
        assert!(engine.try_arm(&state, Some(10), None).is_none());
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
        assert!(engine.try_arm(&state, Some(1), None).is_none());
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

    #[test]
    fn macro_allows_blocks_long_on_btc_dump() {
        let cfg = test_config();
        let pump_cfg = cfg.read().unwrap().pump.clone();
        let macro_htf = MacroHtfState {
            btc_klines: bearish_htf_klines(),
            eth_klines: bullish_htf_klines(),
        };
        assert!(!macro_allows(Side::Long, &macro_htf, &pump_cfg));
    }
}
