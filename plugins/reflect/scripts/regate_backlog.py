#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
regate_backlog.py — apply the W2 gate + dedup to the EXISTING queue (W5, decision #6).

The pending queue accumulated entries under the old (no-gate, no-dedup) regime —
~113 at the time of the rearchitecture, most of them reflect-on-reflect,
no-signal, or duplicates. This one-shot re-runs every queued entry through the
same gate the producers now use and rewrites the queue with only the survivors,
so the drainer doesn't spend model calls clearing worthless backlog.

Always archives the original queue first (never destructive).

CLI:
    regate_backlog.py [--state-dir DIR] [--dry-run]
"""

from __future__ import annotations

import json
import os
import sys
from collections import Counter
from pathlib import Path

_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

import reflect_gate  # noqa: E402


def state_dir(override: str = "") -> Path:
    if override:
        return Path(override).expanduser()
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def regate(sd: Path, *, dry_run: bool = False) -> dict:
    queue = sd / "pending_reflections.jsonl"
    cost = sd / "drain-cost.jsonl"
    if not queue.exists():
        return {"total": 0, "kept": 0, "dropped": 0, "reasons": {}}

    entries = []
    for line in queue.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError:
            continue

    kept: list[dict] = []
    seen_paths: set[str] = set()
    reasons: Counter = Counter()
    for e in entries:
        tp = e.get("transcript_path", "")
        rp = reflect_gate._resolved(tp)
        if rp in seen_paths:
            reasons["dup-in-queue"] += 1
            continue
        # Evaluate against the cost log (already-processed) + the gate verdict.
        # Use a throwaway empty queue path so already_queued doesn't self-match.
        if reflect_gate.already_processed(tp, cost):
            reasons["dup-already-processed"] += 1
            continue
        verdict = reflect_gate.evaluate(tp)
        if verdict.action == "skip":
            reasons[verdict.reason] += 1
            continue
        seen_paths.add(rp)
        kept.append(e)
        reasons["kept:" + verdict.reason] += 1

    result = {
        "total": len(entries),
        "kept": len(kept),
        "dropped": len(entries) - len(kept),
        "reasons": dict(reasons),
    }

    if dry_run:
        return result

    # Archive original, then rewrite with survivors.
    archive = queue.with_suffix(f".jsonl.pre-regate")
    archive.write_text("\n".join(json.dumps(e) for e in entries) + ("\n" if entries else ""),
                       encoding="utf-8")
    queue.write_text("\n".join(json.dumps(e) for e in kept) + ("\n" if kept else ""),
                     encoding="utf-8")
    result["archived_to"] = str(archive)
    return result


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Re-gate the pending reflection backlog")
    ap.add_argument("--state-dir", default="")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    sd = state_dir(args.state_dir)
    res = regate(sd, dry_run=args.dry_run)
    prefix = "[dry-run] " if args.dry_run else ""
    print(f"{prefix}backlog re-gate: total={res['total']} kept={res['kept']} "
          f"dropped={res['dropped']}")
    for reason, n in sorted(res["reasons"].items(), key=lambda kv: -kv[1]):
        print(f"    {reason}: {n}")
    if not args.dry_run and res.get("archived_to"):
        print(f"  original archived -> {res['archived_to']}")


if __name__ == "__main__":
    main()
