//! Backtest & validation engine (Phase 6).
//!
//! Replays stored resolved signals (SL/TP + outcome + `ml_features` +
//! `setup_probability_pct`) and computes strategy metrics without touching
//! MEXC. Three entry points:
//!
//!   * [`Backtester::run`]        — gate by a fixed ML probability threshold.
//!   * [`Backtester::run_decision`] — gate by the full Phase 5 decision engine
//!     (neutral/mock regime for determinism), so the backtest reflects what
//!     the live pipeline would actually trade.
//!   * [`StrategyLearner::walk_forward`] — train the online model on the first
//!     `train_frac` of history and report out-of-sample accuracy *and* PnL
//!     metrics (win rate, expectancy, profit factor, max drawdown).
//!
//! [`acceptance_gate`] turns a metrics object into a pass/fail verdict against
//! the configured paper-acceptance thresholds — the mandatory check before
//! enabling live trading.
//!
//! PnL model (unified across all entry points): each trade risks `risk_pct` of
//! equity. A win returns `reward_risk * risk_pct`, a stop-loss returns
//! `-risk_pct`, and an expired setup (never hit TP or SL in the window) is a
//! quarter-R time-stop (`-0.25 * risk_pct`). A round-trip `fee_pct` is
//! subtracted from every trade. Equity compounds multiplicatively.

use serde_json::{json, Value};

use crate::ai::{DecisionEngine, DecisionInputs};
use crate::config::{BacktestConfig, DecisionConfig};
use crate::ml::features::normalize_feature_vector;
use crate::ml::online::OnlineClassifier;
use crate::ml::MarketRegime;

/// Fraction of `risk_pct` lost when a setup expires (time-stop) instead of
/// hitting its TP or SL.
const EXPIRED_LOSS_FRACTION: f64 = 0.25;

/// One simulated trade outcome.
#[derive(Debug, Clone, Copy)]
struct SimTrade {
    won: bool,
    expired: bool,
    /// PnL as a fraction of equity (already net of fees).
    pnl: f64,
}

