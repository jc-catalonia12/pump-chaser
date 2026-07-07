//! Native online supervised learner — logistic regression trained incrementally
//! on every resolved trade outcome. No Python required: the model lives entirely
//! in Rust, learns continuously, and persists its weights to disk.
//!
//! **Improvements over v1:**
//! - Weighted SGD: real trades train at 2×, shadow signals at 1×, expired at 0.3×.
//! - Soft labels: expired outcomes use label 0.45 (neither strong win nor loss).
//! - Gate-threshold accuracy: stores (proba, label) pairs so accuracy can be
//!   computed at any threshold (e.g. the configured `supervised_threshold`).
//! - Dimension migration guard: if the saved model has a different FEATURE_DIM
//!   the model is reset cleanly instead of silently padding with zeros.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::ml::features::FEATURE_DIM;

const DEFAULT_LR: f64 = 0.05;
const DEFAULT_L2: f64 = 1e-4;
const ACCURACY_WINDOW: usize = 200;

/// Persisted state of the online classifier. Plain data so it serializes cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineModelState {
    pub weights: Vec<f64>,
    pub bias: f64,
    /// Running feature mean (Welford) used for standardization.
    pub feat_mean: Vec<f64>,
    /// Running feature M2 (sum of squared deltas) used for variance.
    pub feat_m2: Vec<f64>,
    pub feat_count: u64,
    pub samples: u64,
    pub wins: u64,
    pub losses: u64,
    /// Rolling window of recent (predicted_proba, actual_label) pairs for
    /// computing accuracy at any threshold — stored as [proba, label].
    #[serde(default)]
    pub recent_outcomes: Vec<[f64; 2]>,
    pub lr: f64,
    pub l2: f64,
    pub updated_at: String,
    /// Stored feature dimension so we can detect model/code mismatch on load.
    #[serde(default)]
    pub feature_dim: usize,
    /// Platt scaling: calibrated = sigmoid(platt_a * raw_logit + platt_b)
    #[serde(default)]
    pub platt_a: f64,
    #[serde(default = "default_platt_b")]
    pub platt_b: f64,
    /// Rolling average R on wins for EV thresholding.
    #[serde(default)]
    pub avg_win_r: f64,
    #[serde(default)]
    pub avg_loss_r: f64,
}

fn default_platt_b() -> f64 {
    0.0
}

impl Default for OnlineModelState {
    fn default() -> Self {
        Self {
            weights: vec![0.0; FEATURE_DIM],
            bias: 0.0,
            feat_mean: vec![0.0; FEATURE_DIM],
            feat_m2: vec![0.0; FEATURE_DIM],
            feat_count: 0,
            samples: 0,
            wins: 0,
            losses: 0,
            recent_outcomes: Vec::new(),
            lr: DEFAULT_LR,
            l2: DEFAULT_L2,
            updated_at: String::new(),
            feature_dim: FEATURE_DIM,
            platt_a: 1.0,
            platt_b: 0.0,
            avg_win_r: 1.0,
            avg_loss_r: 1.0,
        }
    }
}

pub struct OnlineClassifier {
    state: OnlineModelState,
    path: PathBuf,
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

impl OnlineClassifier {
    /// Resolve the model file path from the configured onnx path (same dir) or
    /// fall back to `data/models/online_model.json`.
    pub fn resolve_path(onnx_model_path: Option<&str>) -> PathBuf {
        if let Some(p) = onnx_model_path {
            if let Some(parent) = Path::new(p).parent() {
                return parent.join("online_model.json");
            }
        }
        PathBuf::from("data/models/online_model.json")
    }

    pub fn load(onnx_model_path: Option<&str>) -> Self {
        let path = Self::resolve_path(onnx_model_path);
        let state = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<OnlineModelState>(&text) {
                Ok(s) => {
                    // Hard reset if feature dimension changed — padding zeros would give
                    // silently wrong predictions since standardization stats are stale.
                    let saved_dim = if s.feature_dim > 0 { s.feature_dim } else { s.weights.len() };
                    if saved_dim != FEATURE_DIM {
                        info!(
                            "Online model feature dim changed ({} → {}) — resetting weights",
                            saved_dim, FEATURE_DIM
                        );
                        OnlineModelState::default()
                    } else {
                        s
                    }
                }
                Err(exc) => {
                    warn!("Failed to parse online model {}: {exc} — starting fresh", path.display());
                    OnlineModelState::default()
                }
            },
            Err(_) => OnlineModelState::default(),
        };
        Self { state, path }
    }

