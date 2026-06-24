//! ML inference + continuous learning.
//!
//! Scoring priority:
//!   1. Native online logistic-regression model once it has enough resolved
//!      trades (learns on every trade, no Python needed).
//!   2. Static ONNX model (optional, exported offline) as a cold-start fallback.
//!
//! The online model is the one that "keeps getting better": every closed trade
//! feeds `record_outcome`, which updates the weights and persists them.

use serde_json::{json, Value};
use tracing::debug;
#[cfg(feature = "onnx")]
use tracing::warn;

use crate::config::SharedAppConfig;
use crate::exchange::KlineBar;
use crate::ml::features::{
    legacy_onnx_feature_vector, normalize_feature_vector, TechnicalFeatureBuilder, FEATURE_DIM,
    LEGACY_ONNX_FEATURE_DIM,
};
use crate::ml::online::OnlineClassifier;
#[cfg(feature = "onnx")]
use crate::ml::onnx::OnnxClassifier;
use crate::signals::PumpSignal;

/// Result of ML enrichment — distinguishes tradable signals from shadow-only rejects.
#[derive(Debug, Clone)]
pub enum EnhanceOutcome {
    /// Passed ML gate (or ML disabled) — eligible for execution.
    Tradable(PumpSignal),
    /// Failed hard ML gate — save for shadow training only.
    MlRejected(PumpSignal),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MlStatus {
    pub enabled: bool,
    pub supervised_enabled: bool,
    pub supervised_fitted: bool,
    pub hard_ml_gate: bool,
    pub supervised_threshold: f64,
    pub onnx_model_path: Option<String>,
}

pub struct MlPipeline {
    config: SharedAppConfig,
    #[cfg(feature = "onnx")]
    classifier: Option<OnnxClassifier>,
    online: OnlineClassifier,
}

impl MlPipeline {
    pub fn new(config: SharedAppConfig) -> Self {
        let online = OnlineClassifier::load(config.read().unwrap().ml.onnx_model_path.as_deref());
        #[cfg(feature = "onnx")]
        {
            let cfg = config.read().unwrap();
            let classifier = if cfg.ml.supervised_enabled {
                OnnxClassifier::try_load(&cfg.ml)
            } else {
                None
            };
            drop(cfg);
            Self {
                config,
                classifier,
                online,
            }
        }
        #[cfg(not(feature = "onnx"))]
        {
            Self { config, online }
        }
    }

    fn min_samples(&self) -> u64 {
        self.config.read().unwrap().ml.min_training_samples as u64
    }

    #[cfg(feature = "onnx")]
    fn onnx_loaded(&self) -> bool {
        self.classifier.is_some()
    }
    #[cfg(not(feature = "onnx"))]
    fn onnx_loaded(&self) -> bool {
        false
    }

    pub fn status(&self) -> MlStatus {
        let cfg = self.config.read().unwrap();
        let online_ready = self.online.is_ready(self.min_samples());
        MlStatus {
            enabled: cfg.ml.enabled,
            supervised_enabled: cfg.ml.supervised_enabled,
            supervised_fitted: online_ready || self.onnx_loaded(),
            hard_ml_gate: cfg.ml.hard_ml_gate,
            supervised_threshold: cfg.ml.supervised_threshold,
            onnx_model_path: cfg.ml.onnx_model_path.clone(),
        }
    }

    /// Rich learning status for the dashboard / API.
    pub fn learning_status(&self) -> Value {
        let cfg = self.config.read().unwrap();
        let online_ready = self.online.is_ready(self.min_samples());
        let active = if online_ready {
            "online"
        } else if self.onnx_loaded() {
            "onnx"
        } else {
            "warming_up"
        };
        json!({
            "enabled": cfg.ml.enabled,
            "supervised_enabled": cfg.ml.supervised_enabled,
            "hard_ml_gate": cfg.ml.hard_ml_gate,
            "supervised_threshold": cfg.ml.supervised_threshold,
            "active_model": active,
            "onnx_loaded": self.onnx_loaded(),
            "online_model": self.online.stats_with_threshold(
                self.min_samples(),
                cfg.ml.supervised_threshold,
            ),
        })
    }

    pub fn online_stats(&self) -> Value {
        self.online_stats_with_threshold()
    }

    pub fn online_sample_count(&self) -> u64 {
        self.online.samples()
    }

    pub fn build_features(&self, signal: &PumpSignal, klines: Option<&[KlineBar]>) -> Vec<f64> {
        let side_long = signal.price_change_pct >= 0.0;
        TechnicalFeatureBuilder::signal_features(
            klines,
            signal.composite_score,
            signal.zone_score,
            signal.volume_surge_ratio,
            signal.price_change_pct,
            side_long,
        )
    }

    fn adjust_score(&self, base_score: f64, proba: f64) -> f64 {
        let threshold = self.config.read().unwrap().ml.supervised_threshold;
        if proba < threshold {
            let penalty = (threshold - proba) * 20.0;
            (base_score - penalty).max(0.0)
        } else {
            let boost = (proba - threshold) * 15.0;
            (base_score + boost).min(100.0)
        }
    }

