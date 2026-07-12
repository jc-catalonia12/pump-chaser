"""Download historical MEXC futures candles into local files."""

from __future__ import annotations

import time
from typing import Iterable

import pandas as pd
import requests

from training.paths import ensure_dirs, raw_path, read_frame, write_frame
from training.schema import INTERVAL_SECONDS, INTERVALS

MEXC_REST = "https://contract.mexc.co"
DEFAULT_SYMBOLS = (
    "BTC_USDT",
    "ETH_USDT",
    "SOL_USDT",
    "XRP_USDT",
    "DOGE_USDT",
)

# Stock / TradFi concept plates — mirror src/exchange/symbols.rs
_EXCLUDED_PLATE_KEYWORDS = (
    "mc-trade-zone-stock",
    "mc-trade-zone-tradfi",
    "mc-trade-zone-metals",
    "mc-trade-zone-oil",
    "mc-trade-zone-commodities",
)


def _concept_plates(raw: dict) -> list[str]:
    plate = raw.get("conceptPlate")
    if isinstance(plate, list):
        return [str(p).strip().lower() for p in plate if str(p).strip()]
    if isinstance(plate, str) and plate.strip():
        text = plate.strip()
        if text.startswith("["):
            try:
                import json

                arr = json.loads(text)
                if isinstance(arr, list):
                    return [str(p).strip().lower() for p in arr if str(p).strip()]
            except Exception:  # noqa: BLE001
                pass
        return [text.lower()]
    return []


def _is_usdt_m_crypto_perp(raw: dict, crypto_only: bool = True) -> bool:
    if str(raw.get("quoteCoin") or "").upper() != "USDT":
        return False
    if str(raw.get("settleCoin") or "").upper() != "USDT":
        return False
    if int(raw.get("state") or 0) != 0:
        return False
    if bool(raw.get("isHidden")):
        return False
    if not bool(raw.get("apiAllowed", True)):
        return False
    if not crypto_only:
        return True
    if int(raw.get("type") or 1) == 2:
        return False
    for plate in _concept_plates(raw):
        for kw in _EXCLUDED_PLATE_KEYWORDS:
            if kw in plate:
                return False
    return True


def fetch_liquid_universe(
    top: int = 20,
    min_turnover_usdt: float = 500_000.0,
    min_price_usdt: float = 0.05,
    crypto_only: bool = True,
    session: requests.Session | None = None,
    timeout: float = 30.0,
) -> list[str]:
    """Fetch USDT-M crypto perps ranked by 24h turnover (same idea as the Rust scanner)."""
    sess = session or requests.Session()
    detail = sess.get(f"{MEXC_REST}/api/v1/contract/detail", timeout=timeout)
    detail.raise_for_status()
    detail_payload = detail.json()
    contracts = detail_payload.get("data") or []
    if isinstance(contracts, dict):
        contracts = [contracts]
    allowed = {
        str(c.get("symbol"))
        for c in contracts
        if isinstance(c, dict) and c.get("symbol") and _is_usdt_m_crypto_perp(c, crypto_only)
    }

    tickers_resp = sess.get(f"{MEXC_REST}/api/v1/contract/ticker", timeout=timeout)
    tickers_resp.raise_for_status()
    tickers_payload = tickers_resp.json()
    tickers = tickers_payload.get("data") or []
    if isinstance(tickers, dict):
        tickers = [tickers]

    ranked: list[tuple[str, float]] = []
    for t in tickers:
        if not isinstance(t, dict):
            continue
        symbol = str(t.get("symbol") or "")
        if symbol not in allowed:
            continue
        last = float(t.get("lastPrice") or 0.0)
        if last > 0.0 and last < min_price_usdt:
            continue
        turnover = float(t.get("amount24") or 0.0)
        if turnover <= 0.0:
            turnover = float(t.get("volume24") or 0.0) * last
        if turnover < min_turnover_usdt:
            continue
        ranked.append((symbol, turnover))

    ranked.sort(key=lambda x: x[1], reverse=True)
    symbols = [s for s, _ in ranked[: max(1, top)]]
    if not symbols:
        print("Auto-universe empty — falling back to DEFAULT_SYMBOLS")
        return list(DEFAULT_SYMBOLS)
    print(f"Auto-universe: top {len(symbols)} liquid USDT-M symbols (min turnover ${min_turnover_usdt:,.0f})")
    for i, (sym, turn) in enumerate(ranked[: len(symbols)], 1):
        print(f"  {i:2d}. {sym}  turnover≈${turn:,.0f}")
    return symbols


