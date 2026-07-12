"""Export trained multi-class models to ONNX."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from training.schema import FEATURE_DIM


def export_model_to_onnx(model: Any, out_path: Path) -> Path:
    """Export sklearn or LightGBM classifier to ONNX (input name: `input`)."""
    out_path = Path(out_path)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    model_name = type(model).__name__.lower()
    if "lgbm" in model_name or "lightgbm" in model_name:
        return _export_lightgbm(model, out_path)
    return _export_sklearn(model, out_path)


def _export_sklearn(model: Any, out_path: Path) -> Path:
    from skl2onnx import convert_sklearn
    from skl2onnx.common.data_types import FloatTensorType

    onnx_model = convert_sklearn(
        model,
        initial_types=[("input", FloatTensorType([None, FEATURE_DIM]))],
        options={id(model): {"zipmap": False}},
    )
    with open(out_path, "wb") as f:
        f.write(onnx_model.SerializeToString())
    return out_path


def _export_lightgbm(model: Any, out_path: Path) -> Path:
    """Export LightGBM with dense probability tensor (no ZipMap) for Rust ort."""
    from onnxmltools import convert_lightgbm
    from onnxmltools.convert.common.data_types import FloatTensorType

    booster = model.booster_ if hasattr(model, "booster_") else model
    onnx_model = convert_lightgbm(
        booster,
        initial_types=[("input", FloatTensorType([None, FEATURE_DIM]))],
        target_opset=12,
        zipmap=False,
    )
    with open(out_path, "wb") as f:
        f.write(onnx_model.SerializeToString())
    return out_path


def verify_onnx(path: Path, feature_dim: int = FEATURE_DIM) -> dict:
    """Smoke-test ONNX Runtime inference."""
    import numpy as np
    import onnxruntime as ort

    sess = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    inp = sess.get_inputs()[0]
    name = inp.name
    x = np.zeros((1, feature_dim), dtype=np.float32)
    outs = sess.run(None, {name: x})
    shapes = [getattr(o, "shape", None) for o in outs]
    return {"input_name": name, "output_shapes": [list(s) if s is not None else None for s in shapes]}