    /// Predict win probability using the best available model, or None when
    /// neither model is ready yet.
    fn predict(&self, features: &[f64], klines: Option<&[KlineBar]>) -> Option<f64> {
        if !self.config.read().unwrap().ml.supervised_enabled {
            return None;
        }
        if self.online.is_ready(self.min_samples()) {
            return Some(self.online.predict_proba(features));
        }
        #[cfg(feature = "onnx")]
        {
            if let Some(ref clf) = self.classifier {
                let onnx_features = self.build_onnx_features(features, klines, clf.input_dim());
                match clf.predict_proba(&onnx_features) {
                    Ok(p) => return Some(p),
                    Err(exc) => warn!("ONNX predict failed: {exc}"),
                }
            }
        }
        None
    }

    /// Build the feature vector ONNX expects — legacy 10-dim models need absolute EMAs.
    fn build_onnx_features(
        &self,
        features: &[f64],
        klines: Option<&[KlineBar]>,
        input_dim: usize,
    ) -> Vec<f64> {
        if input_dim == LEGACY_ONNX_FEATURE_DIM {
            if let Some(k) = klines {
                return legacy_onnx_feature_vector(k, None);
            }
        }
        normalize_feature_vector(Some(features), input_dim)
    }

    pub fn enhance_signal(
        &mut self,
        signal: PumpSignal,
        klines: Option<&[KlineBar]>,
    ) -> Option<PumpSignal> {
        match self.enhance_signal_outcome(signal, klines) {
            EnhanceOutcome::Tradable(s) => Some(s),
            EnhanceOutcome::MlRejected(_) => None,
        }
    }

    /// Enrich a signal with ML features and classify whether it may trade or is
    /// shadow-only (ML gate reject).
    pub fn enhance_signal_outcome(
        &mut self,
        mut signal: PumpSignal,
        klines: Option<&[KlineBar]>,
    ) -> EnhanceOutcome {
        let cfg = self.config.read().unwrap().clone();
        if !cfg.ml.enabled {
            return EnhanceOutcome::Tradable(signal);
        }

        let features =
            normalize_feature_vector(Some(&self.build_features(&signal, klines)), FEATURE_DIM);
        signal.ml_features = features.clone();

        if let Some(p) = self.predict(&features, klines) {
            signal.setup_probability_pct = (p * 100.0 * 10.0).round() / 10.0;
            signal.composite_score = self.adjust_score(signal.composite_score, p);
            signal
                .message
                .push_str(&format!(" | ML win prob {:.0}%", signal.setup_probability_pct));

            if cfg.ml.hard_ml_gate && p < cfg.ml.supervised_threshold {
                debug!(
                    "{} blocked by hard ML gate (prob {:.1}% < {:.0}%) — shadow candidate",
                    signal.symbol,
                    signal.setup_probability_pct,
                    cfg.ml.supervised_threshold * 100.0
                );
                return EnhanceOutcome::MlRejected(signal);
            }
        }

        EnhanceOutcome::Tradable(signal)
    }

    /// Attach ML features and predicted probability without applying the hard gate.
    pub fn attach_features(
        &mut self,
        mut signal: PumpSignal,
        klines: Option<&[KlineBar]>,
    ) -> PumpSignal {
        if !self.config.read().unwrap().ml.enabled {
            return signal;
        }
        let features =
            normalize_feature_vector(Some(&self.build_features(&signal, klines)), FEATURE_DIM);
        signal.ml_features = features.clone();
        if let Some(p) = self.predict(&features, klines) {
            signal.setup_probability_pct = (p * 100.0 * 10.0).round() / 10.0;
        }
        signal
    }

    /// Learn from a resolved trade.
    /// `won` is true for a profitable trade. Returns the pre-update probability.
    pub fn record_outcome(&mut self, features: &[f64], won: bool) -> f64 {
        self.record_outcome_weighted(features, won, 1.0)
    }

    /// Weighted learn — `weight` scales the gradient step.
    /// Real trades use `ml.trade_win_weight` / `ml.trade_loss_weight`; shadow uses 1.0 or less.
    pub fn record_outcome_weighted(&mut self, features: &[f64], won: bool, weight: f64) -> f64 {
        let normalized = normalize_feature_vector(Some(features), FEATURE_DIM);
        self.online.update_weighted(&normalized, won, weight)
    }

    /// Learn from a resolved trade with a soft label (0.0–1.0).
    /// Useful for expired signals that are "probably not wins" but not definite losses.
    pub fn record_outcome_soft(&mut self, features: &[f64], label: f64, weight: f64) -> f64 {
        let normalized = normalize_feature_vector(Some(features), FEATURE_DIM);
        self.online.update_soft(&normalized, label, weight)
    }

    /// Stats augmented with gate-threshold accuracy for the Training screen.
    pub fn online_stats_with_threshold(&self) -> Value {
        let cfg = self.config.read().unwrap();
        let threshold = cfg.ml.supervised_threshold;
        let min = self.min_samples();
        self.online.stats_with_threshold(min, threshold)
    }
}
