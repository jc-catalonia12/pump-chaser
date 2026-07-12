"""Join features + labels into datasets/training.csv.gz."""

from __future__ import annotations

import pandas as pd

from training.paths import (
    DATA_FEATURES,
    DATA_LABELS,
    TRAINING_PARQUET,
    ensure_dirs,
    read_frame,
    write_frame,
)
from training.schema import FEATURE_COLUMNS


def _symbol_from_features_name(name: str, interval: str) -> str | None:
    for ext in (".csv.gz", ".parquet"):
        suffix = f"_{interval}{ext}"
        if name.endswith(suffix):
            return name[: -len(suffix)]
    return None


def build_dataset(
    interval: str = "Min15",
    symbols: list[str] | None = None,
    *,
    primary_only: bool = True,
) -> pd.DataFrame:
    """Join features + labels.

    When `primary_only` is True (default), keep only momentum-primary bars:
    price stacked vs EMA20/EMA50. Labels on those bars are already meta-aligned
    (LONG/SHORT win vs NO_TRADE fail), which is standard meta-labeling.
    """
    ensure_dirs()
    allow = {s.upper() for s in symbols} if symbols else None
    frames: list[pd.DataFrame] = []
    for feat_path in sorted(DATA_FEATURES.glob(f"*_{interval}.*")):
        symbol = _symbol_from_features_name(feat_path.name, interval)
        if not symbol:
            continue
        if allow is not None and symbol.upper() not in allow:
            continue
        lab_path = labels_path_for(symbol, interval)
        if lab_path is None:
            print(f"Skipping {symbol}: missing labels")
            continue
        feat = read_frame(feat_path)
        lab = read_frame(lab_path)
        merged = feat.merge(lab, on=["timestamp", "symbol"], how="inner")
        merged = merged[merged["Target"] >= 0].copy()
        if primary_only and {"ema20_ratio", "ema50_ratio"}.issubset(merged.columns):
            # Matches labels.generate_labels momentum primary:
            # long: c > ema20 > ema50  → ema20_ratio < 0, ema50_ratio < ema20_ratio
            # short: c < ema20 < ema50 → ema20_ratio > 0, ema50_ratio > ema20_ratio
            e20 = merged["ema20_ratio"]
            e50 = merged["ema50_ratio"]
            primary_long = (e20 < 0) & (e50 < e20)
            primary_short = (e20 > 0) & (e50 > e20)
            before = len(merged)
            merged = merged[primary_long | primary_short].copy()
            print(
                f"Dataset slice {symbol}: {len(merged)} primary rows "
                f"(from {before} labeled)"
            )
        else:
            print(f"Dataset slice {symbol}: {len(merged)} rows")
        frames.append(merged)

    if not frames:
        raise SystemExit("No feature/label pairs found — run features and labels first")

    ds = pd.concat(frames, ignore_index=True)
    ds = ds.sort_values(["symbol", "timestamp"]).reset_index(drop=True)
    cols = ["timestamp", "symbol", *FEATURE_COLUMNS, "Target"]
    ds = ds[cols]
    write_frame(ds, TRAINING_PARQUET)
    print(f"Wrote {len(ds)} rows → {TRAINING_PARQUET}")
    print("Class distribution:", ds["Target"].value_counts().to_dict())
    return ds


def labels_path_for(symbol: str, interval: str):
    for ext in (".csv.gz", ".parquet"):
        p = DATA_LABELS / f"{symbol}_{interval}{ext}"
        if p.exists():
            return p
    return None


def load_training_xy() -> tuple[pd.DataFrame, pd.Series]:
    if not TRAINING_PARQUET.exists():
        raise FileNotFoundError(f"Missing {TRAINING_PARQUET} — run dataset builder first")
    ds = read_frame(TRAINING_PARQUET)
    x = ds[FEATURE_COLUMNS]
    y = ds["Target"].astype(int)
    return x, y
