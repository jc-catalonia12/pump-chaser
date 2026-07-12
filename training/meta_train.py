"""Binary meta-label training: will the momentum primary setup win?

Exports a 3-class ONNX model compatible with Rust:
  NO_TRADE / LONG / SHORT
constructed from primary side × P(win).
"""

from __future__ import annotations

import json
from datetime import datetime, timezone

import numpy as np
import pandas as pd

from training.export_onnx import export_model_to_onnx, verify_onnx
from training.paths import (
    CANDIDATE_METRICS,
    CANDIDATE_ONNX,
    TRAINING_PARQUET,
    ensure_dirs,
    read_frame,
)
from training.schema import FEATURE_COLUMNS, LABEL_LONG, LABEL_NO_TRADE, LABEL_SHORT
from training.train import (
    DEFAULT_CONFIDENCE_THRESHOLD,
    compute_metrics,
    score_promotion,
    train_sklearn_export_model,
)
from training.walk_forward import walk_forward_folds


def _primary_side(df: pd.DataFrame) -> np.ndarray:
    e20 = df["ema20_ratio"].to_numpy()
    e50 = df["ema50_ratio"].to_numpy()
    side = np.zeros(len(df), dtype=np.int32)
    side[(e20 < 0) & (e50 < e20)] = LABEL_LONG
    side[(e20 > 0) & (e50 > e20)] = LABEL_SHORT
    return side


def _binary_from_target(y: np.ndarray) -> np.ndarray:
    return (y != LABEL_NO_TRADE).astype(np.int32)


def _three_class_from_binary(
    side: np.ndarray,
    p_win: np.ndarray,
    threshold: float,
) -> tuple[np.ndarray, np.ndarray]:
    """Build preds + synthetic 3-class proba from P(win) and primary side."""
    n = len(side)
    proba = np.zeros((n, 3), dtype=np.float64)
    # P(NO_TRADE)=1-p_win, assign p_win to the primary side.
    proba[:, 0] = 1.0 - p_win
    long_mask = side == LABEL_LONG
    short_mask = side == LABEL_SHORT
    proba[long_mask, 1] = p_win[long_mask]
    proba[short_mask, 2] = p_win[short_mask]
    # Rows without primary → always NO_TRADE
    none = side == LABEL_NO_TRADE
    proba[none, 0] = 1.0
    proba[none, 1] = 0.0
    proba[none, 2] = 0.0

    pred = np.full(n, LABEL_NO_TRADE, dtype=np.int32)
    take = (p_win >= threshold) & ~none
    pred[take & long_mask] = LABEL_LONG
    pred[take & short_mask] = LABEL_SHORT
    return pred, proba


def _tune_binary_threshold(
    y_true: np.ndarray,
    side: np.ndarray,
    p_win: np.ndarray,
    *,
    min_thr: float = 0.55,
    max_thr: float = 0.90,
    step: float = 0.02,
    min_rate: float = 0.02,
) -> tuple[float, dict]:
    best_thr = max(DEFAULT_CONFIDENCE_THRESHOLD, 0.60)
    pred0, proba0 = _three_class_from_binary(side, p_win, best_thr)
    best_m = compute_metrics(y_true, pred0, proba0)
    best_score = (best_m.get("avg_r_proxy", -999), best_m.get("precision_tradeable", 0))

    thr = min_thr
    while thr <= max_thr + 1e-9:
        pred, proba = _three_class_from_binary(side, p_win, thr)
        m = compute_metrics(y_true, pred, proba)
        if m.get("tradeable_rate", 0) < min_rate:
            thr = round(thr + step, 4)
            continue
        score = (m.get("avg_r_proxy", -999), m.get("precision_tradeable", 0))
        if score > best_score:
            best_score = score
            best_thr = thr
            best_m = m
        thr = round(thr + step, 4)
    best_m = dict(best_m)
    best_m["confidence_threshold"] = float(best_thr)
    return float(best_thr), best_m


