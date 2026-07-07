pub mod features;
pub mod inference;
pub mod labels;
pub mod online;
#[cfg(feature = "onnx")]
pub mod onnx;

pub use features::{MarketRegime, MlFeatureContext};
pub use inference::{EnhanceOutcome, MlPipeline, MlStatus};
pub use online::OnlineClassifier;

/// Resolve the ONNX export path from config. Kept feature-independent (unlike
/// `onnx::resolve_model_path`) so the offline retrain script can be pointed at
/// the right file even when the bot itself is built without the `onnx` feature.
pub fn onnx_model_path(cfg: &crate::config::MlConfig) -> std::path::PathBuf {
    match cfg.onnx_model_path {
        Some(ref p) => std::path::PathBuf::from(p),
        None => std::path::PathBuf::from("data/models/supervised.onnx"),
    }
}
