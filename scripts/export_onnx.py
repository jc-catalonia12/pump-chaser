#!/usr/bin/env python3
"""Export supervised setup classifier to ONNX for the Rust bot (tract inference).

Usage (from repo root, with venv active):

  python scripts/export_onnx.py
  python scripts/export_onnx.py --db data/mexc_trading_bot.db

Requires: pip install -r requirements.txt
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
from pathlib import Path

FEATURE_DIM = 15  # 10 technical + 5 signal-context features (composite_score, zone_score, volume_surge, side_long, price_chg_abs)


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
        if not features:
            comps = payload.get("components") or {}
            features = [
                float(comps.get("volume_zscore", payload.get("volume_zscore", 0))),
                float(comps.get("volume_surge_ratio", payload.get("volume_surge_ratio", 0))),
                float(comps.get("price_change_pct", payload.get("price_change_pct", 0))),
                float(comps.get("oi_proxy_score", payload.get("oi_proxy_score", 0))),
            ]
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


def train_and_export(x: list[list[float]], y: list[int], out_path: Path) -> None:
    from sklearn.ensemble import GradientBoostingClassifier
    from skl2onnx import convert_sklearn
    from skl2onnx.common.data_types import FloatTensorType

    if len(x) < 50 or len(set(y)) < 2:
        raise SystemExit(
            f"Need >=50 labeled samples with both classes; got {len(x)} rows, classes={set(y)}"
        )

    # Pad short feature vectors to FEATURE_DIM
    x = [normalize(row) for row in x]

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
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "wb") as f:
        f.write(onnx_model.SerializeToString())

    try:
        import onnxruntime as ort

        sess = ort.InferenceSession(str(out_path), providers=["CPUExecutionProvider"])
        sample = [[0.0] * FEATURE_DIM]
        sess.run(None, {sess.get_inputs()[0].name: sample})
    except Exception as exc:
        raise SystemExit(f"Exported ONNX failed ONNX Runtime validation: {exc}") from exc

    print(f"Wrote ONNX model ({len(x)} samples) -> {out_path}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Export ML model to ONNX for Rust bot")
    parser.add_argument(
        "--db",
        type=Path,
        default=Path("data/mexc_trading_bot.db"),
        help="SQLite DB with resolved signals",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=Path("data/models/supervised.onnx"),
        help="Output ONNX path",
    )
    parser.add_argument("--limit", type=int, default=500)
    args = parser.parse_args()

    if not args.db.exists():
        print(f"DB not found: {args.db}", file=sys.stderr)
        sys.exit(1)

    x, y = load_training_rows(args.db, args.limit)
    train_and_export(x, y, args.out)


if __name__ == "__main__":
    main()