def _fit_binary(x_train, y_bin_train, x_val=None, y_bin_val=None):
    try:
        import lightgbm as lgb

        model = lgb.LGBMClassifier(
            n_estimators=500,
            learning_rate=0.03,
            max_depth=5,
            num_leaves=31,
            min_child_samples=50,
            subsample=0.8,
            colsample_bytree=0.8,
            reg_alpha=0.2,
            reg_lambda=1.0,
            class_weight="balanced",
            random_state=42,
            verbosity=-1,
        )
        kwargs = {}
        if x_val is not None and y_bin_val is not None and len(y_bin_val) >= 50:
            kwargs["eval_set"] = [(x_val, y_bin_val)]
            kwargs["callbacks"] = [
                lgb.early_stopping(60, verbose=False),
                lgb.log_evaluation(period=0),
            ]
        model.fit(x_train, y_bin_train, **kwargs)
        return model, "lightgbm"
    except Exception:
        from sklearn.ensemble import GradientBoostingClassifier

        model = GradientBoostingClassifier(
            n_estimators=200, learning_rate=0.05, max_depth=3, random_state=42
        )
        model.fit(x_train, y_bin_train)
        return model, "sklearn_gbm"


class _MetaThreeClassWrapper:
    """Sklearn-like wrapper so ONNX export can use a real 3-class GBM distilled
    from meta predictions, OR we distill by fitting GBM on soft targets.

    For ONNX we distill: train a 3-class GBM on (X, hard 3-class labels produced
    by meta model at the chosen threshold) — not ideal. Better: export binary
    LGBM and post-process in Rust. For now distill a 3-class model on the
    training set using soft assignment:
      if side long: label = LONG if p_win>=thr else NO_TRADE
    """

    pass