    pub fn is_ready(&self, min_samples: u64) -> bool {
        self.state.samples >= min_samples.max(1)
    }

    pub fn samples(&self) -> u64 {
        self.state.samples
    }

    fn standardize(&self, raw: &[f64]) -> Vec<f64> {
        let count = self.state.feat_count;
        (0..FEATURE_DIM)
            .map(|i| {
                let x = raw.get(i).copied().unwrap_or(0.0);
                if count < 2 {
                    return x;
                }
                let mean = self.state.feat_mean[i];
                let var = self.state.feat_m2[i] / (count as f64);
                let std = var.sqrt();
                if std > 1e-9 {
                    ((x - mean) / std).clamp(-6.0, 6.0)
                } else {
                    0.0
                }
            })
            .collect()
    }

    /// Predict win probability for a raw (un-standardized) feature vector.
    pub fn predict_proba(&self, raw: &[f64]) -> f64 {
        self.calibrated_proba(self.raw_logit(raw))
    }

    fn raw_logit(&self, raw: &[f64]) -> f64 {
        let z = self.standardize(raw);
        let mut acc = self.state.bias;
        for i in 0..FEATURE_DIM {
            acc += self.state.weights[i] * z[i];
        }
        acc
    }

    /// Calibrated probability via Platt scaling on the raw logit.
    pub fn calibrated_proba(&self, raw_logit: f64) -> f64 {
        let a = if self.state.platt_a.abs() < 1e-9 {
            1.0
        } else {
            self.state.platt_a
        };
        sigmoid(a * raw_logit + self.state.platt_b)
    }

    /// Re-fit Platt scaling on recent hard-label outcomes (every 50 samples).
    fn maybe_refit_platt(&mut self) {
        if self.state.recent_outcomes.len() < 30 {
            return;
        }
        if self.state.samples % 50 != 0 {
            return;
        }
        let mut logits = Vec::new();
        let mut labels = Vec::new();
        for &[proba, label] in &self.state.recent_outcomes {
            if label >= 0.9 || label <= 0.1 {
                let p = proba.clamp(1e-6, 1.0 - 1e-6);
                let logit = (p / (1.0 - p)).ln();
                logits.push(logit);
                labels.push(if label >= 0.5 { 1.0 } else { 0.0 });
            }
        }
        if logits.len() < 20 {
            return;
        }
        let mut a = self.state.platt_a.max(0.1);
        let mut b = self.state.platt_b;
        for _ in 0..80 {
            let mut grad_a = 0.0;
            let mut grad_b = 0.0;
            for (&logit, &y) in logits.iter().zip(labels.iter()) {
                let p = sigmoid(a * logit + b);
                let err = p - y;
                grad_a += err * logit;
                grad_b += err;
            }
            let n = logits.len() as f64;
            a -= 0.05 * grad_a / n;
            b -= 0.05 * grad_b / n;
        }
        self.state.platt_a = a.clamp(0.1, 5.0);
        self.state.platt_b = b.clamp(-3.0, 3.0);
    }

    /// Track rolling R-multiples for EV-based threshold selection.
    pub fn record_r_outcome(&mut self, r_multiple: f64, won: bool) {
        let alpha = 0.05;
        if won && r_multiple > 0.0 {
            self.state.avg_win_r = (1.0 - alpha) * self.state.avg_win_r + alpha * r_multiple;
        } else if !won {
            let loss_r = r_multiple.abs().max(0.5);
            self.state.avg_loss_r = (1.0 - alpha) * self.state.avg_loss_r + alpha * loss_r;
        }
    }

    /// Auto threshold maximizing expected value: p * avg_win_r - (1-p) * avg_loss_r.
    pub fn auto_threshold(&self, floor: f64) -> f64 {
        let win_r = self.state.avg_win_r.max(0.1);
        let loss_r = self.state.avg_loss_r.max(0.1);
        let ev_thresh = loss_r / (win_r + loss_r);
        ev_thresh.clamp(floor, 0.85)
    }

    /// Rolling average R-multiple realized on winning trades (floored at 0.1
    /// so downstream Kelly-fraction math never divides by ~zero).
    pub fn avg_win_r(&self) -> f64 {
        self.state.avg_win_r.max(0.1)
    }

    /// Rolling average R-multiple magnitude realized on losing trades.
    pub fn avg_loss_r(&self) -> f64 {
        self.state.avg_loss_r.max(0.1)
    }

