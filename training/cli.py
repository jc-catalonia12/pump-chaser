"""CLI entry: python -m training <command>"""

from __future__ import annotations

import argparse
import json
import sys


def _add_universe_args(p: argparse.ArgumentParser, *, default_auto: bool = False) -> None:
    p.add_argument(
        "--symbols",
        nargs="+",
        default=None,
        help="Explicit symbols (e.g. BTC_USDT ETH_USDT). Use with --no-auto-universe to force.",
    )
    group = p.add_mutually_exclusive_group()
    group.add_argument(
        "--auto-universe",
        dest="auto_universe",
        action="store_true",
        help="Auto-pick top liquid USDT-M crypto perps from MEXC",
    )
    group.add_argument(
        "--no-auto-universe",
        dest="auto_universe",
        action="store_false",
        help="Do not auto-pick symbols (use --symbols or built-in majors)",
    )
    p.set_defaults(auto_universe=default_auto)
    p.add_argument(
        "--top",
        type=int,
        default=10,
        help="How many liquid symbols to take when using auto-universe (default 10)",
    )
    p.add_argument(
        "--min-turnover",
        type=float,
        default=500_000.0,
        help="Minimum 24h quote turnover (USDT) for auto-universe",
    )


def _resolve_symbols(args: argparse.Namespace) -> list[str]:
    from training.download import DEFAULT_SYMBOLS, fetch_liquid_universe

    if args.auto_universe:
        return fetch_liquid_universe(
            top=int(args.top or 10),
            min_turnover_usdt=float(args.min_turnover or 500_000.0),
        )
    if args.symbols:
        return list(args.symbols)
    return list(DEFAULT_SYMBOLS)


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="python -m training",
        description="Historical candle ML pipeline (V2.0.0) — offline training only",
    )
    sub = p.add_subparsers(dest="cmd", required=True)

    d = sub.add_parser("download", help="Download MEXC historical candles → data/raw/")
    _add_universe_args(d, default_auto=False)
    d.add_argument(
        "--intervals",
        nargs="+",
        default=["Min1", "Min5", "Min15", "Min60", "Hour4"],
    )
    d.add_argument("--days", type=int, default=180)

    f = sub.add_parser("features", help="Build features from raw candles")
    f.add_argument("--interval", default="Min15")

    l = sub.add_parser("labels", help="Generate triple-barrier labels")
    l.add_argument("--interval", default="Min15")
    l.add_argument("--tp", type=float, default=0.02)
    l.add_argument("--sl", type=float, default=0.01)
    l.add_argument("--horizon", type=int, default=48)

    ds = sub.add_parser("dataset", help="Join features+labels → datasets/training.csv.gz")
    ds.add_argument("--interval", default="Min15")
    ds.add_argument("--symbols", nargs="+", default=None)

    t = sub.add_parser("train", help="Walk-forward train + export candidate.onnx")
    t.add_argument("--folds", type=int, default=4)
    t.add_argument(
        "--promote",
        action="store_true",
        help="Promote candidate to production if metrics beat gates",
    )

    v = sub.add_parser("validate", help="Compare candidate metrics vs production (no train)")
    v.add_argument("--promote", action="store_true")

    sub.add_parser("deploy", help="Force-promote candidate → production (no metric gate)")

    u = sub.add_parser("universe", help="List top liquid MEXC USDT-M symbols (no download)")
    u.add_argument("--top", type=int, default=10)
    u.add_argument("--min-turnover", type=float, default=500_000.0)

    pipe = sub.add_parser(
        "pipeline",
        help="Easiest path: auto-universe → download → features → labels → train → promote",
    )
    _add_universe_args(pipe, default_auto=True)
    pipe.add_argument("--interval", default="Min15")
    pipe.add_argument("--days", type=int, default=180)
    pipe.add_argument("--folds", type=int, default=4)
    pipe.add_argument("--tp", type=float, default=0.02, help="Label take-profit fraction (default 2%%)")
    pipe.add_argument("--sl", type=float, default=0.01, help="Label stop-loss fraction (default 1%%)")
    pipe.add_argument("--horizon", type=int, default=48, help="Label look-ahead bars")
    pipe.add_argument("--download-all-tfs", action="store_true", help="Also download other TFs")
    pipe.add_argument(
        "--force-promote",
        action="store_true",
        help="Promote candidate even if it does not beat production gates",
    )
    pipe.add_argument(
        "--no-promote",
        action="store_true",
        help="Train + score only; never write production (for experiment grids)",
    )
    pipe.add_argument(
        "--meta",
        action="store_true",
        help="Use binary meta-label training (P(setup wins) × momentum side)",
    )
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    if args.cmd == "universe":
        from training.download import fetch_liquid_universe

        symbols = fetch_liquid_universe(top=args.top, min_turnover_usdt=args.min_turnover)
        print(json.dumps({"count": len(symbols), "symbols": symbols}, indent=2))
        return 0

    if args.cmd == "download":
        from training.download import download_universe

        symbols = _resolve_symbols(args)
        result = download_universe(
            symbols=symbols,
            intervals=args.intervals,
            days=args.days,
        )
        print(json.dumps({"symbols": symbols, "files": result}, indent=2))
        return 0

    if args.cmd == "features":
        from training.features import build_features_all

        build_features_all(interval=args.interval)
        return 0

    if args.cmd == "labels":
        from training.labels import build_labels_all

        build_labels_all(
            interval=args.interval,
            tp_pct=args.tp,
            sl_pct=args.sl,
            horizon_bars=args.horizon,
        )
        return 0

    if args.cmd == "dataset":
        from training.dataset import build_dataset

        build_dataset(interval=args.interval, symbols=args.symbols)
        return 0

    if args.cmd == "train":
        from training.walk_forward import run_walk_forward

        result = run_walk_forward(n_folds=args.folds, promote=args.promote)
        print(
            json.dumps(
                {k: v for k, v in result.items() if k != "production_metrics"},
                indent=2,
                default=str,
            )
        )
        return 0

    if args.cmd == "validate":
        from training.paths import CANDIDATE_METRICS
        from training.registry import load_production_metrics, promote_candidate
        from training.train import score_promotion

        if not CANDIDATE_METRICS.exists():
            print("No candidate.metrics.json — run train first", file=sys.stderr)
            return 1
        cand = json.loads(CANDIDATE_METRICS.read_text())
        agg = cand.get("aggregate", cand)
        prod = load_production_metrics()
        ok = score_promotion(agg, prod)
        print(json.dumps({"would_promote": ok, "candidate": agg, "production": prod}, indent=2))
        if args.promote and ok:
            promote_candidate(notes="validate --promote")
        return 0 if ok else 2

    if args.cmd == "deploy":
        from training.registry import promote_candidate

        promote_candidate(notes="forced deploy")
        return 0

    if args.cmd == "pipeline":
        from training.download import download_universe
        from training.dataset import build_dataset
        from training.features import build_features_all
        from training.labels import build_labels_all
        from training.walk_forward import run_walk_forward

        symbols = _resolve_symbols(args)
        print(f"Pipeline symbols ({len(symbols)}): {', '.join(symbols)}")
        print(f"Label barriers: TP={args.tp:.2%} SL={args.sl:.2%} horizon={args.horizon} bars")
        intervals = (
            ["Min1", "Min5", "Min15", "Min60", "Hour4"]
            if args.download_all_tfs
            else [args.interval]
        )
        download_universe(symbols=symbols, intervals=intervals, days=args.days)
        build_features_all(interval=args.interval, symbols=symbols)
        build_labels_all(
            interval=args.interval,
            symbols=symbols,
            tp_pct=args.tp,
            sl_pct=args.sl,
            horizon_bars=args.horizon,
        )
        build_dataset(interval=args.interval, symbols=symbols)
        allow_promote = not args.no_promote
        if getattr(args, "meta", False):
            from training.meta_train import run_meta_walk_forward

            result = run_meta_walk_forward(n_folds=args.folds, promote=allow_promote)
        else:
            result = run_walk_forward(n_folds=args.folds, promote=allow_promote)
        if allow_promote and not result.get("promoted") and (
            result.get("would_promote") or args.force_promote
        ):
            from training.registry import promote_candidate

            note = (
                "pipeline --force-promote"
                if args.force_promote and not result.get("would_promote")
                else "pipeline would_promote follow-up"
            )
            promote_candidate(notes=note)
            result["promoted"] = True
        elif not result.get("promoted"):
            if args.no_promote:
                print("Candidate kept only — --no-promote (experiment mode)")
            else:
                print(
                    "Candidate kept only — did not beat production "
                    "(use --force-promote to override)"
                )
        print(json.dumps(result, indent=2, default=str))
        return 0

    return 1

if __name__ == "__main__":
    raise SystemExit(main())
