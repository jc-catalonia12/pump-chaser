//! ML inference — historical ONNX model is primary; online learning is opt-in.

use serde_json::{json, Value};
use tracing::debug;
#[cfg(feature = "onnx")]
use tracing::warn;

use crate::config::SharedAppConfig;
use crate::exchange::KlineBar;
use crate::ml::features::{normalize_feature_vector, MlFeatureContext, TechnicalFeatureBuilder, FEATURE_DIM};
use crate::ml::online::OnlineClassifier;
#[cfg(feature = "onnx")]
use crate::ml::onnx::{ClassProba, OnnxClassifier};
use crate::signals::PumpSignal;

/// Result of ML enrichment — distinguishes tradable signals from shadow-only rejects.
#[derive(Debug, Clone)]
pub enum EnhanceOutcome {
    Tradable(PumpSignal),
    MlRejected(PumpSignal),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MlStatus {
    pub enabled: bool,
    pub supervised_enabled: bool,
    pub supervised_fitted: bool,
    pub hard_ml_gate: bool,
    pub gate_auto_enabled: bool,
    pub effective_threshold: f64,
    pub supervised_threshold: f64,
    pub onnx_model_path: Option<String>,
}

pub struct MlPipeline {
    config: SharedAppConfig,
    #[cfg(feature = "onnx")]
    classifier: Option<OnnxClassifier>,
    online: OnlineClassifier,
    gate_auto_enabled: bool,
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
                gate_auto_enabled: false,
            }
        }
        #[cfg(not(feature = "onnx"))]
        {
            Self {
                config,
                online,
                gate_auto_enabled: false,
            }
        }
    }

    pub fn set_gate_auto_enabled(&mut self, enabled: bool) {
        self.gate_auto_enabled = enabled;
    }

    pub fn gate_auto_enabled(&self) -> bool {
        self.gate_auto_enabled
    }

    fn min_samples(&self) -> u64 {
        self.config.read().unwrap().ml.min_training_samples as u64
    }

    fn online_learning_enabled(&self) -> bool {
        self.config.read().unwrap().ml.online_learning_enabled
    }

    #[cfg(feature = "onnx")]
    fn onnx_loaded(&self) -> bool {
        self.classifier.is_some()
    }
    #[cfg(not(feature = "onnx"))]
    fn onnx_loaded(&self) -> bool {
        false
    }

    /// Hot-reload production ONNX after an offline historical retrain.
    #[cfg(feature = "onnx")]
    pub fn reload_onnx(&mut self) {
        let cfg = self.config.read().unwrap().ml.clone();
        self.classifier = if cfg.supervised_enabled {
            OnnxClassifier::try_load(&cfg)
        } else {
            None
        };
    }
    #[cfg(not(feature = "onnx"))]
    pub fn reload_onnx(&mut self) {}

    pub fn effective_threshold(&self) -> f64 {
        let cfg = self.config.read().unwrap();
        let floor = cfg.ml.supervised_threshold;
        if self.online_learning_enabled() && self.online.is_ready(self.min_samples()) {
            self.online.auto_threshold(floor)
        } else {
            floor
        }
    }

    pub fn status(&self) -> MlStatus {
        let cfg = self.config.read().unwrap();
        let hard = cfg.ml.hard_ml_gate || self.gate_auto_enabled;
        MlStatus {
            enabled: cfg.ml.enabled,
            supervised_enabled: cfg.ml.supervised_enabled,
            supervised_fitted: self.onnx_loaded()
                || (self.online_learning_enabled() && self.online.is_ready(self.min_samples())),
            hard_ml_gate: hard,
            gate_auto_enabled: self.gate_auto_enabled,
            effective_threshold: self.effective_threshold(),
            supervised_threshold: cfg.ml.supervised_threshold,
            onnx_model_path: cfg.ml.onnx_model_path.clone(),
        }
    }

    pub fn learning_status(&self) -> Value {
        let cfg = self.config.read().unwrap();
        let online_on = cfg.ml.online_learning_enabled;
        let online_ready = online_on && self.online.is_ready(self.min_samples());
        let eff = self.effective_threshold();
        let has_online = online_on && self.online.samples() > 0;
        let has_onnx = self.onnx_loaded();
        let active = match (has_onnx, has_online) {
            (true, true) => "onnx_primary",
            (true, false) => "onnx",
            (false, true) => "online",
            (false, false) => "warming_up",
        };
        json!({
            "enabled": cfg.ml.enabled,
            "supervised_enabled": cfg.ml.supervised_enabled,
            "online_learning_enabled": online_on,
            "hard_ml_gate": cfg.ml.hard_ml_gate || self.gate_auto_enabled,
            "gate_auto_enabled": self.gate_auto_enabled,
            "supervised_threshold": cfg.ml.supervised_threshold,
            "effective_threshold": eff,
            "active_model": active,
            "online_ready": online_ready,
            "onnx_loaded": has_onnx,
            "kelly_fraction": cfg.ml.kelly_fraction,
            "avg_win_r": self.online.avg_win_r(),
            "avg_loss_r": self.online.avg_loss_r(),
            "auto_retrain_enabled": cfg.ml.auto_retrain_enabled,
            "training_mode": "historical_candles",
            "online_model": if online_on {
                self.online.stats_with_threshold(self.min_samples(), eff)
            } else {
                json!({"disabled": true, "samples": 0})
            },
        })
    }

    pub fn online_stats(&self) -> Value {
        self.online_stats_with_threshold()
    }

    pub fn online_sample_count(&self) -> u64 {
        self.online.samples()
    }

    pub fn build_features(
        &self,
        signal: &PumpSignal,
        klines: Option<&[KlineBar]>,
        ctx: &MlFeatureContext,
    ) -> Vec<f64> {
        let side_long = signal.price_change_pct >= 0.0;
        TechnicalFeatureBuilder::signal_features(
            klines,
            signal.composite_score,
            signal.zone_score,
            signal.volume_surge_ratio,
            signal.price_change_pct,
            side_long,
            &signal.strategy,
            ctx,
        )
    }

    pub(crate) fn apply_kelly_sizing(&self, signal: &mut PumpSignal, proba: f64) {
        let cfg = self.config.read().unwrap();
        let thresh = self.effective_threshold();
        let min_scale = cfg.ml.ml_risk_scale_min;
        let max_scale = cfg.ml.ml_risk_scale_max;

        let scale = if proba < thresh {
            min_scale
        } else {
            let win_r = self.online.avg_win_r().max(1.5);
            let loss_r = self.online.avg_loss_r().max(1.0);
            let odds = (win_r / loss_r).max(0.1);
            let kelly_full = (proba - (1.0 - proba) / odds).max(0.0);
            let target = (kelly_full * cfg.ml.kelly_fraction).clamp(0.0, 1.0);
            min_scale + target * (max_scale - min_scale)
        };

        signal.suggested_risk_pct *= scale;
        signal.suggested_leverage =
            ((signal.suggested_leverage as f64) * scale).round().max(1.0) as u32;
    }

    fn adjust_score(&self, base_score: f64, proba: f64) -> f64 {
        let threshold = self.effective_threshold();
        if proba < threshold {
            let penalty = (threshold - proba) * 20.0;
            (base_score - penalty).max(0.0)
        } else {
            let boost = (proba - threshold) * 15.0;
            (base_score + boost).min(100.0)
        }
    }

    /// Primary prediction from historical ONNX; optional online blend only when enabled.
    fn predict_for_side(&self, features: &[f64], side_long: bool) -> Option<f64> {
        if !self.config.read().unwrap().ml.supervised_enabled {
            return None;
        }
        let onnx = self.onnx_class_proba(features);
        let online_pred = if self.online_learning_enabled() && self.online.samples() > 0 {
            Some(self.online.predict_proba(features))
        } else {
            None
        };

        match (onnx, online_pred) {
            (Some(cls), Some(o)) => {
                // Historical model stays primary (80%); online is a light adapt overlay.
                let side_p = cls.side_probability(side_long);
                Some(0.8 * side_p + 0.2 * o)
            }
            (Some(cls), None) => Some(cls.side_probability(side_long)),
            (None, Some(o)) => Some(o),
            (None, None) => None,
        }
    }

    #[cfg(feature = "onnx")]
    fn onnx_class_proba(&self, features: &[f64]) -> Option<ClassProba> {
        let clf = self.classifier.as_ref()?;
        let onnx_features = normalize_feature_vector(Some(features), clf.input_dim());
        match clf.predict_proba(&onnx_features) {
            Ok(p) => Some(p),
            Err(exc) => {
                warn!("ONNX predict failed: {exc}");
                None
            }
        }
    }
    #[cfg(not(feature = "onnx"))]
    fn onnx_class_proba(&self, _features: &[f64]) -> Option<()> {
        None
    }

    #[cfg(feature = "onnx")]
    fn onnx_prefers_no_trade(&self, features: &[f64]) -> bool {
        self.onnx_class_proba(features)
            .map(|p| p.prefers_no_trade())
            .unwrap_or(false)
    }
    #[cfg(not(feature = "onnx"))]
    fn onnx_prefers_no_trade(&self, _features: &[f64]) -> bool {
        false
    }

    pub fn enhance_signal(
        &mut self,
        signal: PumpSignal,
        klines: Option<&[KlineBar]>,
        ctx: &MlFeatureContext,
    ) -> Option<PumpSignal> {
        match self.enhance_signal_outcome(signal, klines, ctx) {
            EnhanceOutcome::Tradable(s) => Some(s),
            EnhanceOutcome::MlRejected(_) => None,
        }
    }

    pub fn enhance_signal_outcome(
        &mut self,
        mut signal: PumpSignal,
        klines: Option<&[KlineBar]>,
        ctx: &MlFeatureContext,
    ) -> EnhanceOutcome {
        let cfg = self.config.read().unwrap().clone();
        if !cfg.ml.enabled {
            return EnhanceOutcome::Tradable(signal);
        }

        let features =
            normalize_feature_vector(Some(&self.build_features(&signal, klines, ctx)), FEATURE_DIM);
        signal.ml_features = features.clone();
        let side_long = signal.price_change_pct >= 0.0;

        if let Some(p) = self.predict_for_side(&features, side_long) {
            signal.setup_probability_pct = (p * 100.0 * 10.0).round() / 10.0;
            signal.composite_score = self.adjust_score(signal.composite_score, p);
            self.apply_kelly_sizing(&mut signal, p);
            signal.message.push_str(&format!(
                " | ML {} prob {:.0}% (thr {:.0}%)",
                if side_long { "LONG" } else { "SHORT" },
                signal.setup_probability_pct,
                self.effective_threshold() * 100.0
            ));

            let paper_relax = !cfg.execution.live_trading_enabled && cfg.execution.paper_relax_gates;
            let hard = (cfg.ml.hard_ml_gate || self.gate_auto_enabled) && !paper_relax;
            let thresh = self.effective_threshold();
            let no_trade = self.onnx_prefers_no_trade(&features);
            if hard && (p < thresh || no_trade) {
                debug!(
                    "{} blocked by ML gate (prob {:.1}% < {:.0}% or NO_TRADE)",
                    signal.symbol,
                    signal.setup_probability_pct,
                    thresh * 100.0
                );
                return EnhanceOutcome::MlRejected(signal);
            }
        }

        EnhanceOutcome::Tradable(signal)
    }

    pub fn attach_features(
        &mut self,
        mut signal: PumpSignal,
        klines: Option<&[KlineBar]>,
        ctx: &MlFeatureContext,
    ) -> PumpSignal {
        if !self.config.read().unwrap().ml.enabled {
            return signal;
        }
        let features =
            normalize_feature_vector(Some(&self.build_features(&signal, klines, ctx)), FEATURE_DIM);
        signal.ml_features = features.clone();
        let side_long = signal.price_change_pct >= 0.0;
        if let Some(p) = self.predict_for_side(&features, side_long) {
            signal.setup_probability_pct = (p * 100.0 * 10.0).round() / 10.0;
        }
        signal
    }

    pub fn record_outcome(&mut self, features: &[f64], won: bool) -> f64 {
        self.record_outcome_weighted(features, won, 1.0)
    }

    pub fn record_outcome_weighted(&mut self, features: &[f64], won: bool, weight: f64) -> f64 {
        if !self.online_learning_enabled() {
            return 0.0;
        }
        let normalized = normalize_feature_vector(Some(features), FEATURE_DIM);
        self.online.update_weighted(&normalized, won, weight)
    }

    pub fn record_outcome_soft(&mut self, features: &[f64], label: f64, weight: f64) -> f64 {
        if !self.online_learning_enabled() {
            return 0.0;
        }
        let normalized = normalize_feature_vector(Some(features), FEATURE_DIM);
        self.online.update_soft(&normalized, label, weight)
    }

    pub fn record_r_outcome(&mut self, r_multiple: f64, won: bool) {
        if !self.online_learning_enabled() {
            return;
        }
        self.online.record_r_outcome(r_multiple, won);
    }

    pub fn evaluate_gate_auto(&mut self) -> Option<bool> {
        if !self.online_learning_enabled() {
            return None;
        }
        let cfg = self.config.read().unwrap().clone();
        if self.online.samples() < cfg.ml.gate_auto_min_samples as u64 {
            return None;
        }
        let thresh = self.effective_threshold();
        let acc = self.online.accuracy_at_threshold(thresh)?;
        let prev = self.gate_auto_enabled;
        if !prev && acc >= cfg.ml.gate_min_accuracy {
            self.gate_auto_enabled = true;
            return Some(true);
        }
        if prev && acc < cfg.ml.gate_disable_accuracy {
            self.gate_auto_enabled = false;
            return Some(false);
        }
        None
    }

    pub fn online_stats_with_threshold(&self) -> Value {
        let threshold = self.effective_threshold();
        let min = self.min_samples();
        self.online.stats_with_threshold(min, threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::signals::SignalStrength;
    use std::sync::{Arc, RwLock};
    use tempfile::tempdir;

    fn test_pipeline(onnx_path: &str) -> MlPipeline {
        let yaml = "mexc: {}\nscanner: {}\nzones: {}\ntrading: {}\nrisk: {}\nexecution: {}\nstorage: {}\nml: {}\nserver: {}\n";
        let mut cfg: AppConfig = serde_yaml::from_str(yaml).expect("test config");
        cfg.ml.onnx_model_path = Some(onnx_path.to_string());
        cfg.ml.min_training_samples = 20;
        cfg.ml.online_learning_enabled = true; // allow Kelly tests to train online
        MlPipeline::new(Arc::new(RwLock::new(cfg)))
    }

    fn test_signal(
        composite_score: f64,
        zone_score: f64,
        volume_surge_ratio: f64,
        price_change_pct: f64,
    ) -> PumpSignal {
        PumpSignal {
            symbol: "TEST_USDT".into(),
            strategy: "ai".into(),
            composite_score,
            strength: SignalStrength::Moderate,
            last_price: 10.0,
            price_change_pct,
            volume_surge_ratio,
            confluence_count: 0,
            confluences: vec![],
            confluence_details: vec![],
            setup_probability_pct: 0.0,
            suggested_risk_pct: 1.0,
            suggested_leverage: 10,
            zone_score,
            zone_message: String::new(),
            sizing_tier: String::new(),
            message: String::new(),
            generated_at: chrono::Utc::now(),
            signal_id: None,
            projected_stop_loss: 9.8,
            projected_take_profits: vec![10.4],
            tp_close_fractions: vec![1.0],
            ml_features: vec![],
            entry_mode: "market".into(),
            limit_entry_price: 0.0,
            expected_value_r: 0.0,
            reward_risk: 0.0,
            decision_reason: String::new(),
        }
    }

    #[test]
    fn kelly_sizing_scales_with_edge() {
        let dir = tempdir().unwrap();
        let onnx_path = dir.path().join("production.onnx");
        let mut ml = test_pipeline(onnx_path.to_str().unwrap());

        let mut strong_features = vec![0.0; FEATURE_DIM];
        strong_features[0] = 1.0;
        strong_features[4] = 0.8;
        let mut weak_features = vec![0.0; FEATURE_DIM];
        weak_features[0] = -1.0;
        weak_features[4] = 0.2;

        for _ in 0..60 {
            ml.record_outcome_weighted(&strong_features, true, 1.0);
            ml.record_r_outcome(2.0, true);
            ml.record_outcome_weighted(&weak_features, false, 1.0);
            ml.record_r_outcome(-1.0, false);
        }
        assert!(ml.online_sample_count() >= 100);

        let mut s = test_signal(92.0, 85.0, 6.0, 0.04);
        ml.apply_kelly_sizing(&mut s, 0.85);
        let mut w = test_signal(8.0, 5.0, 0.1, -0.001);
        ml.apply_kelly_sizing(&mut w, 0.20);
        assert!(s.suggested_risk_pct >= w.suggested_risk_pct);
        assert!(s.suggested_leverage >= w.suggested_leverage);
    }

    #[test]
    fn learning_status_reports_online_once_trained() {
        let dir = tempdir().unwrap();
        let onnx_path = dir.path().join("production.onnx");
        let mut ml = test_pipeline(onnx_path.to_str().unwrap());
        assert_eq!(ml.learning_status()["active_model"], "warming_up");

        let mut f = vec![0.0; FEATURE_DIM];
        f[0] = 1.0;
        ml.record_outcome_weighted(&f, true, 1.0);
        assert_eq!(ml.learning_status()["active_model"], "online");
    }

    #[test]
    fn online_learning_disabled_skips_updates() {
        let dir = tempdir().unwrap();
        let onnx_path = dir.path().join("production.onnx");
        let yaml = "mexc: {}\nscanner: {}\nzones: {}\ntrading: {}\nrisk: {}\nexecution: {}\nstorage: {}\nml: {}\nserver: {}\n";
        let mut cfg: AppConfig = serde_yaml::from_str(yaml).unwrap();
        cfg.ml.onnx_model_path = Some(onnx_path.to_str().unwrap().to_string());
        cfg.ml.online_learning_enabled = false;
        let mut ml = MlPipeline::new(Arc::new(RwLock::new(cfg)));
        let f = vec![1.0; FEATURE_DIM];
        ml.record_outcome_weighted(&f, true, 1.0);
        assert_eq!(ml.online_sample_count(), 0);
        assert_eq!(ml.learning_status()["active_model"], "warming_up");
    }

    #[test]
    fn reload_onnx_missing_file_is_safe() {
        let dir = tempdir().unwrap();
        let onnx_path = dir.path().join("does_not_exist.onnx");
        let mut ml = test_pipeline(onnx_path.to_str().unwrap());
        ml.reload_onnx();
        assert!(!ml.onnx_loaded());
    }
}
