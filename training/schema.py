"""Shared feature / label schema — must match Rust `src/ml/features.rs`."""

from __future__ import annotations

FEATURE_COLUMNS: list[str] = [
    "ema20_ratio",
    "ema50_ratio",
    "ema100_ratio",
    "ema200_ratio",
    "rsi_14",
    "macd_hist",
    "atr_pct",
    "adx_14",
    "vwap_dist",
    "bb_width",
    "volume_ma_ratio",
    "volatility",
    "body_pct",
    "upper_wick_pct",
    "lower_wick_pct",
    "return_1",
    "return_5",
    "return_20",
    "momentum_10",
    "trend_strength",
    "hour_sin",
    "hour_cos",
    "dow_sin",
    "dow_cos",
]

FEATURE_DIM = len(FEATURE_COLUMNS)

# Multi-class label encoding (must match ONNX output class order).
LABEL_NO_TRADE = 0
LABEL_LONG = 1
LABEL_SHORT = 2
LABEL_NAMES = ["NO_TRADE", "LONG", "SHORT"]

# Triple-barrier defaults — tightened for cleaner LONG/SHORT labels
DEFAULT_TP_PCT = 0.02  # +2.0%
DEFAULT_SL_PCT = 0.01  # -1.0%
DEFAULT_HORIZON_BARS = 48  # look-ahead window for barrier resolution

# MEXC futures interval names
INTERVALS = ("Min1", "Min5", "Min15", "Min60", "Hour4")
DEFAULT_INTERVAL = "Min15"

INTERVAL_SECONDS = {
    "Min1": 60,
    "Min5": 300,
    "Min15": 900,
    "Min60": 3600,
    "Hour1": 3600,
    "Hour4": 14400,
    "Day1": 86400,
}


def feature_schema_dict() -> dict:
    return {
        "version": "2.0.0",
        "feature_columns": FEATURE_COLUMNS,
        "feature_dim": FEATURE_DIM,
        "label_names": LABEL_NAMES,
        "label_encoding": {
            "NO_TRADE": LABEL_NO_TRADE,
            "LONG": LABEL_LONG,
            "SHORT": LABEL_SHORT,
        },
        "input_name": "input",
        "default_interval": DEFAULT_INTERVAL,
        "triple_barrier": {
            "tp_pct": DEFAULT_TP_PCT,
            "sl_pct": DEFAULT_SL_PCT,
            "horizon_bars": DEFAULT_HORIZON_BARS,
        },
    }
