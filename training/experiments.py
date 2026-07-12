"""Controlled experiment grid for historical ML (paper-only; never enables live).

Uses the existing env/ interpreter via: env/bin/python -m training.experiments
"""

from __future__ import annotations

import json
import subprocess
import sys
from datetime import datetime, timezone
from training.paths import CANDIDATE_METRICS, MODELS, PRODUCTION_METRICS, ensure_dirs
from training.registry import load_production_metrics
from training.train import score_promotion

MAJORS = ["BTC_USDT", "ETH_USDT", "SOL_USDT"]

# Min15 experiments may promote if they beat production (live HTF is Min15).
# Min60 is scored only — promoting would misalign with scanner htf_interval.
EXPERIMENTS: list[dict] = [
    {
        "id": "majors_min15_tp1.5_sl0.8",
        "interval": "Min15",
        "tp": 0.015,
        "sl": 0.008,
        "horizon": 48,
        "allow_promote": True,
    },
    {
        "id": "majors_min15_tp2_sl1",
        "interval": "Min15",
        "tp": 0.02,
        "sl": 0.01,
        "horizon": 48,
        "allow_promote": True,
    },
    {
        "id": "majors_min60_tp2_sl1",
        "interval": "Min60",
        "tp": 0.02,
        "sl": 0.01,
        "horizon": 24,
        "allow_promote": False,
    },
    {
        "id": "majors_min60_tp3_sl1.5",
        "interval": "Min60",
        "tp": 0.03,
        "sl": 0.015,
        "horizon": 24,
        "allow_promote": False,
    },
]

SUMMARY_PATH = MODELS / "experiments_summary.json"


def _pipeline_cmd(exp: dict, *, days: int, folds: int) -> list[str]:
    cmd = [
        sys.executable,
        "-m",
        "training",
        "pipeline",
        "--no-auto-universe",
        "--symbols",
        *MAJORS,
        "--interval",
        exp["interval"],
        "--days",
        str(days),
        "--folds",
        str(folds),
        "--tp",
        str(exp["tp"]),
        "--sl",
        str(exp["sl"]),
        "--horizon",
        str(exp["horizon"]),
    ]
    if not exp["allow_promote"]:
        cmd.append("--no-promote")
    return cmd


def run_grid(*, days: int = 180, folds: int = 4, max_runs: int | None = None) -> dict:
    ensure_dirs()
    production_before = load_production_metrics()
    prod_trained_before = (production_before or {}).get("trained_at")
    runs: list[dict] = []
    experiments = EXPERIMENTS[: max_runs or len(EXPERIMENTS)]

    print(f"Experiment grid: {len(experiments)} runs (paper-only, never enables live)")
    print(f"Python: {sys.executable}")
    print(f"Production before: {json.dumps(production_before, indent=2)}")

    for i, exp in enumerate(experiments, start=1):
        print("\n" + "=" * 72)
        print(f"[{i}/{len(experiments)}] {exp['id']}")
        print("=" * 72)
        cmd = _pipeline_cmd(exp, days=days, folds=folds)
        print(" ".join(cmd))
        # Score against production *before* this run so would_promote is meaningful
        # even if a later promote updates production.
        prod_snapshot = load_production_metrics()
        proc = subprocess.run(cmd, capture_output=False)
        entry: dict = {
            "id": exp["id"],
            "config": exp,
            "exit_code": proc.returncode,
            "finished_at": datetime.now(timezone.utc).isoformat(),
        }
        if CANDIDATE_METRICS.exists():
            cand = json.loads(CANDIDATE_METRICS.read_text())
            agg = cand.get("aggregate", cand)
            entry["aggregate"] = agg
            entry["would_promote"] = score_promotion(agg, prod_snapshot)
            prod_after = load_production_metrics()
            entry["promoted"] = (
                exp["allow_promote"]
                and (prod_after or {}).get("trained_at") != prod_trained_before
                and (prod_after or {}).get("trained_at") == agg.get("trained_at")
            )
            if entry["promoted"]:
                prod_trained_before = (prod_after or {}).get("trained_at")
            entry["production_after"] = prod_after
        else:
            entry["error"] = "missing candidate.metrics.json"
        runs.append(entry)
        _write_summary(production_before, runs)

    summary = _write_summary(production_before, runs)
    _print_ranking(summary)
    return summary


def _write_summary(production_before: dict | None, runs: list[dict]) -> dict:
    ranked = sorted(
        [r for r in runs if isinstance(r.get("aggregate"), dict)],
        key=lambda r: (
            r["aggregate"].get("avg_r_proxy", -999),
            r["aggregate"].get("precision_tradeable", 0),
            r["aggregate"].get("f1_macro", 0),
        ),
        reverse=True,
    )
    summary = {
        "created_at": datetime.now(timezone.utc).isoformat(),
        "live_trading": False,
        "production_before": production_before,
        "production_after": load_production_metrics(),
        "runs": runs,
        "best_by_avg_r": ranked[0]["id"] if ranked else None,
        "ranking": [
            {
                "id": r["id"],
                "avg_r_proxy": r["aggregate"].get("avg_r_proxy"),
                "precision_tradeable": r["aggregate"].get("precision_tradeable"),
                "f1_macro": r["aggregate"].get("f1_macro"),
                "would_promote": r.get("would_promote"),
            }
            for r in ranked
        ],
    }
    SUMMARY_PATH.write_text(json.dumps(summary, indent=2, default=str))
    print(f"\nWrote {SUMMARY_PATH}")
    return summary


def _print_ranking(summary: dict) -> None:
    print("\n" + "=" * 72)
    print("EXPERIMENT SUMMARY (paper only — live remains disabled)")
    print("=" * 72)
    for row in summary.get("ranking", []):
        print(
            f"  {row['id']}: avg_r={row['avg_r_proxy']:.4f} "
            f"prec_trade={row['precision_tradeable']:.3f} "
            f"f1={row['f1_macro']:.3f} "
            f"would_promote={row['would_promote']}"
        )
    print(f"Best by avg_r_proxy: {summary.get('best_by_avg_r')}")
    before = summary.get("production_before") or {}
    after = summary.get("production_after") or {}
    if before.get("precision_tradeable") != after.get("precision_tradeable"):
        print("Production metrics changed (a Min15 candidate was promoted).")
    else:
        print("Production unchanged (no experiment beat promotion gates).")


def main() -> int:
    import argparse

    p = argparse.ArgumentParser(description="Run controlled ML experiment grid")
    p.add_argument("--days", type=int, default=180)
    p.add_argument("--folds", type=int, default=4)
    p.add_argument("--max-runs", type=int, default=None, help="Stop after N experiments")
    args = p.parse_args()
    run_grid(days=args.days, folds=args.folds, max_runs=args.max_runs)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
