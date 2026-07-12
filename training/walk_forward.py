"""Walk-forward validation and candidate model export."""

from __future__ import annotations

import json
from datetime import datetime, timezone

import numpy as np

from training.export_onnx import export_model_to_onnx, verify_onnx
from training.paths import (
    CANDIDATE_METRICS,
    CANDIDATE_ONNX,
    TRAINING_PARQUET,
    ensure_dirs,
)
from training.schema import FEATURE_COLUMNS
from training.train import (
    DEFAULT_CONFIDENCE_THRESHOLD,
    apply_confidence_gate,
    compute_metrics,
    score_promotion,
    train_classifier,
    train_final_model,
    train_sklearn_export_model,
)


def walk_forward_folds(
    timestamps: np.ndarray,
    n_folds: int = 4,
    train_ratio: float = 0.7,
    *,
    rolling: bool = True,
) -> list[tuple[np.ndarray, np.ndarray]]:
    """Time-ordered folds. Prefer rolling windows for non-stationary crypto."""
    n = len(timestamps)
    if n < 500:
        cut = int(n * train_ratio)
        idx = np.arange(n)
        return [(idx[:cut], idx[cut:])]

    order = np.argsort(timestamps)
    chunk = n // (n_folds + 1)
    folds: list[tuple[np.ndarray, np.ndarray]] = []
    for k in range(n_folds):
        test_start = chunk * (k + 1)
        test_end = chunk * (k + 2)
        if k == n_folds - 1:
            test_end = n
        if rolling:
            # Train only on the preceding ~2 chunks (recent regime).
            train_start = max(0, test_start - 2 * chunk)
            train_idx = order[train_start:test_start]
        else:
            train_idx = order[:test_start]
        test_idx = order[test_start:test_end]
        if len(train_idx) < 100 or len(test_idx) < 50:
            continue
        folds.append((train_idx, test_idx))
    return folds


