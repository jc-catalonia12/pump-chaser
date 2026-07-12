//! ONNX multi-class classifier inference via ONNX Runtime (ort).
//!
//! Classes (must match `training/schema.py`):
//!   0 = NO_TRADE, 1 = LONG, 2 = SHORT

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ndarray::Array2;
use ort::session::Session;
use ort::value::TensorRef;
use tracing::{info, warn};

use crate::config::MlConfig;
use crate::error::{BotError, Result};
use crate::ml::features::{normalize_feature_vector, FEATURE_DIM};

/// Probabilities for the three target classes.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClassProba {
    pub no_trade: f64,
    pub long: f64,
    pub short: f64,
}

impl ClassProba {
    /// Probability of the actionable side matching `side_long`.
    pub fn side_probability(&self, side_long: bool) -> f64 {
        if side_long {
            self.long
        } else {
            self.short
        }
    }

    /// Best actionable class probability (max of LONG/SHORT).
    pub fn best_trade_probability(&self) -> f64 {
        self.long.max(self.short)
    }

    pub fn prefers_no_trade(&self) -> bool {
        self.no_trade >= self.long && self.no_trade >= self.short
    }
}

#[derive(Clone)]
pub struct OnnxClassifier {
    session: Arc<Mutex<Session>>,
    path: PathBuf,
    input_dim: usize,
}

impl OnnxClassifier {
    pub fn try_load(config: &MlConfig) -> Option<Self> {
        let path = resolve_model_path(config);
        if !path.exists() {
            warn!(
                "ONNX model not found at {} — train with: python -m training pipeline",
                path.display()
            );
            return None;
        }
        match Self::load(&path) {
            Ok(m) => {
                info!(
                    "Loaded ONNX classifier from {} (input_dim={})",
                    path.display(),
                    m.input_dim
                );
                Some(m)
            }
            Err(e) => {
                warn!(
                    "Failed to load ONNX model {}: {e} — retrain with python -m training",
                    path.display()
                );
                None
            }
        }
    }

    fn load(path: &Path) -> Result<Self> {
        let mut builder = Session::builder().map_err(|e| BotError::Ml(e.to_string()))?;
        let session = builder
            .commit_from_file(path)
            .map_err(|e| BotError::Ml(e.to_string()))?;
        let input_dim = session_input_feature_dim(&session);
        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            path: path.to_path_buf(),
            input_dim,
        })
    }

    pub fn is_loaded(&self) -> bool {
        true
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn input_dim(&self) -> usize {
        self.input_dim
    }

    /// Predict class probabilities for a feature vector.
    pub fn predict_proba(&self, features: &[f64]) -> Result<ClassProba> {
        let vec = normalize_feature_vector(Some(features), self.input_dim);
        let input_f32: Vec<f32> = vec.iter().map(|v| *v as f32).collect();
        let input = Array2::from_shape_vec((1, self.input_dim), input_f32)
            .map_err(|e| BotError::Ml(e.to_string()))?;
        let tensor = TensorRef::from_array_view(input.view())
            .map_err(|e| BotError::Ml(e.to_string()))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| BotError::Ml("ONNX session lock poisoned".into()))?;
        let outputs = session
            .run(ort::inputs![tensor])
            .map_err(|e| BotError::Ml(e.to_string()))?;

        extract_class_proba(&outputs)
    }

    /// Back-compat: P(win-like) ≈ max(P(LONG), P(SHORT)).
    pub fn predict_trade_probability(&self, features: &[f64]) -> Result<f64> {
        Ok(self.predict_proba(features)?.best_trade_probability())
    }
}

fn session_input_feature_dim(session: &Session) -> usize {
    session
        .inputs()
        .first()
        .and_then(|out| out.dtype().tensor_shape())
        .and_then(|shape| {
            let dims: Vec<i64> = shape.iter().copied().collect();
            if dims.len() >= 2 && dims[1] > 0 {
                Some(dims[1] as usize)
            } else {
                None
            }
        })
        .unwrap_or(FEATURE_DIM)
}

fn extract_class_proba(outputs: &ort::session::SessionOutputs<'_>) -> Result<ClassProba> {
    for (name, value) in outputs.iter() {
        let Ok(view) = value.try_extract_array::<f32>() else {
            continue;
        };
        let shape = view.shape();

        // Preferred: [1, 3] probabilities for NO_TRADE / LONG / SHORT
        if shape.len() == 2 && shape[0] == 1 && shape[1] >= 3 {
            return Ok(ClassProba {
                no_trade: view[[0, 0]] as f64,
                long: view[[0, 1]] as f64,
                short: view[[0, 2]] as f64,
            });
        }
        if shape.len() == 1 && shape[0] >= 3 {
            return Ok(ClassProba {
                no_trade: view[0] as f64,
                long: view[1] as f64,
                short: view[2] as f64,
            });
        }
        // Binary legacy: [1, 2] → treat class1 as tradeable long-ish
        if shape.len() == 2 && shape[0] == 1 && shape[1] == 2 {
            let p = view[[0, 1]] as f64;
            return Ok(ClassProba {
                no_trade: 1.0 - p,
                long: p,
                short: 0.0,
            });
        }
        if shape.len() == 2 && shape[0] == 1 && shape[1] == 1 {
            let p = view[[0, 0]] as f64;
            return Ok(ClassProba {
                no_trade: 1.0 - p,
                long: p,
                short: 0.0,
            });
        }
        tracing::debug!("ONNX output '{name}' shape {:?} — skipped", shape);
    }
    Err(BotError::Ml("unexpected ONNX output shape".into()))
}

pub fn resolve_model_path(config: &MlConfig) -> PathBuf {
    crate::ml::onnx_model_path(config)
}

#[cfg(all(test, feature = "onnx"))]
mod tests {
    use super::*;

    #[test]
    fn ort_loads_production_export() {
        for candidate in [
            PathBuf::from("models/production.onnx"),
            PathBuf::from("data/models/production.onnx"),
            PathBuf::from("data/models/supervised.onnx"),
        ] {
            if !candidate.exists() {
                continue;
            }
            let clf = OnnxClassifier::load(&candidate)
                .expect("historical ONNX export should load via ONNX Runtime");
            assert!(clf.input_dim() == FEATURE_DIM || clf.input_dim() > 0);
            let sample = vec![0.0; clf.input_dim()];
            let _ = clf.predict_proba(&sample).expect("ONNX predict should succeed");
            return;
        }
    }
}
