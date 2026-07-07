//! ML inference + continuous learning.

use serde_json::{json, Value};
use tracing::debug;
#[cfg(feature = "onnx")]
use tracing::warn;

use crate::config::SharedAppConfig;
use crate::exchange::KlineBar;
#[cfg(feature = "onnx")]
use crate::ml::features::{legacy_onnx_feature_vector, LEGACY_ONNX_FEATURE_DIM};
use crate::ml::features::{normalize_feature_vector, MlFeatureContext, TechnicalFeatureBuilder, FEATURE_DIM};
use crate::ml::online::OnlineClassifier;
#[cfg(feature = "onnx")]
use crate::ml::onnx::OnnxClassifier;
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

    #[cfg(feature = "onnx")]
    fn onnx_loaded(&self) -> bool {
        self.classifier.is_some()
    }
    #[cfg(not(feature = "onnx"))]
    fn onnx_loaded(&self) -> bool {
        false
    }

    /// Re-load the ONNX classifier from its configured path. Called after an
    /// offline retrain (`scripts/export_onnx.py`) writes a fresh export so the
    /// live pipeline picks it up without a restart. No-op if `supervised_enabled`
    /// is off or the file is missing/invalid — the pipeline just keeps using
    /// whatever it already had loaded (or none).
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

    /// How much weight the online model gets in the online/ONNX blend, 0..1.
    /// Ramps from mostly-ONNX (untrained online model) to fully online once
    /// the online model has seen 3x the configured minimum sample count —
    /// by then it has adapted to live conditions the offline ONNX export
    /// (trained on a stale snapshot) cannot react to as quickly.
    fn online_blend_weight(&self) -> f64 {
        let maturity_target = (self.min_samples().max(1) as f64) * 3.0;
        (self.online.samples() as f64 / maturity_target).clamp(0.0, 1.0)
    }

    pub fn effective_threshold(&self) -> f64 {
        let cfg = self.config.read().unwrap();
        let floor = cfg.ml.supervised_threshold;
        if self.online.is_ready(self.min_samples()) {
            self.online.auto_threshold(floor)
        } else {
            floor
        }
    }

    pub fn status(&self) -> MlStatus {
        let cfg = self.config.read().unwrap();
        let online_ready = self.online.is_ready(self.min_samples());
        let hard = cfg.ml.hard_ml_gate || self.gate_auto_enabled;
        MlStatus {
            enabled: cfg.ml.enabled,
            supervised_enabled: cfg.ml.supervised_enabled,
            supervised_fitted: online_ready || self.onnx_loaded(),
            hard_ml_gate: hard,
            gate_auto_enabled: self.gate_auto_enabled,
            effective_threshold: self.effective_threshold(),
            supervised_threshold: cfg.ml.supervised_threshold,
            onnx_model_path: cfg.ml.onnx_model_path.clone(),
        }
    }

    pub fn learning_status(&self) -> Value {
        let cfg = self.config.read().unwrap();
        let online_ready = self.online.is_ready(self.min_samples());
        let eff = self.effective_threshold();
        let has_online = self.online.samples() > 0;
        let has_onnx = self.onnx_loaded();
        let (active, blend_weight) = match (has_online, has_onnx) {
            (true, true) => ("ensemble", self.online_blend_weight()),
            (true, false) => ("online", 1.0),
            (false, true) => ("onnx", 0.0),
            (false, false) => ("warming_up", 0.0),
        };
        json!({
            "enabled": cfg.ml.enabled,
            "supervised_enabled": cfg.ml.supervised_enabled,
            "hard_ml_gate": cfg.ml.hard_ml_gate || self.gate_auto_enabled,
            "gate_auto_enabled": self.gate_auto_enabled,
            "supervised_threshold": cfg.ml.supervised_threshold,
            "effective_threshold": eff,
            "active_model": active,
            "online_ready": online_ready,
            "onnx_loaded": has_onnx,
            "ensemble_online_weight": (blend_weight * 1000.0).round() / 1000.0,
            "kelly_fraction": cfg.ml.kelly_fraction,
            "avg_win_r": self.online.avg_win_r(),
            "avg_loss_r": self.online.avg_loss_r(),
            "auto_retrain_enabled": cfg.ml.auto_retrain_enabled,
            "online_model": self.online.stats_with_threshold(self.min_samples(), eff),
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

    /// Fractional-Kelly sizing: scales `suggested_risk_pct` and
    /// `suggested_leverage` by the model's edge, using the realized average
    /// win/loss R-multiples the online model tracks. Below threshold both are
    /// shrunk to the configured floor (mirrors the pre-Kelly behavior); at/above
    /// threshold the scale tracks `kelly_fraction x full-Kelly` so higher-EV
    /// setups get more size/leverage and marginal ones get proportionally less,
    /// always bounded by the existing `ml_risk_scale_min/max` safety envelope.
    fn apply_kelly_sizing(&self, signal: &mut PumpSignal, proba: f64) {
        let cfg = self.config.read().unwrap();
        let thresh = self.effective_threshold();
        let min_scale = cfg.ml.ml_risk_scale_min;
        let max_scale = cfg.ml.ml_risk_scale_max;

        let scale = if proba < thresh {
            min_scale
        } else {
            let win_r = self.online.avg_win_r();
            let loss_r = self.online.avg_loss_r();
            let odds = (win_r / loss_r).max(0.1);
            let kelly_full = (proba - (1.0 - proba) / odds).max(0.0);
            let target = (kelly_full * cfg.ml.kelly_fraction).clamp(0.0, 1.0);
            min_scale + target * (max_scale - min_scale)
        };

        signal.suggested_risk_pct *= scale;
        signal.suggested_leverage = ((signal.suggested_leverage as f64) * scale).round().max(1.0) as u32;
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

    /// Blend the fast-adapting online model with the batch-trained ONNX GBM
    /// into a single calibrated probability. When only one is available, its
    /// raw output is used as-is; when both are, the blend weight ramps from
    /// mostly-ONNX to mostly-online as the online model matures (see
    /// `online_blend_weight`) so the ensemble leans on whichever signal is
    /// more trustworthy at that point in the model's life.
    fn predict(&self, features: &[f64], klines: Option<&[KlineBar]>) -> Option<f64> {
        if !self.config.read().unwrap().ml.supervised_enabled {
            return None;
        }
        let online_pred = if self.online.samples() > 0 {
            Some(self.online.predict_proba(features))
        } else {
            None
        };
        let onnx_pred = self.onnx_predict(features, klines);

        match (online_pred, onnx_pred) {
            (Some(o), Some(x)) => {
                let w = self.online_blend_weight();
                Some(w * o + (1.0 - w) * x)
            }
            (Some(o), None) => Some(o),
            (None, Some(x)) => Some(x),
            (None, None) => None,
        }
    }

    #[cfg(feature = "onnx")]
    fn onnx_predict(&self, features: &[f64], klines: Option<&[KlineBar]>) -> Option<f64> {
        let clf = self.classifier.as_ref()?;
        let onnx_features = self.build_onnx_features(features, klines, clf.input_dim());
        match clf.predict_proba(&onnx_features) {
            Ok(p) => Some(p),
            Err(exc) => {
                warn!("ONNX predict failed: {exc}");
                None
            }
        }
    }
    #[cfg(not(feature = "onnx"))]
    fn onnx_predict(&self, _features: &[f64], _klines: Option<&[KlineBar]>) -> Option<f64> {
        None
    }

    #[cfg(feature = "onnx")]
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

        if let Some(p) = self.predict(&features, klines) {
            signal.setup_probability_pct = (p * 100.0 * 10.0).round() / 10.0;
            signal.composite_score = self.adjust_score(signal.composite_score, p);
            self.apply_kelly_sizing(&mut signal, p);
            signal.message.push_str(&format!(
                " | ML win prob {:.0}% (thr {:.0}%)",
                signal.setup_probability_pct,
                self.effective_threshold() * 100.0
            ));

            let paper_relax = !cfg.execution.live_trading_enabled && cfg.execution.paper_relax_gates;
            let hard = (cfg.ml.hard_ml_gate || self.gate_auto_enabled) && !paper_relax;
            let thresh = self.effective_threshold();
            if hard && p < thresh {
                debug!(
                    "{} blocked by ML gate (prob {:.1}% < {:.0}%)",
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
        if let Some(p) = self.predict(&features, klines) {
            signal.setup_probability_pct = (p * 100.0 * 10.0).round() / 10.0;
        }
        signal
    }

    pub fn record_outcome(&mut self, features: &[f64], won: bool) -> f64 {
        self.record_outcome_weighted(features, won, 1.0)
    }

    pub fn record_outcome_weighted(&mut self, features: &[f64], won: bool, weight: f64) -> f64 {
        let normalized = normalize_feature_vector(Some(features), FEATURE_DIM);
        self.online.update_weighted(&normalized, won, weight)
    }

    pub fn record_outcome_soft(&mut self, features: &[f64], label: f64, weight: f64) -> f64 {
        let normalized = normalize_feature_vector(Some(features), FEATURE_DIM);
        self.online.update_soft(&normalized, label, weight)
    }

    pub fn record_r_outcome(&mut self, r_multiple: f64, won: bool) {
        self.online.record_r_outcome(r_multiple, won);
    }

    /// Toggle auto gate based on rolling accuracy vs configured floors.
    pub fn evaluate_gate_auto(&mut self) -> Option<bool> {
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
        MlPipeline::new(Arc::new(RwLock::new(cfg)))
    }

    fn test_signal(composite_score: f64, zone_score: f64, volume_surge_ratio: f64, price_change_pct: f64) -> PumpSignal {
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

    /// Trains the online model on a clearly separable "strong setup" vs "weak
    /// setup" pattern (via the exact same feature builder the live pipeline
    /// uses) with a healthy win/loss R profile, then checks that a strong
    /// setup gets scaled-up risk/leverage via the Kelly-fraction sizing while
    /// a weak one gets shrunk to the configured floor.
    #[test]
    fn kelly_sizing_scales_with_edge() {
        let dir = tempdir().unwrap();
        let onnx_path = dir.path().join("supervised.onnx");
        let mut ml = test_pipeline(onnx_path.to_str().unwrap());
        let ctx = MlFeatureContext::default();

        let strong = test_signal(92.0, 85.0, 6.0, 0.04);
        let weak = test_signal(8.0, 5.0, 0.1, -0.001);
        let strong_features = ml.build_features(&strong, None, &ctx);
        let weak_features = ml.build_features(&weak, None, &ctx);

        for _ in 0..60 {
            ml.record_outcome_weighted(&strong_features, true, 1.0);
            ml.record_r_outcome(2.0, true);
            ml.record_outcome_weighted(&weak_features, false, 1.0);
            ml.record_r_outcome(-1.0, false);
        }
        assert!(ml.online_sample_count() >= 100);

        let strong_before_risk = strong.suggested_risk_pct;
        let strong_before_lev = strong.suggested_leverage;
        let strong_out = match ml.enhance_signal_outcome(strong, None, &ctx) {
            EnhanceOutcome::Tradable(s) => s,
            EnhanceOutcome::MlRejected(s) => s,
        };
        assert!(
            strong_out.setup_probability_pct > 50.0,
            "should favor the strong setup pattern, got {}",
            strong_out.setup_probability_pct
        );
        assert!(strong_out.suggested_risk_pct > 0.0);
        assert!(strong_out.suggested_risk_pct <= strong_before_risk);
        assert!(strong_out.suggested_leverage <= strong_before_lev);

        let weak_before_risk = weak.suggested_risk_pct;
        let weak_out = match ml.enhance_signal_outcome(weak, None, &ctx) {
            EnhanceOutcome::Tradable(s) => s,
            EnhanceOutcome::MlRejected(s) => s,
        };
        assert!(
            weak_out.setup_probability_pct < 50.0,
            "should disfavor the weak setup pattern, got {}",
            weak_out.setup_probability_pct
        );

        // The strong setup should always come out sized up relative to the weak one.
        assert!(strong_out.suggested_risk_pct > weak_out.suggested_risk_pct);
        assert!(strong_out.suggested_leverage >= weak_out.suggested_leverage);
        // Weak setup shrinks all the way to the configured floor scale.
        let default_cfg: AppConfig = serde_yaml::from_str(
            "mexc: {}\nscanner: {}\nzones: {}\ntrading: {}\nrisk: {}\nexecution: {}\nstorage: {}\nml: {}\nserver: {}\n",
        )
        .unwrap();
        let floor = default_cfg.ml.ml_risk_scale_min;
        assert!((weak_out.suggested_risk_pct - weak_before_risk * floor).abs() < 1e-6);
    }

    /// With only the online model trained (no ONNX file present), the ensemble
    /// should report "online" rather than "ensemble" or "warming_up".
    #[test]
    fn learning_status_reports_online_once_trained() {
        let dir = tempdir().unwrap();
        let onnx_path = dir.path().join("supervised.onnx");
        let mut ml = test_pipeline(onnx_path.to_str().unwrap());
        assert_eq!(ml.learning_status()["active_model"], "warming_up");

        let mut f = vec![0.0; FEATURE_DIM];
        f[0] = 1.0;
        ml.record_outcome_weighted(&f, true, 1.0);
        assert_eq!(ml.learning_status()["active_model"], "online");
    }

    /// Reloading against a missing ONNX file must not panic — the pipeline
    /// just ends up with no classifier, same as if it never had one.
    #[test]
    fn reload_onnx_missing_file_is_safe() {
        let dir = tempdir().unwrap();
        let onnx_path = dir.path().join("does_not_exist.onnx");
        let mut ml = test_pipeline(onnx_path.to_str().unwrap());
        ml.reload_onnx();
        assert!(!ml.onnx_loaded());
    }
}
