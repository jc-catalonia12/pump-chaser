"""Model registry: candidate / production / archive + feature schema."""

from __future__ import annotations

import json
import shutil
from datetime import datetime, timezone
from pathlib import Path

from training.paths import (
    ARCHIVE_DIR,
    CANDIDATE_METRICS,
    CANDIDATE_ONNX,
    DATA_MODELS,
    FEATURE_SCHEMA,
    PRODUCTION_METRICS,
    PRODUCTION_ONNX,
    ensure_dirs,
)
from training.schema import feature_schema_dict


def write_feature_schema(path: Path | None = None) -> Path:
    ensure_dirs()
    dest = path or FEATURE_SCHEMA
    dest.write_text(json.dumps(feature_schema_dict(), indent=2))
    # Mirror for Rust data/models/
    mirror = DATA_MODELS / "feature_schema.json"
    mirror.write_text(dest.read_text())
    return dest


def load_production_metrics() -> dict | None:
    if not PRODUCTION_METRICS.exists():
        return None
    data = json.loads(PRODUCTION_METRICS.read_text())
    if isinstance(data, dict) and "aggregate" in data:
        return data["aggregate"]
    return data if isinstance(data, dict) else None


def promote_candidate(notes: str = "") -> dict:
    """Archive current production (if any), then move candidate → production.

    Also copies production.onnx into data/models/ for the Rust bot default path.
    """
    ensure_dirs()
    write_feature_schema()

    if not CANDIDATE_ONNX.exists():
        raise FileNotFoundError(f"Missing candidate model: {CANDIDATE_ONNX}")

    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    archive_slot = ARCHIVE_DIR / stamp
    archive_slot.mkdir(parents=True, exist_ok=True)

    if PRODUCTION_ONNX.exists():
        shutil.copy2(PRODUCTION_ONNX, archive_slot / "production.onnx")
    if PRODUCTION_METRICS.exists():
        shutil.copy2(PRODUCTION_METRICS, archive_slot / "production.metrics.json")

    shutil.copy2(CANDIDATE_ONNX, PRODUCTION_ONNX)
    if CANDIDATE_METRICS.exists():
        meta = json.loads(CANDIDATE_METRICS.read_text())
        if isinstance(meta, dict):
            meta["promoted_at"] = datetime.now(timezone.utc).isoformat()
            meta["notes"] = notes
            PRODUCTION_METRICS.write_text(json.dumps(meta, indent=2))
        else:
            shutil.copy2(CANDIDATE_METRICS, PRODUCTION_METRICS)
    else:
        PRODUCTION_METRICS.write_text(
            json.dumps({"promoted_at": stamp, "notes": notes}, indent=2)
        )

    # Mirror into data/models/ so Rust `onnx_model_path` default keeps working.
    shutil.copy2(PRODUCTION_ONNX, DATA_MODELS / "production.onnx")
    # Also write as supervised.onnx for backwards-compatible config paths.
    shutil.copy2(PRODUCTION_ONNX, DATA_MODELS / "supervised.onnx")
    if PRODUCTION_METRICS.exists():
        shutil.copy2(PRODUCTION_METRICS, DATA_MODELS / "production.metrics.json")

    (archive_slot / "notes.txt").write_text(notes or "promoted")
    print(f"Production updated → {PRODUCTION_ONNX} (archived prior to {archive_slot})")
    return {"production": str(PRODUCTION_ONNX), "archive": str(archive_slot)}