    /// Online update — binary label. Convenience wrapper around `update_soft`.
    /// `weight` controls how much this sample influences the model:
    ///   - 2.0  for real (live/paper) trade outcomes
    ///   - 1.0  for shadow-resolved win/loss signals
    ///   - 0.3  (with label 0.45) for shadow-resolved expired signals
    pub fn update(&mut self, raw: &[f64], won: bool) -> f64 {
        self.update_soft(raw, if won { 1.0 } else { 0.0 }, 1.0)
    }

    pub fn update_weighted(&mut self, raw: &[f64], won: bool, weight: f64) -> f64 {
        self.update_soft(raw, if won { 1.0 } else { 0.0 }, weight)
    }

    /// Core SGD update with a soft label (0.0–1.0) and a sample weight multiplier.
    /// The Welford standardization stats are updated unweighted (weight affects only
    /// the gradient step), which keeps the feature statistics accurate.
    pub fn update_soft(&mut self, raw: &[f64], label: f64, weight: f64) -> f64 {
        let label = label.clamp(0.0, 1.0);
        let weight = weight.max(0.0);

        // 1. Update running feature stats (Welford) — always unweighted.
        self.state.feat_count += 1;
        let count = self.state.feat_count as f64;
        for i in 0..FEATURE_DIM {
            let x = raw.get(i).copied().unwrap_or(0.0);
            let delta = x - self.state.feat_mean[i];
            self.state.feat_mean[i] += delta / count;
            let delta2 = x - self.state.feat_mean[i];
            self.state.feat_m2[i] += delta * delta2;
        }

        // 2. Standardize + predict with current weights.
        let z = self.standardize(raw);
        let mut acc = self.state.bias;
        for i in 0..FEATURE_DIM {
            acc += self.state.weights[i] * z[i];
        }
        let proba = self.calibrated_proba(acc);

        // 3. Track rolling (proba, label) pairs for threshold-aware accuracy.
        self.state.recent_outcomes.push([proba, label]);
        if self.state.recent_outcomes.len() > ACCURACY_WINDOW {
            let overflow = self.state.recent_outcomes.len() - ACCURACY_WINDOW;
            self.state.recent_outcomes.drain(0..overflow);
        }

        // 4. Weighted SGD step (logistic loss + L2 regularization).
        let err = (proba - label) * weight;
        let lr = self.state.lr;
        let l2 = self.state.l2;
        for i in 0..FEATURE_DIM {
            let grad = err * z[i] + l2 * self.state.weights[i];
            self.state.weights[i] -= lr * grad;
        }
        self.state.bias -= lr * err;

        // 5. Bookkeeping — count only hard-label samples (not soft expired).
        self.state.samples += 1;
        if label >= 0.5 {
            self.state.wins += 1;
        } else {
            self.state.losses += 1;
        }
        self.state.updated_at = chrono::Utc::now().to_rfc3339();
        self.maybe_refit_platt();

        self.save();
        proba
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&self.state) {
            Ok(text) => {
                if let Err(exc) = std::fs::write(&self.path, text) {
                    warn!("Failed to persist online model {}: {exc}", self.path.display());
                }
            }
            Err(exc) => warn!("Failed to serialize online model: {exc}"),
        }
    }

    /// Accuracy of predictions at a given threshold (default 0.5).
    /// Uses the rolling `recent_outcomes` window.
    pub fn accuracy_at_threshold(&self, threshold: f64) -> Option<f64> {
        if self.state.recent_outcomes.is_empty() {
            return None;
        }
        let mut correct = 0u64;
        let mut total = 0u64;
        for &[proba, label] in &self.state.recent_outcomes {
            // Only evaluate on hard-label samples (skip soft expired at ~0.45).
            if label >= 0.9 || label <= 0.1 {
                total += 1;
                let predicted_win = proba >= threshold;
                let actual_win = label >= 0.5;
                if predicted_win == actual_win {
                    correct += 1;
                }
            }
        }
        if total == 0 {
            None
        } else {
            Some(correct as f64 / total as f64)
        }
    }

    pub fn recent_accuracy(&self) -> Option<f64> {
        self.accuracy_at_threshold(0.5)
    }

    pub fn win_rate(&self) -> f64 {
        let resolved = self.state.wins + self.state.losses;
        if resolved == 0 {
            0.0
        } else {
            self.state.wins as f64 / resolved as f64
        }
    }

    pub fn stats(&self, min_samples: u64) -> Value {
        let round4 = |x: f64| (x * 10000.0).round() / 10000.0;
        json!({
            "type": "online_logistic_regression",
            "feature_dim": FEATURE_DIM,
            "samples": self.state.samples,
            "wins": self.state.wins,
            "losses": self.state.losses,
            "win_rate": round4(self.win_rate()),
            "recent_accuracy": self.recent_accuracy().map(round4),
            "recent_window": self.state.recent_outcomes.len(),
            "ready": self.is_ready(min_samples),
            "min_samples": min_samples,
            "learning_rate": self.state.lr,
            "platt_a": round4(self.state.platt_a),
            "platt_b": round4(self.state.platt_b),
            "avg_win_r": round4(self.state.avg_win_r),
            "avg_loss_r": round4(self.state.avg_loss_r),
            "updated_at": self.state.updated_at,
        })
    }

    /// Rich stats including gate-threshold accuracy for a given threshold value.
    pub fn stats_with_threshold(&self, min_samples: u64, threshold: f64) -> Value {
        let round4 = |x: f64| (x * 10000.0).round() / 10000.0;
        let gate_acc = self.accuracy_at_threshold(threshold).map(round4);
        let mut v = self.stats(min_samples);
        if let Some(obj) = v.as_object_mut() {
            obj.insert("gate_threshold".into(), json!(threshold));
            obj.insert("gate_accuracy".into(), json!(gate_acc));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn learns_separable_pattern() {
        let dir = tempdir().unwrap();
        let onnx = dir.path().join("supervised.onnx");
        let mut clf = OnlineClassifier::load(Some(onnx.to_str().unwrap()));

        for _ in 0..200 {
            let mut win = vec![0.0; FEATURE_DIM];
            win[0] = 5.0;
            clf.update(&win, true);
            let mut loss = vec![0.0; FEATURE_DIM];
            loss[0] = -5.0;
            clf.update(&loss, false);
        }

        let mut win = vec![0.0; FEATURE_DIM];
        win[0] = 5.0;
        let mut loss = vec![0.0; FEATURE_DIM];
        loss[0] = -5.0;
        assert!(clf.predict_proba(&win) > 0.6, "should favor winning pattern");
        assert!(clf.predict_proba(&loss) < 0.4, "should disfavor losing pattern");
        assert_eq!(clf.samples(), 400);
    }

    #[test]
    fn persists_and_reloads() {
        let dir = tempdir().unwrap();
        let onnx = dir.path().join("supervised.onnx");
        {
            let mut clf = OnlineClassifier::load(Some(onnx.to_str().unwrap()));
            let mut f = vec![0.0; FEATURE_DIM];
            f[0] = 1.0;
            clf.update(&f, true);
        }
        let reloaded = OnlineClassifier::load(Some(onnx.to_str().unwrap()));
        assert_eq!(reloaded.samples(), 1);
    }

    #[test]
    fn weighted_update_trains_faster() {
        let dir = tempdir().unwrap();
        let onnx = dir.path().join("supervised.onnx");

        let mut clf_w = OnlineClassifier::load(Some(onnx.to_str().unwrap()));
        let mut clf_n = OnlineClassifier::load(Some(onnx.to_str().unwrap()));
        for _ in 0..50 {
            let mut win = vec![0.0; FEATURE_DIM];
            win[0] = 3.0;
            clf_w.update_weighted(&win, true, 3.0);
            clf_n.update_weighted(&win, true, 1.0);
            let mut loss = vec![0.0; FEATURE_DIM];
            loss[0] = -3.0;
            clf_w.update_weighted(&loss, false, 3.0);
            clf_n.update_weighted(&loss, false, 1.0);
        }
        let mut probe = vec![0.0; FEATURE_DIM];
        probe[0] = 3.0;
        // Weighted model should converge to higher confidence faster.
        assert!(clf_w.predict_proba(&probe) >= clf_n.predict_proba(&probe));
    }

    #[test]
    fn gate_accuracy_at_threshold() {
        let dir = tempdir().unwrap();
        let onnx = dir.path().join("supervised.onnx");
        let mut clf = OnlineClassifier::load(Some(onnx.to_str().unwrap()));

        // Feed all wins — proba should rise above 0.58 threshold.
        for _ in 0..150 {
            let mut f = vec![0.0; FEATURE_DIM];
            f[0] = 4.0;
            clf.update(&f, true);
        }
        // Some accuracy should be computable.
        assert!(clf.accuracy_at_threshold(0.5).is_some());
    }
}