def _parse_klines(symbol: str, data: dict) -> pd.DataFrame:
    times = data.get("time") or []
    if not times:
        return pd.DataFrame(
            columns=[
                "timestamp",
                "symbol",
                "open",
                "high",
                "low",
                "close",
                "volume",
                "quote_volume",
            ]
        )
    opens = data.get("open") or []
    highs = data.get("high") or []
    lows = data.get("low") or []
    closes = data.get("close") or []
    vols = data.get("vol") or []
    amounts = data.get("amount") or []

    rows = []
    for i, ts in enumerate(times):
        rows.append(
            {
                "timestamp": int(ts),
                "symbol": symbol,
                "open": float(opens[i]) if i < len(opens) else 0.0,
                "high": float(highs[i]) if i < len(highs) else 0.0,
                "low": float(lows[i]) if i < len(lows) else 0.0,
                "close": float(closes[i]) if i < len(closes) else 0.0,
                "volume": float(vols[i]) if i < len(vols) else 0.0,
                "quote_volume": float(amounts[i]) if i < len(amounts) else 0.0,
            }
        )
    return pd.DataFrame(rows)


def fetch_klines_chunk(
    symbol: str,
    interval: str,
    start: int | None = None,
    end: int | None = None,
    session: requests.Session | None = None,
    timeout: float = 30.0,
) -> pd.DataFrame:
    """Fetch one page of klines from MEXC futures REST."""
    sess = session or requests.Session()
    url = f"{MEXC_REST}/api/v1/contract/kline/{symbol}"
    params: dict[str, str | int] = {"interval": interval}
    if start is not None:
        params["start"] = int(start)
    if end is not None:
        params["end"] = int(end)
    resp = sess.get(url, params=params, timeout=timeout)
    resp.raise_for_status()
    payload = resp.json()
    if not payload.get("success", True) and payload.get("code", 0) != 0:
        raise RuntimeError(f"MEXC API error for {symbol} {interval}: {payload}")
    data = payload.get("data") or {}
    if not isinstance(data, dict):
        return pd.DataFrame()
    return _parse_klines(symbol, data)


def download_symbol(
    symbol: str,
    interval: str = "Min15",
    days: int = 180,
    end_ts: int | None = None,
    rate_limit_sec: float = 0.12,
    session: requests.Session | None = None,
) -> pd.DataFrame:
    """Paginate historical candles for one symbol/interval and merge with existing parquet."""
    ensure_dirs()
    if interval not in INTERVAL_SECONDS:
        raise ValueError(f"Unsupported interval {interval}; use one of {list(INTERVAL_SECONDS)}")

    sess = session or requests.Session()
    bar_sec = INTERVAL_SECONDS[interval]
    end = int(end_ts or time.time())
    start = end - int(days * 86400)

    out_path = raw_path(symbol, interval)
    existing = pd.DataFrame()
    if out_path.exists():
        existing = read_frame(out_path)
        if not existing.empty and "timestamp" in existing.columns:
            # Incremental: only fetch from last bar forward (minus overlap).
            last_ts = int(existing["timestamp"].max())
            start = max(start, last_ts - bar_sec * 5)

    chunks: list[pd.DataFrame] = []
    cursor = start
    # MEXC returns up to ~2000 bars; page by time windows.
    page_bars = 1500
    page_sec = page_bars * bar_sec

    while cursor < end:
        page_end = min(cursor + page_sec, end)
        df = fetch_klines_chunk(symbol, interval, start=cursor, end=page_end, session=sess)
        time.sleep(rate_limit_sec)
        if df.empty:
            cursor = page_end + bar_sec
            continue
        chunks.append(df)
        max_ts = int(df["timestamp"].max())
        if max_ts <= cursor:
            cursor = page_end + bar_sec
        else:
            cursor = max_ts + bar_sec

    if not chunks and existing.empty:
        return pd.DataFrame()

    frames = [existing] if not existing.empty else []
    frames.extend(chunks)
    merged = pd.concat(frames, ignore_index=True)
    merged = merged.drop_duplicates(subset=["timestamp"], keep="last")
    merged = merged.sort_values("timestamp").reset_index(drop=True)
    merged["symbol"] = symbol
    write_frame(merged, out_path)
    return merged


def download_universe(
    symbols: Iterable[str] | None = None,
    intervals: Iterable[str] | None = None,
    days: int = 180,
) -> dict[str, int]:
    """Download candles for many symbols/intervals. Returns {path: row_count}."""
    ensure_dirs()
    symbols = list(symbols or DEFAULT_SYMBOLS)
    intervals = list(intervals or INTERVALS)
    sess = requests.Session()
    results: dict[str, int] = {}
    for sym in symbols:
        for iv in intervals:
            print(f"Downloading {sym} {iv} ({days}d)...")
            try:
                df = download_symbol(sym, interval=iv, days=days, session=sess)
                path = str(raw_path(sym, iv))
                results[path] = len(df)
                print(f"  → {len(df)} bars → {path}")
            except Exception as exc:  # noqa: BLE001 — keep going on one bad symbol
                print(f"  ! failed {sym} {iv}: {exc}")
                results[f"{sym}_{iv}"] = -1
    return results


if __name__ == "__main__":
    download_universe(days=30, intervals=["Min15"])
