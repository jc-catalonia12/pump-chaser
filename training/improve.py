"""Keep training variants until a paper-quality model is promoted.

Usage:
  ./env/bin/python -m training.improve
"""

from __future__ import annotations

import json
import subprocess
import sys
from datetime import datetime, timezone

from training.paths import MODELS, ensure_dirs
from training.registry import load_production_metrics
from training.train import score_promotion

GOAL = {
    "avg_r_proxy": 0.0,
    "final_holdout_avg_r_proxy": 0.05,
    "final_holdout_precision_tradeable": 0.36,
    "tradeable_rate": 0.03,
}

VARIANTS: list[dict] = [
    {
        "id": "meta_top10_tp2_sl1",
        "args": [
            "--meta",
            "--auto-universe",
            "--top",
            "10",
            "--interval",
            "Min15",
            "--days",
            "180",
            "--folds",
            "5",
            "--tp",
            "0.02",
            "--sl",
            "0.01",
            "--horizon",
            "48",
        ],
    },
    {
        "id": "meta_liquid6_tp2_sl1",
        "args": [
            "--meta",
            "--no-auto-universe",
            "--symbols",
            "WLD_USDT",
            "ZEC_USDT",
            "HYPE_USDT",
            "TAO_USDT",
            "SOL_USDT",
            "SUI_USDT",
            "--interval",
            "Min15",
            "--days",
            "180",
            "--folds",
            "5",
            "--tp",
            "0.02",
            "--sl",
            "0.01",
            "--horizon",
            "40",
        ],
    },
    {
        "id": "meta_top8_tp1.5_sl0.75",
        "args": [
            "--meta",
            "--auto-universe",
            "--top",
            "8",
            "--interval",
            "Min15",
            "--days",
            "180",
            "--folds",
            "5",
            "--tp",
            "0.015",
            "--sl",
            "0.0075",
            "--horizon",
            "36",
        ],
    },
    {
        "id": "meta_top12_days120",
        "args": [
            "--meta",
            "--auto-universe",
            "--top",
            "12",
            "--interval",
            "Min15",
            "--days",
            "120",
            "--folds",
            "4",
            "--tp",
            "0.02",
            "--sl",
            "0.01",
            "--horizon",
            "36",
        ],
    },
    {
        "id": "meta_majors_240d",
        "args": [
            "--meta",
            "--no-auto-universe",
            "--symbols",
            "BTC_USDT",
            "ETH_USDT",
            "SOL_USDT",
            "--interval",
            "Min15",
            "--days",
            "240",
            "--folds",
            "5",
            "--tp",
            "0.02",
            "--sl",
            "0.01",
            "--horizon",
            "48",
        ],
    },
]

SUMMARY = MODELS / "improve_summary.json"


def meets_goal(agg: dict) -> bool:
    hold_r = agg.get("final_holdout_avg_r_proxy", agg.get("avg_r_proxy", -999))
    hold_p = agg.get(
        "final_holdout_precision_tradeable", agg.get("precision_tradeable", 0)
    )
    return (
        hold_r > GOAL["final_holdout_avg_r_proxy"]
        and hold_p >= GOAL["final_holdout_precision_tradeable"]
        and agg.get("tradeable_rate", 0) >= GOAL["tradeable_rate"]
        and agg.get("avg_r_proxy", -999) > GOAL["avg_r_proxy"]
    )


def main() -> int:
    ensure_dirs()
    runs: list[dict] = []
    print(f"Improve loop using {sys.executable}")
    print(f"Goal: {GOAL}")
    print("Live trading will NOT be enabled.")

    for i, variant in enumerate(VARIANTS, start=1):
        print("\n" + "=" * 72)
        print(f"[{i}/{len(VARIANTS)}] {variant['id']}")
        print("=" * 72)
        cmd = [sys.executable, "-m", "training", "pipeline", *variant["args"]]
        print(" ".join(cmd))
        proc = subprocess.run(cmd)
        cand_path = MODELS / "candidate.metrics.json"
        entry: dict = {
            "id": variant["id"],
            "exit_code": proc.returncode,
            "finished_at": datetime.now(timezone.utc).isoformat(),
        }
        if cand_path.exists():
            cand = json.loads(cand_path.read_text())
            agg = cand.get("aggregate", cand)
            entry["aggregate"] = agg
            entry["meets_goal"] = meets_goal(agg)
            entry["would_promote"] = score_promotion(agg, load_production_metrics())
            prod = load_production_metrics()
            entry["promoted"] = bool(prod and prod.get("trained_at") == agg.get("trained_at"))
            print(
                f"→ avg_r={agg.get('avg_r_proxy', 0):+.3f} "
                f"hold_r={agg.get('final_holdout_avg_r_proxy', 0):+.3f} "
                f"hold_prec={agg.get('final_holdout_precision_tradeable', 0):.3f} "
                f"rate={agg.get('tradeable_rate', 0):.3f} "
                f"thr={agg.get('confidence_threshold', 0):.2f} "
                f"goal={entry['meets_goal']} promoted={entry['promoted']}"
            )
        else:
            entry["error"] = "no candidate metrics"
        runs.append(entry)
        SUMMARY.write_text(
            json.dumps(
                {
                    "goal": GOAL,
                    "live_trading": False,
                    "runs": runs,
                    "updated_at": datetime.now(timezone.utc).isoformat(),
                },
                indent=2,
                default=str,
            )
        )
        if entry.get("meets_goal") and entry.get("promoted"):
            print("\nGOAL REACHED — model promoted for paper use. Live still disabled.")
            return 0
        if entry.get("meets_goal") and not entry.get("promoted"):
            from training.registry import promote_candidate

            promote_candidate(notes=f"improve loop goal met: {variant['id']}")
            print("\nGOAL REACHED — forced promote after absolute quality bar.")
            return 0

    print("\nAll variants finished without meeting goal. See models/improve_summary.json")
    best = max(
        (r for r in runs if "aggregate" in r),
        key=lambda r: (
            r["aggregate"].get("final_holdout_avg_r_proxy", -999),
            r["aggregate"].get("avg_r_proxy", -999),
        ),
        default=None,
    )
    if best:
        a = best["aggregate"]
        print(
            f"Best so far: {best['id']} hold_r={a.get('final_holdout_avg_r_proxy', 0):+.3f} "
            f"avg_r={a.get('avg_r_proxy', 0):+.3f}"
        )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