/// Extract entry/SL/TP from a stored signal and simulate its PnL. Returns
/// `None` for unresolved (pending) signals.
fn simulate_trade(sig: &Value, fee_pct: f64, risk_pct: f64) -> Option<(SimTrade, f64)> {
    let outcome = sig.get("outcome").and_then(|v| v.as_str()).unwrap_or("pending");
    if !matches!(outcome, "win" | "loss" | "expired") {
        return None;
    }

    let entry = sig.get("last_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let sl = sig.get("projected_stop_loss").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let tp = sig
        .get("projected_take_profits")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let reward_risk = if entry > 0.0 && sl > 0.0 && tp > 0.0 {
        let risk_dist = (entry - sl).abs();
        let reward_dist = (tp - entry).abs();
        if risk_dist > 1e-12 {
            reward_dist / risk_dist
        } else {
            1.0
        }
    } else {
        1.0
    };

    let pnl = match outcome {
        "win" => reward_risk * risk_pct - fee_pct,
        "loss" => -risk_pct - fee_pct,
        "expired" => -EXPIRED_LOSS_FRACTION * risk_pct - fee_pct,
        _ => -fee_pct,
    };

    Some((
        SimTrade {
            won: outcome == "win",
            expired: outcome == "expired",
            pnl,
        },
        reward_risk,
    ))
}

/// Compute the full metric set from an ordered list of simulated trades.
fn metrics_json(trades: &[SimTrade], filtered: u64, total: u64, settings: Value) -> Value {
    let mut wins = 0u64;
    let mut losses = 0u64;
    let mut expired = 0u64;
    let mut gross_profit = 0.0f64;
    let mut gross_loss = 0.0f64;
    let mut equity = 1.0f64;
    let mut peak = 1.0f64;
    let mut max_dd = 0.0f64;
    let mut equity_curve = vec![1.0f64];

    for t in trades {
        if t.won {
            wins += 1;
        } else if t.expired {
            expired += 1;
        } else {
            losses += 1;
        }
        if t.pnl >= 0.0 {
            gross_profit += t.pnl;
        } else {
            gross_loss += -t.pnl;
        }
        equity *= 1.0 + t.pnl;
        if equity > peak {
            peak = equity;
        }
        let dd = if peak > 0.0 { (peak - equity) / peak } else { 0.0 };
        if dd > max_dd {
            max_dd = dd;
        }
        equity_curve.push((equity * 10000.0).round() / 10000.0);
    }

    let traded = wins + losses + expired;
    let win_rate = if wins + losses > 0 {
        wins as f64 / (wins + losses) as f64
    } else {
        0.0
    };
    let total_return = equity - 1.0;
    let expectancy = if traded > 0 { total_return / traded as f64 } else { 0.0 };
    let profit_factor = if gross_loss > 1e-12 {
        Some(gross_profit / gross_loss)
    } else if gross_profit > 0.0 {
        None // no losses at all — profit factor is infinite
    } else {
        Some(0.0)
    };
    let avg_win = if wins > 0 { gross_profit / wins as f64 } else { 0.0 };
    let avg_loss = if losses + expired > 0 {
        gross_loss / (losses + expired) as f64
    } else {
        0.0
    };

    let r = |x: f64| (x * 10000.0).round() / 10000.0;
    json!({
        "total_signals": total,
        "filtered": filtered,
        "traded": traded,
        "wins": wins,
        "losses": losses,
        "expired": expired,
        "win_rate": r(win_rate),
        "total_return_pct": r(total_return),
        "max_drawdown_pct": r(max_dd),
        "expectancy_per_trade": r(expectancy),
        "profit_factor": profit_factor.map(r),
        "avg_win": r(avg_win),
        "avg_loss": r(avg_loss),
        "gross_profit": r(gross_profit),
        "gross_loss": r(gross_loss),
        "equity_curve": equity_curve,
        "settings": settings,
    })
}

pub struct Backtester;

impl Default for Backtester {
    fn default() -> Self {
        Self::new()
    }
}

impl Backtester {
    pub fn new() -> Self {
        Self
    }

    pub fn run_json(&self, signals: &[Value], ml_threshold: f64, fee_pct: f64, risk_pct: f64) -> Value {
        Self::run(signals, ml_threshold, fee_pct, risk_pct)
    }

    /// Replay resolved signals, trading only those whose stored
    /// `setup_probability_pct` clears `ml_threshold`.
    pub fn run(signals: &[Value], ml_threshold: f64, fee_pct: f64, risk_pct: f64) -> Value {
        let mut trades = Vec::new();
        let mut total = 0u64;
        let mut filtered = 0u64;

        for sig in signals {
            let Some((trade, _rr)) = simulate_trade(sig, fee_pct, risk_pct) else {
                continue;
            };
            total += 1;
            let setup_prob = sig.get("setup_probability_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if setup_prob < ml_threshold {
                filtered += 1;
                continue;
            }
            trades.push(trade);
        }

        metrics_json(
            &trades,
            filtered,
            total,
            json!({
                "gate": "ml_threshold",
                "ml_threshold": ml_threshold,
                "fee_pct": fee_pct,
                "risk_pct": risk_pct,
            }),
        )
    }

    pub fn run_decision_json(
        &self,
        signals: &[Value],
        decision_cfg: &DecisionConfig,
        fee_pct: f64,
        risk_pct: f64,
    ) -> Value {
        Self::run_decision(signals, decision_cfg, fee_pct, risk_pct)
    }

    /// Replay resolved signals through the Phase 5 decision engine (neutral
    /// regime, no sentiment — deterministic), trading only approved signals
    /// and scaling each trade's risk by the decision's `size_scale` so the
    /// backtest mirrors the live conviction-sizing behavior.
    pub fn run_decision(
        signals: &[Value],
        decision_cfg: &DecisionConfig,
        fee_pct: f64,
        risk_pct: f64,
    ) -> Value {
        let mut trades = Vec::new();
        let mut total = 0u64;
        let mut filtered = 0u64;

        for sig in signals {
            if simulate_trade(sig, fee_pct, risk_pct).is_none() {
                continue;
            }
            total += 1;

            let entry = sig.get("last_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let sl = sig.get("projected_stop_loss").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tp = sig
                .get("projected_take_profits")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let win_prob = sig.get("setup_probability_pct").and_then(|v| v.as_f64()).unwrap_or(0.0) / 100.0;
            let side_long = sig.get("price_change_pct").and_then(|v| v.as_f64()).unwrap_or(0.0) >= 0.0;

            let decision = DecisionEngine::decide(
                decision_cfg,
                &DecisionInputs {
                    win_prob,
                    side_long,
                    entry,
                    stop_loss: sl,
                    take_profit: tp,
                    regime: MarketRegime::default(),
                    global_sentiment: 0.0,
                    symbol_sentiment: 0.0,
                },
            );

            if !decision.approved {
                filtered += 1;
                continue;
            }

            // Re-simulate at the decision's conviction-scaled risk.
            let scaled_risk = risk_pct * decision.size_scale;
            if let Some((trade, _rr)) = simulate_trade(sig, fee_pct, scaled_risk) {
                trades.push(trade);
            }
        }

        metrics_json(
            &trades,
            filtered,
            total,
            json!({
                "gate": "decision_engine",
                "fee_pct": fee_pct,
                "risk_pct": risk_pct,
                "min_expected_value": decision_cfg.min_expected_value,
                "min_reward_risk": decision_cfg.min_reward_risk,
            }),
        )
    }
}

/// Evaluate a metrics object against the configured paper-acceptance gates.
/// Returns `{ passed, checks: [...], summary }`. This is the mandatory
/// pre-live check: `passed == true` means the strategy has cleared every
/// configured minimum on the replayed history.
pub fn acceptance_gate(metrics: &Value, cfg: &BacktestConfig) -> Value {
    let traded = metrics.get("traded").and_then(|v| v.as_u64()).unwrap_or(0);
    let win_rate = metrics.get("win_rate").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let expectancy = metrics.get("expectancy_per_trade").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let max_dd = metrics.get("max_drawdown_pct").and_then(|v| v.as_f64()).unwrap_or(1.0);
    // Missing profit_factor (null) means no losses — treat as passing.
    let profit_factor = metrics.get("profit_factor").and_then(|v| v.as_f64());

    let mut checks = Vec::new();
    let mut passed = true;

    let mut check = |name: &str, ok: bool, actual: Value, required: Value| {
        if !ok {
            passed = false;
        }
        checks.push(json!({
            "check": name,
            "passed": ok,
            "actual": actual,
            "required": required,
        }));
    };

    check(
        "min_trades",
        traded >= cfg.acceptance_min_trades as u64,
        json!(traded),
        json!(cfg.acceptance_min_trades),
    );
    check(
        "min_win_rate",
        win_rate >= cfg.acceptance_min_win_rate,
        json!(win_rate),
        json!(cfg.acceptance_min_win_rate),
    );
    check(
        "min_profit_factor",
        profit_factor.map(|pf| pf >= cfg.acceptance_min_profit_factor).unwrap_or(true),
        profit_factor.map(Value::from).unwrap_or(Value::Null),
        json!(cfg.acceptance_min_profit_factor),
    );
    check(
        "min_expectancy",
        expectancy >= cfg.acceptance_min_expectancy,
        json!(expectancy),
        json!(cfg.acceptance_min_expectancy),
    );
    check(
        "max_drawdown",
        max_dd <= cfg.acceptance_max_drawdown,
        json!(max_dd),
        json!(cfg.acceptance_max_drawdown),
    );

    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| !c.get("passed").and_then(|v| v.as_bool()).unwrap_or(true))
        .filter_map(|c| c.get("check").and_then(|v| v.as_str()))
        .collect();

    json!({
        "passed": passed,
        "checks": checks,
        "summary": if passed {
            "All acceptance gates passed — strategy cleared for live consideration".to_string()
        } else {
            format!("Failed acceptance gates: {}", failed.join(", "))
        },
    })
}

pub struct StrategyLearner;

impl Default for StrategyLearner {
    fn default() -> Self {
        Self::new()
    }
}

impl StrategyLearner {
    pub fn new() -> Self {
        Self
    }

    pub fn walk_forward_json(&self, signals: &[Value], train_frac: f64, onnx_path: Option<&str>) -> Value {
        Self::walk_forward(signals, train_frac, onnx_path)
    }

    /// Walk-forward validation:
    ///   1. Filter to resolved (win/loss) signals in stored (chronological) order.
    ///   2. Train the online model on the first `train_frac` fraction.
    ///   3. Evaluate on the remaining out-of-sample fraction, reporting both
    ///      classification quality (accuracy, precision) and traded PnL metrics
    ///      (win rate, expectancy, profit factor, max drawdown) for the trades
    ///      the model *would* have taken (predicted win).
    pub fn walk_forward(signals: &[Value], train_frac: f64, onnx_path: Option<&str>) -> Value {
        let resolved: Vec<&Value> = signals
            .iter()
            .filter(|s| {
                matches!(
                    s.get("outcome").and_then(|v| v.as_str()).unwrap_or(""),
                    "win" | "loss"
                )
            })
            .collect();

        if resolved.len() < 20 {
            return json!({
                "error": "Not enough resolved signals for walk-forward (need >= 20 win/loss samples)",
                "resolved": resolved.len(),
            });
        }

        let split = ((resolved.len() as f64 * train_frac) as usize).max(10).min(resolved.len() - 5);
        let train_set = &resolved[..split];
        let test_set = &resolved[split..];

        let mut clf = OnlineClassifier::load(onnx_path);

        let mut in_sample_correct = 0u64;
        let mut in_sample_total = 0u64;
        for sig in train_set {
            let features = extract_features(sig);
            if features.iter().all(|&v| v == 0.0) {
                continue;
            }
            let won = sig.get("outcome").and_then(|v| v.as_str()) == Some("win");
            clf.update(&features, won);
            in_sample_total += 1;
            if won {
                in_sample_correct += 1;
            }
        }
        let in_sample_win_rate = if in_sample_total > 0 {
            in_sample_correct as f64 / in_sample_total as f64
        } else {
            0.0
        };

        // Out-of-sample: for each test signal, the model predicts; if it would
        // trade (proba >= 0.5) we simulate the trade and accumulate PnL.
        let mut oos_correct = 0u64;
        let mut oos_total = 0u64;
        let mut oos_wins = 0u64;
        let mut oos_predicted_wins = 0u64;
        let mut traded: Vec<SimTrade> = Vec::new();
        for sig in test_set {
            let features = extract_features(sig);
            if features.iter().all(|&v| v == 0.0) {
                continue;
            }
            let won = sig.get("outcome").and_then(|v| v.as_str()) == Some("win");
            let proba = clf.predict_proba(&features);
            let predicted = proba >= 0.5;
            if predicted == won {
                oos_correct += 1;
            }
            if won {
                oos_wins += 1;
            }
            if predicted {
                oos_predicted_wins += 1;
                if let Some((trade, _rr)) = simulate_trade(sig, 0.001, 0.01) {
                    traded.push(trade);
                }
            }
            oos_total += 1;
        }
        let oos_accuracy = if oos_total > 0 { oos_correct as f64 / oos_total as f64 } else { 0.0 };
        let oos_win_rate = if oos_total > 0 { oos_wins as f64 / oos_total as f64 } else { 0.0 };
        let precision = if oos_predicted_wins > 0 {
            oos_wins as f64 / oos_predicted_wins as f64
        } else {
            0.0
        };

        let pnl_metrics = metrics_json(
            &traded,
            oos_total - oos_predicted_wins,
            oos_total,
            json!({ "gate": "walk_forward_model", "fee_pct": 0.001, "risk_pct": 0.01 }),
        );

        let r = |x: f64| (x * 10000.0).round() / 10000.0;
        json!({
            "train_samples": train_set.len(),
            "test_samples": test_set.len(),
            "in_sample_win_rate": r(in_sample_win_rate),
            "out_of_sample": {
                "accuracy": r(oos_accuracy),
                "win_rate": r(oos_win_rate),
                "precision": r(precision),
                "total": oos_total,
                "wins": oos_wins,
                "predicted_wins": oos_predicted_wins,
                "traded_pnl": pnl_metrics,
            },
            "train_frac": train_frac,
        })
    }
}

fn extract_features(sig: &Value) -> Vec<f64> {
    use crate::ml::features::FEATURE_DIM;
    let raw: Vec<f64> = sig
        .get("ml_features")
        .and_then(|f| f.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_f64()).collect())
        .unwrap_or_default();
    normalize_feature_vector(Some(&raw), FEATURE_DIM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(outcome: &str, prob: f64, entry: f64, sl: f64, tp: f64, chg: f64) -> Value {
        json!({
            "outcome": outcome,
            "setup_probability_pct": prob,
            "last_price": entry,
            "projected_stop_loss": sl,
            "projected_take_profits": [tp],
            "price_change_pct": chg,
            "ml_features": vec![0.1_f64; 33],
        })
    }

    #[test]
    fn run_computes_profit_factor_and_win_rate() {
        let signals = vec![
            sig("win", 80.0, 100.0, 98.0, 104.0, 0.02),
            sig("loss", 80.0, 100.0, 98.0, 104.0, 0.02),
            sig("win", 80.0, 100.0, 98.0, 104.0, 0.02),
        ];
        let m = Backtester::run(&signals, 50.0, 0.0, 0.01);
        assert_eq!(m["traded"], 3);
        assert_eq!(m["wins"], 2);
        assert_eq!(m["losses"], 1);
        // win_rate = 2/3
        assert!((m["win_rate"].as_f64().unwrap() - 0.6667).abs() < 0.01);
        // profit factor: gross win = 2 * (2.0 * 0.01) = 0.04; gross loss = 0.01; pf = 4.0
        assert!((m["profit_factor"].as_f64().unwrap() - 4.0).abs() < 0.1);
    }

    #[test]
    fn ml_threshold_filters_low_probability_signals() {
        let signals = vec![
            sig("win", 40.0, 100.0, 98.0, 104.0, 0.02),
            sig("loss", 90.0, 100.0, 98.0, 104.0, 0.02),
        ];
        let m = Backtester::run(&signals, 50.0, 0.0, 0.01);
        assert_eq!(m["filtered"], 1);
        assert_eq!(m["traded"], 1);
    }

    #[test]
    fn decision_gate_rejects_low_ev() {
        let cfg = DecisionConfig::default();
        // rr = 0.25 (tp 100.5, sl 98) → below min_reward_risk regardless of prob.
        let signals = vec![sig("win", 95.0, 100.0, 98.0, 100.5, 0.02)];
        let m = Backtester::run_decision(&signals, &cfg, 0.0, 0.01);
        assert_eq!(m["filtered"], 1);
        assert_eq!(m["traded"], 0);
    }

    #[test]
    fn decision_gate_trades_positive_ev() {
        let cfg = DecisionConfig::default();
        // rr = 2.0, prob 70% → EV = 0.7*2 - 0.3 = 1.1R.
        let signals = vec![sig("win", 70.0, 100.0, 98.0, 104.0, 0.02)];
        let m = Backtester::run_decision(&signals, &cfg, 0.0, 0.01);
        assert_eq!(m["traded"], 1);
        assert_eq!(m["wins"], 1);
    }

    #[test]
    fn acceptance_gate_fails_on_too_few_trades() {
        let cfg = BacktestConfig::default();
        let signals = vec![sig("win", 70.0, 100.0, 98.0, 104.0, 0.02)];
        let m = Backtester::run_decision(&signals, &DecisionConfig::default(), 0.0, 0.01);
        let gate = acceptance_gate(&m, &cfg);
        assert_eq!(gate["passed"], false);
        assert!(gate["summary"].as_str().unwrap().contains("min_trades"));
    }

    #[test]
    fn acceptance_gate_passes_strong_history() {
        let mut cfg = BacktestConfig::default();
        cfg.acceptance_min_trades = 5;
        // 8 wins, 2 losses at rr 2.0 → win rate 0.8, strong PF, positive expectancy.
        let mut signals = Vec::new();
        for _ in 0..8 {
            signals.push(sig("win", 70.0, 100.0, 98.0, 104.0, 0.02));
        }
        for _ in 0..2 {
            signals.push(sig("loss", 70.0, 100.0, 98.0, 104.0, 0.02));
        }
        let m = Backtester::run_decision(&signals, &DecisionConfig::default(), 0.0, 0.01);
        let gate = acceptance_gate(&m, &cfg);
        assert_eq!(gate["passed"], true, "gate: {gate}");
    }

    #[test]
    fn expired_is_a_partial_loss() {
        let signals = vec![sig("expired", 80.0, 100.0, 98.0, 104.0, 0.02)];
        let m = Backtester::run(&signals, 50.0, 0.0, 0.01);
        assert_eq!(m["expired"], 1);
        // total return should be a small negative (quarter-R).
        assert!(m["total_return_pct"].as_f64().unwrap() < 0.0);
        assert!(m["total_return_pct"].as_f64().unwrap() > -0.01);
    }
}
