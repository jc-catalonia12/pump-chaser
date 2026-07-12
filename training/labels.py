"""Triple-barrier label generation (LONG / SHORT / NO_TRADE).

Supports momentum-aligned (meta-label style) targets: only keep LONG/SHORT when
a simple trend primary signal agrees with the barrier outcome. This reduces
label noise and is the usual path to positive expectancy.
"""

from __future__ import annotations

import numpy as np
import pandas as pd

from training.paths import ensure_dirs, labels_path, raw_path, read_frame, write_frame
from training.schema import (
    DEFAULT_HORIZON_BARS,
    DEFAULT_SL_PCT,
    DEFAULT_TP_PCT,
    LABEL_LONG,
    LABEL_NO_TRADE,
    LABEL_SHORT,
)


def _atr_pct(high: np.ndarray, low: np.ndarray, close: np.ndarray, period: int = 14) -> np.ndarray:
    n = len(close)
    tr = np.zeros(n, dtype=np.float64)
    tr[0] = high[0] - low[0]
    for i in range(1, n):
        tr[i] = max(high[i] - low[i], abs(high[i] - close[i - 1]), abs(low[i] - close[i - 1]))
    atr = np.zeros(n, dtype=np.float64)
    atr[:period] = np.nan
    if n > period:
        atr[period - 1] = np.mean(tr[:period])
        alpha = 1.0 / period
        for i in range(period, n):
            atr[i] = atr[i - 1] * (1 - alpha) + tr[i] * alpha
    with np.errstate(divide="ignore", invalid="ignore"):
        out = atr / np.where(close > 0, close, np.nan)
    return np.nan_to_num(out, nan=0.0)


def _ema(close: np.ndarray, span: int) -> np.ndarray:
    out = np.empty_like(close, dtype=np.float64)
    if len(close) == 0:
        return out
    alpha = 2.0 / (span + 1.0)
    out[0] = close[0]
    for i in range(1, len(close)):
        out[i] = alpha * close[i] + (1 - alpha) * out[i - 1]
    return out


def _resolve_bar(
    highs: np.ndarray,
    lows: np.ndarray,
    entry: float,
    i: int,
    horizon: int,
    tp_pct: float,
    sl_pct: float,
) -> int:
    """Return LABEL_* for bar i using future path highs/lows."""
    n = len(highs)
    end = min(i + horizon + 1, n)
    if i + 1 >= end or entry <= 0 or tp_pct <= 0 or sl_pct <= 0:
        return LABEL_NO_TRADE

    long_tp = entry * (1.0 + tp_pct)
    long_sl = entry * (1.0 - sl_pct)
    short_tp = entry * (1.0 - tp_pct)
    short_sl = entry * (1.0 + sl_pct)

    long_tp_bar = long_sl_bar = short_tp_bar = short_sl_bar = None
    for j in range(i + 1, end):
        hi = highs[j]
        lo = lows[j]
        if long_tp_bar is None and hi >= long_tp:
            long_tp_bar = j
        if long_sl_bar is None and lo <= long_sl:
            long_sl_bar = j
        if short_tp_bar is None and lo <= short_tp:
            short_tp_bar = j
        if short_sl_bar is None and hi >= short_sl:
            short_sl_bar = j
        long_done = long_tp_bar is not None or long_sl_bar is not None
        short_done = short_tp_bar is not None or short_sl_bar is not None
        if long_done and short_done:
            break

    long_ok = long_tp_bar is not None and (long_sl_bar is None or long_tp_bar < long_sl_bar)
    short_ok = short_tp_bar is not None and (short_sl_bar is None or short_tp_bar < short_sl_bar)

    if long_ok and short_ok:
        return LABEL_LONG if long_tp_bar <= short_tp_bar else LABEL_SHORT
    if long_ok:
        return LABEL_LONG
    if short_ok:
        return LABEL_SHORT
    return LABEL_NO_TRADE


