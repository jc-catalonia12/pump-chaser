//! ONNX supervised classifier inference via ONNX Runtime (ort).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ndarray::Array2;
use ort::session::Session;
use ort::value::TensorRef;
use tracing::{info, warn};

use crate::config::MlConfig;
use crate::error::{BotError, Result};
use crate::ml::features::{normalize_feature_vector, FEATURE_DIM};

#[derive(Clone)]
pub struct OnnxClassifier {
    session: Arc<Mutex<Session>>,
    path: PathBuf,
    /// Feature width expected by this ONNX graph (10 legacy or 15 current).
    input_dim: usize,
}

impl OnnxClassifier {
    pub fn try_load(config: &MlConfig) -> Option<Self> {
        let path = resolve_model_path(config);
        if !path.exists() {
            warn!(
                "ONNX model not found at {} — export with scripts/export_onnx.py",
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
                    "Failed to load ONNX model {}: {e} — re-export with scripts/export_onnx.py",
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

    /// Predict P(win) for a feature vector sized to this model's input width.
    pub fn predict_proba(&self, features: &[f64]) -> Result<f64> {
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

        extract_win_probability(&outputs)
    }
}

/// Read the fixed feature width from the ONNX graph input (e.g. `[None, 10]`).
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

fn extract_win_probability(outputs: &ort::session::SessionOutputs<'_>) -> Result<f64> {
    for (name, value) in outputs.iter() {
        let Ok(view) = value.try_extract_array::<f32>() else {
            continue;
        };
        let shape = view.shape();
        if shape.len() == 2 && shape[0] == 1 && shape[1] >= 2 {
            return Ok(view[[0, 1]] as f64);
        }
        if shape.len() == 1 && shape[0] >= 2 {
            return Ok(view[1] as f64);
        }
        if shape.len() == 2 && shape[0] == 1 && shape[1] == 1 {
            return Ok(view[[0, 0]] as f64);
        }
        if view.len() == 1 {
            let logit = view[0] as f64;
            return Ok(1.0 / (1.0 + (-logit).exp()));
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
    use crate::ml::features::LEGACY_ONNX_FEATURE_DIM;

    #[test]
    fn ort_loads_supervised_export() {
        let path = PathBuf::from("data/models/supervised.onnx");
        if !path.exists() {
            return;
        }
        let clf = OnnxClassifier::load(&path).expect("sklearn ONNX export should load via ONNX Runtime");
        assert!(
            clf.input_dim() == LEGACY_ONNX_FEATURE_DIM
                || clf.input_dim() == 15
                || clf.input_dim() == FEATURE_DIM
        );
        let sample = vec![0.0; clf.input_dim()];
        clf.predict_proba(&sample).expect("ONNX predict should succeed");
    }
}
