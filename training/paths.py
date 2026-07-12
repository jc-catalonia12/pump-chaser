"""Repo-relative data layout for the historical ML pipeline."""

from __future__ import annotations

from pathlib import Path

import pandas as pd

REPO_ROOT = Path(__file__).resolve().parent.parent

DATA_RAW = REPO_ROOT / "data" / "raw"
DATA_FEATURES = REPO_ROOT / "data" / "features"
DATA_LABELS = REPO_ROOT / "data" / "labels"
DATASETS = REPO_ROOT / "datasets"
MODELS = REPO_ROOT / "models"

PRODUCTION_ONNX = MODELS / "production.onnx"
CANDIDATE_ONNX = MODELS / "candidate.onnx"
FEATURE_SCHEMA = MODELS / "feature_schema.json"
PRODUCTION_METRICS = MODELS / "production.metrics.json"
CANDIDATE_METRICS = MODELS / "candidate.metrics.json"
ARCHIVE_DIR = MODELS / "archive"
TRAINING_PARQUET = DATASETS / "training.csv.gz"

# Also mirrored under data/models for the Rust bot's historical default path.
DATA_MODELS = REPO_ROOT / "data" / "models"


def ensure_dirs() -> None:
    for d in (
        DATA_RAW,
        DATA_FEATURES,
        DATA_LABELS,
        DATASETS,
        MODELS,
        ARCHIVE_DIR,
        DATA_MODELS,
    ):
        d.mkdir(parents=True, exist_ok=True)


def raw_path(symbol: str, interval: str) -> Path:
    safe = symbol.replace("/", "_").upper()
    return DATA_RAW / f"{safe}_{interval}.csv.gz"


def features_path(symbol: str, interval: str) -> Path:
    safe = symbol.replace("/", "_").upper()
    return DATA_FEATURES / f"{safe}_{interval}.csv.gz"


def labels_path(symbol: str, interval: str) -> Path:
    safe = symbol.replace("/", "_").upper()
    return DATA_LABELS / f"{safe}_{interval}.csv.gz"


def write_frame(df: pd.DataFrame, path: Path) -> None:
    """Write a dataframe; prefer parquet when pyarrow is available, else csv.gz."""
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.suffix == ".parquet" or path.name.endswith(".parquet"):
        try:
            df.to_parquet(path, index=False)
            return
        except Exception:
            path = path.with_suffix("").with_suffix(".csv.gz")
    df.to_csv(path, index=False, compression="gzip")


def read_frame(path: Path) -> pd.DataFrame:
    path = Path(path)
    if not path.exists():
        # Fall back between parquet / csv.gz naming.
        alt = (
            path.with_suffix("").with_suffix(".csv.gz")
            if path.suffix == ".parquet"
            else path.with_suffix("").with_suffix(".parquet")
            if str(path).endswith(".csv.gz")
            else None
        )
        if alt is not None and alt.exists():
            path = alt
        else:
            raise FileNotFoundError(path)
    if str(path).endswith(".parquet"):
        return pd.read_parquet(path)
    return pd.read_csv(path, compression="gzip")