def generate_labels(
    df: pd.DataFrame,
    tp_pct: float = DEFAULT_TP_PCT,
    sl_pct: float = DEFAULT_SL_PCT,
    horizon_bars: int = DEFAULT_HORIZON_BARS,
    *,
    momentum_align: bool = True,
    atr_scaled: bool = True,
    atr_tp_mult: float = 2.0,
    atr_sl_mult: float = 1.0,
    min_atr_pct: float = 0.002,
) -> pd.DataFrame:
    """Attach Target column (0/1/2) to OHLCV frame."""
    if df.empty:
        return pd.DataFrame(columns=["timestamp", "symbol", "Target"])

    out = df.sort_values("timestamp").reset_index(drop=True)
    highs = out["high"].astype(float).to_numpy()
    lows = out["low"].astype(float).to_numpy()
    closes = out["close"].astype(float).to_numpy()
    n = len(out)
    targets = np.full(n, LABEL_NO_TRADE, dtype=np.int32)

    ema20 = _ema(closes, 20)
    ema50 = _ema(closes, 50)
    atrp = _atr_pct(highs, lows, closes, 14) if atr_scaled else None

    usable = max(0, n - horizon_bars)
    for i in range(usable):
        entry = float(closes[i])
        if atr_scaled and atrp is not None:
            a = float(atrp[i])
            if a < min_atr_pct:
                targets[i] = LABEL_NO_TRADE
                continue
            # Exact R-multiple barriers (keeps payoff ratio = atr_tp_mult:atr_sl_mult).
            use_tp = atr_tp_mult * a
            use_sl = atr_sl_mult * a
        else:
            use_tp, use_sl = tp_pct, sl_pct

        barrier = _resolve_bar(highs, lows, entry, i, horizon_bars, use_tp, use_sl)

        if momentum_align:
            primary_long = closes[i] > ema20[i] > ema50[i]
            primary_short = closes[i] < ema20[i] < ema50[i]
            if barrier == LABEL_LONG and primary_long:
                targets[i] = LABEL_LONG
            elif barrier == LABEL_SHORT and primary_short:
                targets[i] = LABEL_SHORT
            else:
                targets[i] = LABEL_NO_TRADE
        else:
            targets[i] = barrier

    result = pd.DataFrame(
        {
            "timestamp": out["timestamp"].astype(int),
            "symbol": out["symbol"].astype(str),
            "Target": targets,
        }
    )
    if horizon_bars > 0:
        result.loc[result.index[-horizon_bars]:, "Target"] = -1
    return result


def build_labels_for_symbol(
    symbol: str,
    interval: str = "Min15",
    tp_pct: float = DEFAULT_TP_PCT,
    sl_pct: float = DEFAULT_SL_PCT,
    horizon_bars: int = DEFAULT_HORIZON_BARS,
    **kwargs,
) -> pd.DataFrame:
    ensure_dirs()
    src = raw_path(symbol, interval)
    if not src.exists():
        raise FileNotFoundError(f"Raw candles missing: {src}")
    raw = read_frame(src)
    labels = generate_labels(
        raw, tp_pct=tp_pct, sl_pct=sl_pct, horizon_bars=horizon_bars, **kwargs
    )
    dest = labels_path(symbol, interval)
    write_frame(labels, dest)
    return labels


def build_labels_all(
    interval: str = "Min15",
    symbols: list[str] | None = None,
    **kwargs,
) -> list[str]:
    ensure_dirs()
    from training.paths import DATA_RAW

    allow = {s.upper() for s in symbols} if symbols else None
    written: list[str] = []
    for path in sorted(DATA_RAW.glob(f"*_{interval}.*")):
        name = path.name
        for ext in (".csv.gz", ".parquet"):
            if name.endswith(f"_{interval}{ext}"):
                symbol = name[: -len(f"_{interval}{ext}")]
                break
        else:
            continue
        if allow is not None and symbol.upper() not in allow:
            continue
        print(f"Labels {symbol} {interval}...")
        lab = build_labels_for_symbol(symbol, interval, **kwargs)
        written.append(str(labels_path(symbol, interval)))
        counts = lab[lab["Target"] >= 0]["Target"].value_counts().to_dict()
        print(f"  → {len(lab)} rows, class counts={counts}")
    return written
