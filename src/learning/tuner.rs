//! Walk-forward parameter tuner with champion/challenger promotion.

use serde_json::{json, Value};

use crate::backtest::Backtester;
use crate::db::Database;
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct ParamTuner {
    pub champion: Value,
}

impl ParamTuner {
    pub fn default_champion(cfg: &crate::config::AppConfig) -> Value {
        json!({
            "min_composite_score": 60.0,
            "default_sl_pct": cfg.risk.default_sl_pct,
            "supervised_threshold": cfg.ml.supervised_threshold,
            "trailing_activation_pct": cfg.risk.trailing_activation_pct,
            "trailing_stop_pct": cfg.risk.trailing_stop_pct,
        })
    }

    pub async fn load_champion(db: &Database, cfg: &crate::config::AppConfig) -> Result<Self> {
        let overlay = db.get_strategy_overlay().await?;
        let champion = if overlay.is_object() && !overlay.as_object().unwrap().is_empty() {
            overlay
        } else {
            Self::default_champion(cfg)
        };
        Ok(Self { champion })
    }

    /// Grid search over a small parameter space; returns best challenger + metrics.
    pub fn tune(signals: &[Value], champion: &Value) -> Value {
        let mut best_score = f64::NEG_INFINITY;
        let mut best_params = champion.clone();
        let mut best_metrics = json!({});

        let score_grid = [60.0, 65.0, 68.0, 72.0];
        let sl_grid = [0.010, 0.012, 0.015, 0.018];
        let thr_grid = [0.52, 0.55, 0.58, 0.62];

        for &min_score in &score_grid {
            for &sl in &sl_grid {
                for &thr in &thr_grid {
                    let filtered: Vec<Value> = signals
                        .iter()
                        .filter(|s| {
                            s.get("composite_score")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0)
                                >= min_score
                        })
                        .cloned()
                        .collect();
                    if filtered.len() < 30 {
                        continue;
                    }
                    let split = (filtered.len() as f64 * 0.8) as usize;
                    let train = &filtered[..split.max(10).min(filtered.len().saturating_sub(5))];
                    let test = &filtered[split.max(10)..];
                    if test.len() < 5 {
                        continue;
                    }
                    let train_m = Backtester::run(train, thr * 100.0, 0.0012, 0.01);
                    let test_m = Backtester::run(test, thr * 100.0, 0.0012, 0.01);
                    let train_wr = train_m
                        .get("win_rate")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let test_wr = test_m
                        .get("win_rate")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let test_ret = test_m
                        .get("total_return_pct")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    // Penalize overfit: require train WR not too far above test.
                    if train_wr - test_wr > 15.0 {
                        continue;
                    }
                    let score = test_wr * 0.6 + test_ret * 0.4;
                    if score > best_score {
                        best_score = score;
                        best_params = json!({
                            "min_composite_score": min_score,
                            "default_sl_pct": sl,
                            "supervised_threshold": thr,
                            "trailing_activation_pct": champion
                                .get("trailing_activation_pct")
                                .cloned()
                                .unwrap_or(json!(0.01)),
                            "trailing_stop_pct": champion
                                .get("trailing_stop_pct")
                                .cloned()
                                .unwrap_or(json!(0.008)),
                        });
                        best_metrics = json!({
                            "train_win_rate": train_wr,
                            "test_win_rate": test_wr,
                            "test_return_pct": test_ret,
                            "score": (score * 100.0).round() / 100.0,
                            "train_n": train.len(),
                            "test_n": test.len(),
                        });
                    }
                }
            }
        }

        json!({
            "champion": champion,
            "challenger": best_params,
            "oos_metrics": best_metrics,
            "improved": best_score > f64::NEG_INFINITY,
        })
    }

    pub fn should_promote(champion_metrics: &Value, challenger_metrics: &Value) -> bool {
        let c_wr = champion_metrics
            .get("test_win_rate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let n_wr = challenger_metrics
            .get("test_win_rate")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let c_ret = champion_metrics
            .get("test_return_pct")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let n_ret = challenger_metrics
            .get("test_return_pct")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        n_wr >= c_wr + 2.0 && n_ret >= c_ret
    }
}