def run_walk_forward(
    n_folds: int = 4,
    promote: bool = False,
    confidence_threshold: float | None = None,
) -> dict:
    """Train across walk-forward folds, export candidate ONNX, optionally promote."""
    ensure_dirs()
    if not TRAINING_PARQUET.exists():
        raise SystemExit(f"Missing {TRAINING_PARQUET}")

    from training.paths import read_frame

    ds = read_frame(TRAINING_PARQUET)
    x = ds[FEATURE_COLUMNS]
    y = ds["Target"].astype(int)
    ts = ds["timestamp"].to_numpy(dtype=np.int64)

    folds = walk_forward_folds(ts, n_folds=n_folds)
    if not folds:
        raise SystemExit("Not enough data for walk-forward folds")

    fold_metrics: list[dict] = []
    thresholds: list[float] = []
    base_thr = (
        float(confidence_threshold)
        if confidence_threshold is not None
        else DEFAULT_CONFIDENCE_THRESHOLD
    )

    for i, (tr, te) in enumerate(folds):
        if len(np.unique(y.iloc[tr])) < 2:
            print(f"Fold {i}: skip — train set has <2 classes")
            continue
        tm = train_classifier(
            x.iloc[tr],
            y.iloc[tr],
            x.iloc[te],
            y.iloc[te],
            confidence_threshold=base_thr,
            tune_threshold=True,
        )
        tm.metrics["fold"] = float(i)
        fold_metrics.append(tm.metrics)
        thresholds.append(float(tm.confidence_threshold))
        print(
            f"Fold {i}: train={len(tr)} test={len(te)} "
            f"thr={tm.confidence_threshold:.2f} "
            f"f1={tm.metrics.get('f1_macro', 0):.3f} "
            f"prec_trade={tm.metrics.get('precision_tradeable', 0):.3f} "
            f"avg_r={tm.metrics.get('avg_r_proxy', 0):.3f} "
            f"trade_rate={tm.metrics.get('tradeable_rate', 0):.3f}"
        )

    if not fold_metrics:
        raise SystemExit("No successful walk-forward folds")

    # Consensus threshold: prefer high selective gate (paper quality over activity).
    if thresholds:
        chosen_thr = float(max(np.median(thresholds), 0.60))
    else:
        chosen_thr = max(base_thr, 0.60)
    final_tr, final_te = folds[-1]
    print(
        f"Exporting ONNX candidate (prefer LightGBM) with confidence_threshold={chosen_thr:.2f}..."
    )
    export_model, export_backend = train_final_model(x.iloc[final_tr], y.iloc[final_tr])

    # Score final model on last fold with fixed chosen threshold (no re-tune leak).
    final_proba = export_model.predict_proba(x.iloc[final_te])
    final_pred = apply_confidence_gate(final_proba, chosen_thr)
    final_metrics = compute_metrics(
        y.iloc[final_te].to_numpy(), final_pred, final_proba
    )
    final_metrics["confidence_threshold"] = chosen_thr
    print(
        f"Final holdout: prec_trade={final_metrics.get('precision_tradeable', 0):.3f} "
        f"avg_r={final_metrics.get('avg_r_proxy', 0):.3f} "
        f"trade_rate={final_metrics.get('tradeable_rate', 0):.3f}"
    )

    keys = [
        "accuracy",
        "f1_macro",
        "precision_macro",
        "recall_macro",
        "precision_tradeable",
        "precision_long",
        "precision_short",
        "tradeable_rate",
        "avg_r_proxy",
        "net_r_proxy",
        "mean_confidence",
        "mean_trade_confidence",
        "ungated_precision_tradeable",
        "ungated_avg_r_proxy",
        "ungated_tradeable_rate",
        "confidence_threshold",
    ]
    agg: dict[str, float] = {}
    for k in keys:
        vals = [m[k] for m in fold_metrics if k in m]
        if vals:
            agg[k] = float(np.mean(vals))
    # Prefer the consensus threshold + blend fold avg with final holdout quality.
    agg["confidence_threshold"] = chosen_thr
    for k in (
        "precision_tradeable",
        "avg_r_proxy",
        "tradeable_rate",
        "f1_macro",
        "precision_long",
        "precision_short",
    ):
        if k in final_metrics and k in agg:
            agg[k] = float(0.5 * agg[k] + 0.5 * final_metrics[k])
        elif k in final_metrics:
            agg[k] = float(final_metrics[k])

    agg["n_folds"] = float(len(fold_metrics))
    agg["n_test"] = float(sum(m.get("n_test", 0) for m in fold_metrics))
    agg["model_name"] = export_backend
    agg["export_backend"] = export_backend
    agg["trained_at"] = datetime.now(timezone.utc).isoformat()
    agg["dataset_rows"] = float(len(ds))
    agg["feature_dim"] = float(len(FEATURE_COLUMNS))
    agg["final_holdout_precision_tradeable"] = float(
        final_metrics.get("precision_tradeable", 0)
    )
    agg["final_holdout_avg_r_proxy"] = float(final_metrics.get("avg_r_proxy", 0))

    try:
        export_model_to_onnx(export_model, CANDIDATE_ONNX)
        verify_onnx(CANDIDATE_ONNX)
    except Exception as exc:  # noqa: BLE001
        print(f"LightGBM ONNX export failed ({exc}); falling back to sklearn GBM")
        export_model = train_sklearn_export_model(
            x.iloc[final_tr].to_numpy(dtype=np.float64),
            y.iloc[final_tr].to_numpy(dtype=np.int32),
        )
        export_model_to_onnx(export_model, CANDIDATE_ONNX)
        verify_onnx(CANDIDATE_ONNX)
        agg["export_backend"] = "sklearn_gbm_fallback"
        agg["model_name"] = "sklearn_gbm_fallback"

    CANDIDATE_METRICS.write_text(
        json.dumps({"aggregate": agg, "folds": fold_metrics}, indent=2)
    )
    print(f"Wrote candidate model → {CANDIDATE_ONNX}")
    print(f"Wrote candidate metrics → {CANDIDATE_METRICS}")

    from training.registry import load_production_metrics, promote_candidate

    prod = load_production_metrics()
    should = score_promotion(agg, prod)
    result = {
        "metrics": agg,
        "promoted": False,
        "would_promote": should,
        "production_metrics": prod,
        "recommended_supervised_threshold": chosen_thr,
    }
    if promote:
        if should:
            promote_candidate(
                notes=f"walk_forward auto-promote thr={chosen_thr:.2f} backend={agg.get('export_backend')}"
            )
            result["promoted"] = True
            print("PROMOTED candidate → production")
            _maybe_update_settings_threshold(chosen_thr)
        else:
            print("NOT promoted — candidate did not beat production gates")
    return result


def _maybe_update_settings_threshold(threshold: float) -> None:
    """Keep live/paper gate aligned with the tuned training threshold."""
    from pathlib import Path

    settings = Path(__file__).resolve().parent.parent / "config" / "settings.yaml"
    if not settings.exists():
        return
    text = settings.read_text()
    import re

    new_text, n = re.subn(
        r"(supervised_threshold:\s*)([0-9.]+)",
        rf"\g<1>{threshold:.2f}",
        text,
        count=1,
    )
    if n:
        settings.write_text(new_text)
        print(f"Updated config supervised_threshold → {threshold:.2f}")