def run_meta_walk_forward(n_folds: int = 5, promote: bool = False) -> dict:
    ensure_dirs()
    ds = read_frame(TRAINING_PARQUET)
    side_all = _primary_side(ds)
    # Keep primary rows only (should already be filtered).
    keep = side_all != LABEL_NO_TRADE
    ds = ds.loc[keep].reset_index(drop=True)
    side_all = side_all[keep]
    x = ds[FEATURE_COLUMNS]
    y = ds["Target"].astype(int).to_numpy()
    y_bin = _binary_from_target(y)
    ts = ds["timestamp"].to_numpy(dtype=np.int64)

    folds = walk_forward_folds(ts, n_folds=n_folds, rolling=True)
    fold_metrics: list[dict] = []
    thresholds: list[float] = []

    for i, (tr, te) in enumerate(folds):
        n_tr = len(tr)
        cut = max(int(n_tr * 0.8), 50)
        tr_fit, tr_val = tr[:cut], tr[cut:]
        model, _name = _fit_binary(
            x.iloc[tr_fit],
            y_bin[tr_fit],
            x.iloc[tr_val] if len(tr_val) else None,
            y_bin[tr_val] if len(tr_val) else None,
        )
        # Tune thr on val
        if len(tr_val) >= 50:
            p_val = model.predict_proba(x.iloc[tr_val])[:, 1]
            thr, _ = _tune_binary_threshold(
                y[tr_val], side_all[tr_val], p_val, min_thr=0.62, min_rate=0.025
            )
        else:
            thr = 0.65
        p_te = model.predict_proba(x.iloc[te])[:, 1]
        pred, proba = _three_class_from_binary(side_all[te], p_te, thr)
        m = compute_metrics(y[te], pred, proba)
        m["fold"] = float(i)
        m["confidence_threshold"] = thr
        fold_metrics.append(m)
        thresholds.append(thr)
        print(
            f"Meta fold {i}: thr={thr:.2f} prec={m.get('precision_tradeable', 0):.3f} "
            f"avg_r={m.get('avg_r_proxy', 0):.3f} rate={m.get('tradeable_rate', 0):.3f}"
        )

    chosen_thr = float(max(np.median(thresholds), 0.60)) if thresholds else 0.65
    final_tr, final_te = folds[-1]
    model, backend = _fit_binary(x.iloc[final_tr], y_bin[final_tr])
    p_final = model.predict_proba(x.iloc[final_te])[:, 1]
    pred_f, proba_f = _three_class_from_binary(side_all[final_te], p_final, chosen_thr)
    final_metrics = compute_metrics(y[final_te], pred_f, proba_f)
    final_metrics["confidence_threshold"] = chosen_thr
    print(
        f"Meta final holdout: prec={final_metrics.get('precision_tradeable', 0):.3f} "
        f"avg_r={final_metrics.get('avg_r_proxy', 0):.3f} "
        f"rate={final_metrics.get('tradeable_rate', 0):.3f} thr={chosen_thr:.2f}"
    )

    # Distill to 3-class model for ONNX: labels = gated meta predictions on train.
    p_tr = model.predict_proba(x.iloc[final_tr])[:, 1]
    y_distill, _ = _three_class_from_binary(side_all[final_tr], p_tr, chosen_thr)
    # If distillation collapsed to one class, fall back to true targets.
    if len(np.unique(y_distill)) < 2:
        y_distill = y[final_tr]

    print("Distilling 3-class ONNX from meta labels...")
    try:
        import lightgbm as lgb

        export_model = lgb.LGBMClassifier(
            n_estimators=300,
            learning_rate=0.05,
            max_depth=5,
            num_leaves=31,
            class_weight="balanced",
            random_state=42,
            verbosity=-1,
        )
        export_model.fit(x.iloc[final_tr], y_distill)
        export_backend = "lightgbm_meta_distill"
    except Exception:
        export_model = train_sklearn_export_model(
            x.iloc[final_tr].to_numpy(dtype=np.float64),
            y_distill.astype(np.int32),
        )
        export_backend = "sklearn_meta_distill"

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
        "confidence_threshold",
    ]
    agg: dict[str, float] = {}
    for k in keys:
        vals = [
            m[k]
            for m in fold_metrics
            if k in m and (k not in ("avg_r_proxy", "precision_tradeable") or m.get("tradeable_rate", 0) > 0)
        ]
        if vals:
            agg[k] = float(np.mean(vals))
    agg["confidence_threshold"] = chosen_thr
    for k in (
        "precision_tradeable",
        "avg_r_proxy",
        "tradeable_rate",
        "f1_macro",
        "precision_long",
        "precision_short",
    ):
        if k in final_metrics:
            agg[k] = float(0.4 * agg.get(k, final_metrics[k]) + 0.6 * final_metrics[k])
    agg["n_folds"] = float(len(fold_metrics))
    agg["n_test"] = float(sum(m.get("n_test", 0) for m in fold_metrics))
    agg["model_name"] = "meta_binary"
    agg["export_backend"] = export_backend
    agg["trained_at"] = datetime.now(timezone.utc).isoformat()
    agg["dataset_rows"] = float(len(ds))
    agg["feature_dim"] = float(len(FEATURE_COLUMNS))
    agg["final_holdout_precision_tradeable"] = float(
        final_metrics.get("precision_tradeable", 0)
    )
    agg["final_holdout_avg_r_proxy"] = float(final_metrics.get("avg_r_proxy", 0))

    export_model_to_onnx(export_model, CANDIDATE_ONNX)
    verify_onnx(CANDIDATE_ONNX)
    CANDIDATE_METRICS.write_text(
        json.dumps({"aggregate": agg, "folds": fold_metrics}, indent=2)
    )
    print(f"Wrote candidate model → {CANDIDATE_ONNX}")

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
    if promote and should:
        promote_candidate(notes=f"meta binary promote thr={chosen_thr:.2f}")
        result["promoted"] = True
        print("PROMOTED meta candidate → production")
        from training.walk_forward import _maybe_update_settings_threshold

        _maybe_update_settings_threshold(chosen_thr)
    elif promote:
        print("NOT promoted — meta candidate did not beat gates")
    return result
