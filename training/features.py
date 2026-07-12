"""Bar-level feature engineering from OHLCV parquet."""

from __future__ import annotations

import numpy as np
import pandas as pd

from training.paths import ensure_dirs, features_path, raw_path, read_frame, write_frame
from training.schema import FEATURE_COLUMNS


def _ema(series: pd.Series, span: int) -> pd.Series:
    return series.ewm(span=span, adjust=False).mean()


def _rsi(close: pd.Series, period: int = 14) -> pd.Series:
    delta = close.diff()
    gain = delta.clip(lower=0.0)
    loss = (-delta).clip(lower=0.0)
    avg_gain = gain.ewm(alpha=1 / period, min_periods=period, adjust=False).mean()
    avg_loss = loss.ewm(alpha=1 / period, min_periods=period, adjust=False).mean()
    rs = avg_gain / avg_loss.replace(0.0, np.nan)
    return 100.0 - (100.0 / (1.0 + rs))


def _atr(high: pd.Series, low: pd.Series, close: pd.Series, period: int = 14) -> pd.Series:
    prev_close = close.shift(1)
    tr = pd.concat(
        [
            (high - low).abs(),
            (high - prev_close).abs(),
            (low - prev_close).abs(),
        ],
        axis=1,
    ).max(axis=1)
    return tr.rolling(period, min_periods=max(5, period // 2)).mean()


def _adx(high: pd.Series, low: pd.Series, close: pd.Series, period: int = 14) -> pd.Series:
    up = high.diff()
    down = -low.diff()
    plus_dm = np.where((up > down) & (up > 0), up, 0.0)
    minus_dm = np.where((down > up) & (down > 0), down, 0.0)
    atr = _atr(high, low, close, period)
    plus_di = 100.0 * pd.Series(plus_dm, index=close.index).ewm(
        alpha=1 / period, adjust=False
    ).mean() / atr.replace(0.0, np.nan)
    minus_di = 100.0 * pd.Series(minus_dm, index=close.index).ewm(
        alpha=1 / period, adjust=False
    ).mean() / atr.replace(0.0, np.nan)
    dx = (100.0 * (plus_di - minus_di).abs() / (plus_di + minus_di).replace(0.0, np.nan)).fillna(0.0)
    return dx.ewm(alpha=1 / period, adjust=False).mean()


def compute_features(df: pd.DataFrame) -> pd.DataFrame:
    """Transform raw OHLCV into ML features. Preserves timestamp/symbol."""
    if df.empty:
        cols = ["timestamp", "symbol", *FEATURE_COLUMNS]
        return pd.DataFrame(columns=cols)

    out = df.copy().sort_values("timestamp").reset_index(drop=True)
    o = out["open"].astype(float)
    h = out["high"].astype(float)
    l = out["low"].astype(float)
    c = out["close"].astype(float)
    v = out["volume"].astype(float)
    qv = out.get("quote_volume", v * c).astype(float)

    ema20 = _ema(c, 20)
    ema50 = _ema(c, 50)
    ema100 = _ema(c, 100)
    ema200 = _ema(c, 200)

    macd_line = _ema(c, 12) - _ema(c, 26)
    macd_sig = _ema(macd_line, 9)
    macd_hist = macd_line - macd_sig

    atr = _atr(h, l, c, 14)
    atr_pct = atr / c.replace(0.0, np.nan)

    mid = c.rolling(20, min_periods=5).mean()
    std = c.rolling(20, min_periods=5).std()
    bb_width = (4.0 * std) / mid.replace(0.0, np.nan)

    # Session VWAP approximation from cumulative quote/volume.
    cum_qv = qv.cumsum()
    cum_v = v.cumsum().replace(0.0, np.nan)
    vwap = cum_qv / cum_v
    vwap_dist = (c - vwap) / c.replace(0.0, np.nan)

    vol_ma = v.rolling(20, min_periods=5).mean().replace(0.0, np.nan)
    volume_ma_ratio = v / vol_ma

    ret1 = c.pct_change(1)
    volatility = ret1.rolling(20, min_periods=5).std()

    rng = (h - l).replace(0.0, np.nan)
    body = (c - o).abs()
    body_pct = body / rng
    upper_wick_pct = (h - pd.concat([o, c], axis=1).max(axis=1)) / rng
    lower_wick_pct = (pd.concat([o, c], axis=1).min(axis=1) - l) / rng

    # Trend strength: |ema20 - ema50| / ATR
    trend_strength = (ema20 - ema50).abs() / atr.replace(0.0, np.nan)

    ts = pd.to_datetime(out["timestamp"], unit="s", utc=True)
    hour = ts.dt.hour + ts.dt.minute / 60.0
    dow = ts.dt.dayofweek.astype(float)

    feat = pd.DataFrame(
        {
            "timestamp": out["timestamp"].astype(int),
            "symbol": out["symbol"].astype(str),
            "ema20_ratio": ema20 / c.replace(0.0, np.nan) - 1.0,
            "ema50_ratio": ema50 / c.replace(0.0, np.nan) - 1.0,
            "ema100_ratio": ema100 / c.replace(0.0, np.nan) - 1.0,
            "ema200_ratio": ema200 / c.replace(0.0, np.nan) - 1.0,
            "rsi_14": _rsi(c, 14) / 100.0,  # normalize 0..1
            "macd_hist": macd_hist / c.replace(0.0, np.nan),
            "atr_pct": atr_pct,
            "adx_14": _adx(h, l, c, 14) / 100.0,
            "vwap_dist": vwap_dist,
            "bb_width": bb_width,
            "volume_ma_ratio": volume_ma_ratio,
            "volatility": volatility,
            "body_pct": body_pct,
            "upper_wick_pct": upper_wick_pct,
            "lower_wick_pct": lower_wick_pct,
            "return_1": ret1,
            "return_5": c.pct_change(5),
            "return_20": c.pct_change(20),
            "momentum_10": c.pct_change(10),
            "trend_strength": trend_strength,
            "hour_sin": np.sin(2 * np.pi * hour / 24.0),
            "hour_cos": np.cos(2 * np.pi * hour / 24.0),
            "dow_sin": np.sin(2 * np.pi * dow / 7.0),
            "dow_cos": np.cos(2 * np.pi * dow / 7.0),
        }
    )

    for col in FEATURE_COLUMNS:
        feat[col] = feat[col].replace([np.inf, -np.inf], np.nan).fillna(0.0)

    # Drop warm-up rows where EMA200 is unreliable.
    feat = feat.iloc[200:].reset_index(drop=True)
    return feat[["timestamp", "symbol", *FEATURE_COLUMNS]]


def build_features_for_symbol(symbol: str, interval: str = "Min15") -> pd.DataFrame:
    ensure_dirs()
    src = raw_path(symbol, interval)
    if not src.exists():
        raise FileNotFoundError(f"Raw candles missing: {src} — run download first")
    raw = read_frame(src)
    feat = compute_features(raw)
    dest = features_path(symbol, interval)
    write_frame(feat, dest)
    # Keep schema next to models for Rust.
    from training.registry import write_feature_schema

    write_feature_schema()
    return feat


def build_features_all(
    interval: str = "Min15",
    symbols: list[str] | None = None,
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
        print(f"Features {symbol} {interval}...")
        feat = build_features_for_symbol(symbol, interval)
        written.append(str(features_path(symbol, interval)))
        print(f"  → {len(feat)} rows")
    return written
