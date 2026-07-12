#!/usr/bin/env python3
"""DEPRECATED — signal-DB ONNX export (V1).

V2 trains from historical candles instead of live signal outcomes:

    python -m training pipeline --symbols BTC_USDT ETH_USDT --days 180 --interval Min15

This script remains only for emergency rebuilds from SQLite signal history.
Prefer the historical pipeline.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import warnings
from pathlib import Path

warnings.warn(
    "scripts/export_onnx.py is deprecated; use `python -m training pipeline`",
    DeprecationWarning,
    stacklevel=1,
)

# Legacy binary classifier dim — not compatible with V2 24-dim / 3-class models.
FEATURE_DIM = 24


def normalize(vec: list[float] | None, dim: int = FEATURE_DIM) -> list[float]:
    if not vec:
        return [0.0] * dim
    out = [0.0 if v is None else float(v) for v in vec[:dim]]
    if len(out) < dim:
        out.extend([0.0] * (dim - len(out)))
    return out


def load_training_rows(db_path: Path, limit: int = 500) -> tuple[list[list[float]], list[int]]:
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        """
        SELECT payload, outcome FROM signals
        WHERE outcome IN ('win', 'loss', 'expired')
        ORDER BY id DESC LIMIT ?
        """,
        (limit,),
    ).fetchall()
    conn.close()

    wins = {"win"}
    losses = {"loss", "expired"}
    x: list[list[float]] = []
    y: list[int] = []
    for row in rows:
        payload = json.loads(row["payload"] or "{}")
        features = payload.get("ml_features")
        vec = normalize(features)
        if not any(vec):
            continue
        outcome = (row["outcome"] or "").lower()
        if outcome in wins:
            x.append(vec)
            y.append(1)
        elif outcome in losses:
            x.append(vec)
            y.append(0)
    return x, y


def main() -> None:
    print(
        "DEPRECATED: use `python -m training pipeline` for historical candle training.",
        file=sys.stderr,
    )
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", type=Path, default=Path("data/mexc_trading_bot.db"))
    ap.add_argument("--out", type=Path, default=Path("data/models/supervised.onnx"))
    ap.add_argument("--limit", type=int, default=500)
    args = ap.parse_args()

    x, y = load_training_rows(args.db, args.limit)
    if len(x) < 50 or len(set(y)) < 2:
        raise SystemExit(
            f"Need >=50 labeled samples with both classes; got {len(x)} rows, classes={set(y)}"
        )

    from sklearn.ensemble import GradientBoostingClassifier
    from skl2onnx import convert_sklearn
    from skl2onnx.common.data_types import FloatTensorType

    model = GradientBoostingClassifier(
        n_estimators=80,
        learning_rate=0.05,
        max_depth=4,
        random_state=42,
    )
    model.fit(x, y)
    onnx_model = convert_sklearn(
        model,
        initial_types=[("input", FloatTensorType([None, FEATURE_DIM]))],
        options={id(model): {"zipmap": False}},
    )
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with open(args.out, "wb") as f:
        f.write(onnx_model.SerializeToString())
    print(f"Wrote legacy binary ONNX → {args.out} (prefer python -m training)")


if __name__ == "__main__":
    main()
