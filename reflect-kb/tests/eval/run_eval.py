#!/usr/bin/env python3
# ABOUTME: CLI entry for the recall eval harness — builds hermetic KB, runs golden
# ABOUTME: queries, writes a results JSON and prints a summary table.
"""Run the recall eval.

Usage:
  python3 run_eval.py --label baseline                 # hermetic, builds tmp KB
  python3 run_eval.py --label after-R1 --keep-kb DIR   # reuse a previously built KB
  python3 run_eval.py --live --label live-smoke        # against the real ~/.learnings (report-only)

Results land in tests/eval/results/<label>.json (gitignored except baseline).
"""
from __future__ import annotations

import argparse
import json
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).parent
sys.path.insert(0, str(HERE))
from harness import EvalHarness  # noqa: E402

RESULTS = HERE / "results"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", required=True, help="results filename label, e.g. baseline / after-R1")
    ap.add_argument("--live", action="store_true", help="run against the real KB (report-only smoke)")
    ap.add_argument("--keep-kb", default=None, help="persistent workdir (reuse the indexed KB across runs)")
    ap.add_argument("--no-arms", action="store_true", help="skip per-arm attribution (faster)")
    ap.add_argument("--debug", action="store_true")
    args = ap.parse_args()

    RESULTS.mkdir(exist_ok=True)

    if args.keep_kb:
        workdir = Path(args.keep_kb)
        workdir.mkdir(parents=True, exist_ok=True)
        h = EvalHarness(workdir, live=args.live, debug=args.debug)
        if not args.live and not (workdir / "kb" / "nano_graphrag_cache" / "vdb_chunks.json").exists():
            print("[eval] building KB (first run in this workdir)…")
            h.build_kb()
        else:
            print("[eval] reusing existing KB index")
        report = h.run(with_arms=not args.no_arms)
    else:
        with tempfile.TemporaryDirectory(prefix="recall-eval-") as td:
            h = EvalHarness(Path(td), live=args.live, debug=args.debug)
            if not args.live:
                print("[eval] building hermetic KB…")
                h.build_kb()
            report = h.run(with_arms=not args.no_arms)

    out = RESULTS / f"{args.label}.json"
    out.write_text(json.dumps(report, indent=2))

    o = report["overall"]
    print(f"\n=== recall eval · {args.label} ===")
    print(f"queries:      {o['n_queries']}")
    print(f"R@5:          {o['recall_at_5']}")
    print(f"MRR:          {o['mrr']}")
    print(f"noise rate:   {o['noise_rate']}")
    print(f"latency p50:  {o['latency_p50_s']}s   p95: {o['latency_p95_s']}s")
    print("\nper class:")
    for cls, m in report["per_class"].items():
        print(f"  {cls:9} n={m['n']:2}  R@5={m['recall_at_5']}  MRR={m['mrr']}  noise={m['noise_rate']}")
    print(f"\nper-arm top-5 attribution: {report['per_arm_top5_attribution']}")
    print(f"\nresults -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
