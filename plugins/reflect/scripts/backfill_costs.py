#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
backfill_costs.py — seed the reflect cost timeline from existing logs (W3).

The drainer only started recording the full token envelope going forward, so
``reflect cost`` would otherwise show no history. This one-off parses Claude
session transcripts (``~/.claude/projects/**/*.jsonl``), finds the ones that
were reflect runs (the drain prompt / `/reflect` signature), sums their token
buckets, and writes cost records to ``drain-cost-backfill.jsonl`` — a SEPARATE
file so the historical entries never inflate the live daily-cap counter (which
greps only ``drain-cost.jsonl``).

Window: last N days (decision #4 = 30d) by transcript mtime.

Usage:
    backfill_costs.py [--since 30d] [--projects-dir DIR] [--state-dir DIR]
                      [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

try:
    from reflect_gate import is_reflect_on_reflect, _iter_records
except Exception:  # pragma: no cover
    is_reflect_on_reflect = None  # type: ignore[assignment]
    _iter_records = None  # type: ignore[assignment]


def _since_seconds(s: str) -> float:
    s = (s or "30d").strip().lower()
    try:
        if s.endswith("d"):
            return int(s[:-1]) * 86400
        if s.endswith("h"):
            return int(s[:-1]) * 3600
        if s.endswith("w"):
            return int(s[:-1]) * 604800
        return int(s) * 86400
    except ValueError:
        return 30 * 86400


def _envelope(path: Path) -> dict | None:
    """Sum the token buckets across a transcript's assistant turns."""
    i = o = cr = cc = 0
    model = ""
    first_ts = last_ts = None
    for rec in _iter_records(path):
        ts = rec.get("timestamp")
        if ts:
            first_ts = first_ts or ts
            last_ts = ts
        msg = rec.get("message")
        if not isinstance(msg, dict):
            continue
        if msg.get("model"):
            model = msg["model"]
        u = msg.get("usage")
        if isinstance(u, dict):
            i += int(u.get("input_tokens", 0) or 0)
            o += int(u.get("output_tokens", 0) or 0)
            cr += int(u.get("cache_read_input_tokens", 0) or 0)
            cc += int(u.get("cache_creation_input_tokens", 0) or 0)
    total = i + o + cr + cc
    if total == 0:
        return None
    return {
        "input": i, "output": o, "cache_read": cr, "cache_creation": cc,
        "tokens": total, "model": model,
        "first_ts": first_ts, "last_ts": last_ts,
    }


def main() -> None:
    ap = argparse.ArgumentParser(description="Backfill reflect cost timeline")
    ap.add_argument("--since", default="30d")
    ap.add_argument("--projects-dir", default=str(Path.home() / ".claude" / "projects"))
    ap.add_argument("--state-dir", default="")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    if is_reflect_on_reflect is None or _iter_records is None:
        print("reflect_gate unavailable; cannot backfill", file=sys.stderr)
        sys.exit(1)

    import os
    sd = Path(args.state_dir).expanduser() if args.state_dir else \
        Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))
    sd.mkdir(parents=True, exist_ok=True)
    out_file = sd / "drain-cost-backfill.jsonl"

    projects = Path(args.projects_dir).expanduser()
    cutoff = time.time() - _since_seconds(args.since)

    scanned = matched = written = 0
    records: list[str] = []
    for jsonl in projects.glob("**/*.jsonl"):
        try:
            if jsonl.stat().st_mtime < cutoff:
                continue
        except OSError:
            continue
        scanned += 1
        try:
            if not is_reflect_on_reflect(jsonl):
                continue
        except Exception:
            continue
        matched += 1
        env = _envelope(jsonl)
        if env is None:
            continue
        day = (env["first_ts"] or "")[:10]
        rec = {
            "ts": env["first_ts"] or "",
            "day": day,
            "entries": 1,
            "transcript": str(jsonl),
            "outcome": "backfill",
            "source": "backfill",
            "model": env["model"],
            "turns": 0,
            "tokens": env["tokens"],
            "cost_usd": 0,
            "input": env["input"],
            "output": env["output"],
            "cache_read": env["cache_read"],
            "cache_creation": env["cache_creation"],
        }
        records.append(json.dumps(rec))
        written += 1

    if args.dry_run:
        print(f"[dry-run] scanned={scanned} reflect-runs={matched} would-write={written} -> {out_file}")
        return

    with open(out_file, "w", encoding="utf-8") as fh:
        fh.write("\n".join(records) + ("\n" if records else ""))
    print(f"backfill: scanned={scanned} reflect-runs={matched} written={written} -> {out_file}")


if __name__ == "__main__":
    main()
