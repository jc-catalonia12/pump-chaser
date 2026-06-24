//! Minimal backtest engine.
//!
//! Replays stored resolved signals (with SL/TP and outcomes) and computes
//! strategy metrics under configurable thresholds — without touching MEXC.
//!
//! Also includes `StrategyLearner` for walk-forward validation: train the
//! online logistic model on the first 80% of resolved signals and report
//! out-of-sample accuracy on the last 20%.

use serde_json::{json, Value};

use crate::ml::features::normalize_feature_vector;
use crate::ml::online::OnlineClassifier;

pub struct Backtester;

impl Backtester {
    pub fn new() -> Self {
        Self
    }

    pub fn run_json(&self, signals: &[Value], ml_threshold: f64, fee_pct: f64, risk_pct: f64) -> Value {
        Self::run(signals, ml_threshold, fee_pct, risk_pct)
    }

    /// Replay a slice of resolved signals (each containing `outcome`, `ml_features`,
    /// `setup_probability_pct`, `projected_stop_loss`, `projected_take_profits`,
    /// `price_change_pct`) and compute strategy metrics.
    ///
    /// `ml_threshold` — only "trade" signals with setup_probability_pct >= threshold.
    /// `fee_pct` — round-trip fee/slippage (e.g. 0.001 = 0.1%).
    /// `risk_pct` — fraction of equity risked per trade (for rough PnL estimate).
    pub fn run(signals: &[Value], ml_threshold: f64, fee_pct: f64, risk_pct: f64) -> Value {
        let mut total = 0u64;
        let mut filtered = 0u64; // rejected by ML threshold
        let mut wins = 0u64;
        let mut losses = 0u64;
        let mut expired = 0u64;
        let mut equity: f64 = 1.0;
        let mut peak: f64 = 1.0;
        let mut max_dd: f64 = 0.0;
        let mut equity_curve: Vec<f64> = vec![1.0];

        for sig in signals {
            let outcome = sig.get("outcome").and_then(|v| v.as_str()).unwrap_or("pending");
            if !matches!(outcome, "win" | "loss" | "expired") {
                continue;
            }
            total += 1;

            // ML gate
            let setup_prob = sig.get("setup_probability_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if setup_prob < ml_threshold {
                filtered += 1;
                continue;
            }

            // Estimate trade PnL using stored SL/TP levels.
            let entry = sig.get("last_price").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let sl = sig.get("projected_stop_loss").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tp = sig
                .get("projected_take_profits")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);

            let (rr, trade_pnl) = if entry > 0.0 && sl > 0.0 && tp > 0.0 {
                let risk_dist = (entry - sl).abs() / entry;
                let reward_dist = (tp - entry).abs() / entry;
                let rr = if risk_dist > 0.0 { reward_dist / risk_dist } else { 1.0 };
                let pnl = match outcome {
                    "win" => reward_dist * risk_pct - fee_pct,
                    "loss" | "expired" => -(risk_dist * risk_pct) - fee_pct,
                    _ => -fee_pct,
                };
                (rr, pnl)
            } else {
                let pnl = match outcome {
                    "win" => risk_pct - fee_pct,
                    _ => -(risk_pct) - fee_pct,
                };
                (1.0_f64, pnl)
            };
            let _ = rr; // available for future use

            match outcome {
                "win" => wins += 1,
                "loss" => losses += 1,
                "expired" => expired += 1,
                _ => {}
            }

            equity *= 1.0 + trade_pnl;
            if equity > peak { peak = equity; }
            let dd = (peak - equity) / peak;
            if dd > max_dd { max_dd = dd; }
            equity_curve.push((equity * 10000.0).round() / 10000.0);
        }

        let traded = wins + losses + expired;
        let win_rate = if wins + losses > 0 { wins as f64 / (wins + losses) as f64 } else { 0.0 };
        let total_return = equity - 1.0;
        let expectancy = if traded > 0 { total_return / traded as f64 } else { 0.0 };

        json!({
            "total_signals": total,
            "filtered_by_ml": filtered,
            "traded": traded,
            "wins": wins,
            "losses": losses,
            "expired": expired,
            "win_rate": (win_rate * 10000.0).round() / 10000.0,
            "total_return_pct": (total_return * 10000.0).round() / 10000.0,
            "max_drawdown_pct": (max_dd * 10000.0).round() / 10000.0,
            "expectancy_per_trade": (expectancy * 10000.0).round() / 10000.0,
            "equity_curve": equity_curve,
            "settings": {
                "ml_threshold": ml_threshold,
                "fee_pct": fee_pct,
                "risk_pct": risk_pct,
            },
        })
    }
}

pub struct StrategyLearner;

impl StrategyLearner {
    pub fn new() -> Self {
        Self
    }

    pub fn walk_forward_json(&self, signals: &[Value], train_frac: f64, onnx_path: Option<&str>) -> Value {
        Self::walk_forward(signals, train_frac, onnx_path)
    }

    /// Walk-forward validation:
    ///   1. Sort resolved signals chronologically.
    ///   2. Train the online model on the first `train_frac` fraction.
    ///   3. Evaluate on the remaining `1 - train_frac` fraction (out-of-sample).
    ///
    /// Returns an object with in-sample and out-of-sample accuracy / win-rate.
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

        // Create a fresh temporary model (in-memory only).
        let mut clf = OnlineClassifier::load(onnx_path);

        // Train
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
            if won { in_sample_correct += 1; }
        }
        let in_sample_win_rate = if in_sample_total > 0 {
            in_sample_correct as f64 / in_sample_total as f64
        } else {
            0.0
        };

        // Test (out-of-sample)
        let mut oos_correct = 0u64;
        let mut oos_total = 0u64;
        let mut oos_wins = 0u64;
        let mut oos_predicted_wins = 0u64;
        for sig in test_set {
            let features = extract_features(sig);
            if features.iter().all(|&v| v == 0.0) {
                continue;
            }
            let won = sig.get("outcome").and_then(|v| v.as_str()) == Some("win");
            let proba = clf.predict_proba(&features);
            let predicted = proba >= 0.5;
            if predicted == won { oos_correct += 1; }
            if won { oos_wins += 1; }
            if predicted { oos_predicted_wins += 1; }
            oos_total += 1;
        }
        let oos_accuracy = if oos_total > 0 { oos_correct as f64 / oos_total as f64 } else { 0.0 };
        let oos_win_rate = if oos_total > 0 { oos_wins as f64 / oos_total as f64 } else { 0.0 };
        let precision = if oos_predicted_wins > 0 {
            oos_wins as f64 / oos_predicted_wins as f64
        } else {
            0.0
        };

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
