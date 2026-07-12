"""Model training: LightGBM (preferred) or sklearn GBM multi-class."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import numpy as np
from sklearn.metrics import (
    accuracy_score,
    f1_score,
    precision_score,
    recall_score,
)

from training.schema import FEATURE_COLUMNS, FEATURE_DIM, LABEL_NAMES

# Align with config/settings.yaml ml.supervised_threshold (live gate).
DEFAULT_CONFIDENCE_THRESHOLD = 0.58


@dataclass
class TrainedModel:
    name: str
    model: Any
    metrics: dict[str, float]
    feature_columns: list[str]
    confidence_threshold: float = DEFAULT_CONFIDENCE_THRESHOLD


def _try_lightgbm() -> bool:
    try:
        import lightgbm  # noqa: F401

        return True
    except ImportError:
        return False


def apply_confidence_gate(
    y_proba: np.ndarray,
    threshold: float,
) -> np.ndarray:
    """Map probabilities → class preds; force NO_TRADE when max trade conf < threshold.

    Prefer LONG/SHORT over NO_TRADE only when that side's probability clears `threshold`.
    """
    proba = np.asarray(y_proba, dtype=np.float64)
    n = proba.shape[0]
    pred = np.zeros(n, dtype=np.int32)
    if proba.shape[1] < 3:
        pred[:] = np.argmax(proba, axis=1).astype(np.int32)
        return pred

    p_long = proba[:, 1]
    p_short = proba[:, 2]
    # Choose best actionable side; gate by absolute probability.
    prefer_long = p_long >= p_short
    best = np.where(prefer_long, p_long, p_short)
    side = np.where(prefer_long, 1, 2).astype(np.int32)
    trade = best >= threshold
    pred[trade] = side[trade]
    return pred


def tune_confidence_threshold(
    y_true: np.ndarray,
    y_proba: np.ndarray,
    *,
    min_threshold: float = 0.52,
    max_threshold: float = 0.88,
    step: float = 0.02,
    min_tradeable_rate: float = 0.015,
    prefer: float | None = None,
) -> tuple[float, dict[str, float]]:
    """Pick threshold maximizing avg_r_proxy, then precision_tradeable."""
    best_thr = prefer if prefer is not None else DEFAULT_CONFIDENCE_THRESHOLD
    best_metrics = compute_metrics(
        y_true, apply_confidence_gate(y_proba, best_thr), y_proba
    )
    best_score = (
        best_metrics.get("avg_r_proxy", -999.0),
        best_metrics.get("precision_tradeable", 0.0),
        best_metrics.get("tradeable_rate", 0.0),
    )

    thr = min_threshold
    while thr <= max_threshold + 1e-9:
        pred = apply_confidence_gate(y_proba, thr)
        m = compute_metrics(y_true, pred, y_proba)
        if m.get("tradeable_rate", 0.0) < min_tradeable_rate:
            thr = round(thr + step, 4)
            continue
        # Require at least break-even-ish precision when possible (~33% for 2R:1R).
        score = (
            m.get("avg_r_proxy", -999.0),
            m.get("precision_tradeable", 0.0),
            m.get("tradeable_rate", 0.0),
        )
        if score > best_score:
            best_score = score
            best_thr = thr
            best_metrics = m
        thr = round(thr + step, 4)

    best_metrics = dict(best_metrics)
    best_metrics["confidence_threshold"] = float(best_thr)
    return float(best_thr), best_metrics


def train_classifier(
    x_train: np.ndarray | Any,
    y_train: np.ndarray | Any,
    x_test: np.ndarray | Any | None = None,
    y_test: np.ndarray | Any | None = None,
    random_state: int = 42,
    prefer_lightgbm: bool = True,
    confidence_threshold: float | None = None,
    tune_threshold: bool = True,
) -> TrainedModel:
    """Fit a multi-class model and optionally score on a holdout set.

    Threshold is tuned on an *inner* validation slice of the train set only
    (never on x_test) to avoid leakage.
    """
    use_lgb = prefer_lightgbm and _try_lightgbm()

    # Inner split for early stopping + threshold tuning (last 20% of train by row order).
    n_train = len(y_train)
    cut = max(int(n_train * 0.8), min(200, n_train - 50)) if n_train > 300 else n_train
    if hasattr(x_train, "iloc"):
        x_fit, y_fit = x_train.iloc[:cut], y_train.iloc[:cut]
        x_val, y_val = x_train.iloc[cut:], y_train.iloc[cut:]
    else:
        x_fit, y_fit = x_train[:cut], y_train[:cut]
        x_val, y_val = x_train[cut:], y_train[cut:]
    has_val = len(y_val) >= 50

    if use_lgb:
        import lightgbm as lgb

        model = lgb.LGBMClassifier(
            n_estimators=500,
            learning_rate=0.03,
            max_depth=6,
            num_leaves=48,
            min_child_samples=40,
            subsample=0.8,
            colsample_bytree=0.8,
            reg_alpha=0.1,
            reg_lambda=1.0,
            class_weight="balanced",
            random_state=random_state,
            verbosity=-1,
        )
        name = "lightgbm"
        fit_kwargs: dict[str, Any] = {}
        if has_val:
            fit_kwargs["eval_set"] = [(x_val, y_val)]
            fit_kwargs["callbacks"] = [
                lgb.early_stopping(60, verbose=False),
                lgb.log_evaluation(period=0),
            ]
        model.fit(x_fit, y_fit, **fit_kwargs)
    else:
        from sklearn.ensemble import GradientBoostingClassifier

        model = GradientBoostingClassifier(
            n_estimators=200,
            learning_rate=0.05,
            max_depth=4,
            random_state=random_state,
        )
        name = "sklearn_gbm"
        model.fit(x_fit, y_fit)

    metrics: dict[str, float] = {"train_rows": float(n_train)}
    thr = (
        float(confidence_threshold)
        if confidence_threshold is not None
        else DEFAULT_CONFIDENCE_THRESHOLD
    )

    if tune_threshold and has_val:
        thr, _ = tune_confidence_threshold(
            np.asarray(y_val),
            model.predict_proba(x_val),
            prefer=thr,
            min_tradeable_rate=0.02,
        )

    if x_test is not None and y_test is not None and len(y_test) > 0:
        y_true = np.asarray(y_test)
        proba = model.predict_proba(x_test)
        pred = apply_confidence_gate(proba, thr)
        gated = compute_metrics(y_true, pred, proba)
        gated["confidence_threshold"] = thr
        raw_pred = np.argmax(proba, axis=1)
        raw = compute_metrics(y_true, raw_pred, proba)
        metrics.update(gated)
        metrics["ungated_precision_tradeable"] = raw.get("precision_tradeable", 0.0)
        metrics["ungated_avg_r_proxy"] = raw.get("avg_r_proxy", 0.0)
        metrics["ungated_tradeable_rate"] = raw.get("tradeable_rate", 0.0)

    return TrainedModel(
        name=name,
        model=model,
        metrics=metrics,
        feature_columns=list(FEATURE_COLUMNS),
        confidence_threshold=thr,
    )


def train_final_model(
    x_train: np.ndarray | Any,
    y_train: np.ndarray | Any,
    random_state: int = 42,
) -> tuple[Any, str]:
    """Train the model that will be exported to ONNX (prefer LightGBM)."""
    if _try_lightgbm():
        import lightgbm as lgb

        model = lgb.LGBMClassifier(
            n_estimators=400,
            learning_rate=0.03,
            max_depth=6,
            num_leaves=48,
            min_child_samples=40,
            subsample=0.8,
            colsample_bytree=0.8,
            reg_alpha=0.1,
            reg_lambda=1.0,
            class_weight="balanced",
            random_state=random_state,
            verbosity=-1,
        )
        model.fit(x_train, y_train)
        return model, "lightgbm"

    from sklearn.ensemble import GradientBoostingClassifier

    model = GradientBoostingClassifier(
        n_estimators=200,
        learning_rate=0.05,
        max_depth=4,
        random_state=random_state,
    )
    model.fit(np.asarray(x_train, dtype=np.float64), np.asarray(y_train, dtype=np.int32))
    return model, "sklearn_gbm"


def train_sklearn_export_model(
    x_train: np.ndarray,
    y_train: np.ndarray,
    random_state: int = 42,
) -> Any:
    """Fallback sklearn GBM for ONNX when LightGBM export is unavailable."""
    from sklearn.ensemble import GradientBoostingClassifier

    model = GradientBoostingClassifier(
        n_estimators=200,
        learning_rate=0.05,
        max_depth=4,
        random_state=random_state,
    )
    model.fit(np.asarray(x_train, dtype=np.float64), np.asarray(y_train, dtype=np.int32))
    return model


def compute_metrics(
    y_true: np.ndarray,
    y_pred: np.ndarray,
    y_proba: np.ndarray | None = None,
) -> dict[str, float]:
    """Classification + trade-proxy metrics (LONG/SHORT only)."""
    y_true = np.asarray(y_true)
    y_pred = np.asarray(y_pred)
    metrics: dict[str, float] = {
        "accuracy": float(accuracy_score(y_true, y_pred)),
        "f1_macro": float(f1_score(y_true, y_pred, average="macro", zero_division=0)),
        "precision_macro": float(
            precision_score(y_true, y_pred, average="macro", zero_division=0)
        ),
        "recall_macro": float(recall_score(y_true, y_pred, average="macro", zero_division=0)),
        "n_test": float(len(y_true)),
    }

    for cls, name in ((1, "long"), (2, "short")):
        mask = y_pred == cls
        if mask.any():
            metrics[f"precision_{name}"] = float((y_true[mask] == cls).mean())
            metrics[f"pred_count_{name}"] = float(mask.sum())
        else:
            metrics[f"precision_{name}"] = 0.0
            metrics[f"pred_count_{name}"] = 0.0

    trade_mask = (y_pred == 1) | (y_pred == 2)
    if trade_mask.any():
        metrics["precision_tradeable"] = float(
            (y_true[trade_mask] == y_pred[trade_mask]).mean()
        )
        metrics["tradeable_rate"] = float(trade_mask.mean())
        correct = y_true[trade_mask] == y_pred[trade_mask]
        # Match live barriers roughly: +2R TP / -1R SL → expect ~+2 when correct if TP=2*SL.
        r = np.where(correct, 2.0, -1.0)
        metrics["net_r_proxy"] = float(r.sum())
        metrics["avg_r_proxy"] = float(r.mean())
    else:
        metrics["precision_tradeable"] = 0.0
        metrics["tradeable_rate"] = 0.0
        metrics["net_r_proxy"] = 0.0
        metrics["avg_r_proxy"] = 0.0

    if y_proba is not None and y_proba.shape[1] >= 3:
        conf = y_proba[np.arange(len(y_pred)), np.clip(y_pred.astype(int), 0, y_proba.shape[1] - 1)]
        # For NO_TRADE preds, confidence is P(NO_TRADE); for trades use side prob.
        trade_conf = np.maximum(y_proba[:, 1], y_proba[:, 2])
        metrics["mean_confidence"] = float(np.where(trade_mask, trade_conf, conf).mean())
        metrics["mean_trade_confidence"] = (
            float(trade_conf[trade_mask].mean()) if trade_mask.any() else 0.0
        )

    metrics["feature_dim"] = float(FEATURE_DIM)
    return metrics


def score_promotion(candidate: dict[str, float], production: dict[str, float] | None) -> bool:
    """Deploy only if candidate is clearly better on gated trade quality."""
    # Prefer recent holdout (most relevant for paper going forward).
    hold_r = candidate.get("final_holdout_avg_r_proxy", candidate.get("avg_r_proxy", -1))
    hold_p = candidate.get(
        "final_holdout_precision_tradeable", candidate.get("precision_tradeable", 0)
    )
    strong = (
        hold_r > 0.05
        and hold_p >= 0.36
        and candidate.get("tradeable_rate", 0) >= 0.02
        and candidate.get("n_test", 0) >= 100
        and candidate.get("avg_r_proxy", -1) > -0.05  # not disastrous on avg folds
    )

    if not production:
        return strong

    if "confidence_threshold" not in production and "mean_trade_confidence" not in production:
        return strong

    cand_r = candidate.get("avg_r_proxy", -999.0)
    prod_r = production.get("avg_r_proxy", -999.0)
    cand_p = candidate.get("precision_tradeable", 0.0)
    prod_p = production.get("precision_tradeable", 0.0)
    cand_tr = candidate.get("tradeable_rate", 0.0)

    if cand_tr < 0.015:
        return False

    better_r = cand_r > prod_r + 0.02
    better_p = cand_p > prod_p + 0.02
    positive_r = cand_r > 0.0

    if strong:
        return True
    if positive_r and (better_r or better_p):
        return True
    if better_r and better_p:
        return True
    if better_r and cand_p >= prod_p - 0.01:
        return True
    return False


def class_names() -> list[str]:
    return list(LABEL_NAMES)
