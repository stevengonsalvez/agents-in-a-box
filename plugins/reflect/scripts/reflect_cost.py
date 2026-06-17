#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
reflect cost — observability for drain spend (W3).

Reads the drainer's cost log (``~/.reflect/drain-cost.jsonl``, enriched by
reflect-drain-bg.sh with the full token envelope) plus an optional backfill
file (``drain-cost-backfill.jsonl``, written by backfill_costs.py) and reports
spend over time. Tokens are the hard data; $ is authoritative where the drainer
recorded ``cost_usd`` from ``claude -p`` and an *estimate* otherwise.

Usage:
    reflect_cost.py [--since 30d] [--by day|transcript|model|outcome|writer]
                    [--top N] [--json] [--state-dir DIR]
                    [--followup] [--metrics-path FILE] [--quota]

``--by writer`` groups on the M2 writer-output classification
(``writer_class``: valid/prose/idle/poisoned/malformed) recorded per run by
the drainer — the writer-health view. Pre-M2 events show as ``?``.

``--quota`` (M3) switches to the subscription-quota view: the per-window
rate-limit snapshot the drainer ingested from its ``claude -p`` runs
(``quota-state.json``) plus whether the writer gate is currently open or
closed and any standing 'quota_near_limit' deferral. Reads disk state only —
never issues an API call.

``--followup`` (A4) switches to the recall-quality diagnostic: reads the
op="recall_search" lines recall.py appends to ``~/.learnings/metrics.jsonl``
(override with --metrics-path / $REFLECT_METRICS_PATH) and reports the
followup rate — the share of searches where the SAME session searched again
within the window (default 30s) and got a fully disjoint result set, i.e.
recall didn't satisfy the first time. High rate = tune rerank weights / graph
arm budget / OOD threshold. Directional: rapid topic switches may overcount.

Cached-vs-uncached framing (the 2026-05-31 lesson):
    cache_read     = cheap reuse (0.1x)   — what SHOULD dominate
    cache_creation = expensive writes (1.25–2x) — re-paid when caching fails
    io             = input + output
A healthy run is cache_read-heavy; the 41.5M incident was creation-heavy.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path


# Approximate Anthropic list prices, USD per 1M tokens, used ONLY to estimate
# cost for events that have no recorded cost_usd (e.g. backfill from raw logs).
# These are ballpark figures (knowledge cutoff Jan 2026) and may be stale —
# treat the $est column as an order-of-magnitude guide, not a bill. The drainer
# records the authoritative cost_usd from `claude -p` going forward.
_PRICING = {  # (input, output, cache_read, cache_write) per 1M tokens
    "opus":   (15.0, 75.0, 1.50, 18.75),
    "sonnet": (3.0, 15.0, 0.30, 3.75),
    "haiku":  (0.80, 4.0, 0.08, 1.00),
}
_DEFAULT_PRICE = _PRICING["sonnet"]


def _price_for(model: str):
    m = (model or "").lower()
    for key, price in _PRICING.items():
        if key in m:
            return price
    return _DEFAULT_PRICE


def _est_cost(e: dict) -> float:
    """Estimate $ from token buckets when the event has no recorded cost."""
    pin, pout, pcr, pcw = _price_for(str(e.get("model", "")))
    return (
        _int(e, "input") * pin
        + _int(e, "output") * pout
        + _int(e, "cache_read") * pcr
        + _int(e, "cache_creation") * pcw
    ) / 1_000_000


def state_dir(override: str = "") -> Path:
    import os
    if override:
        return Path(override).expanduser()
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def metrics_path(override: str = "") -> Path:
    """A4: resolve the recall metrics log (the engine's metrics.jsonl)."""
    import os
    if override:
        return Path(override).expanduser()
    env = (os.environ.get("REFLECT_METRICS_PATH") or "").strip()
    if env:
        return Path(env).expanduser()
    return Path.home() / ".learnings" / "metrics.jsonl"


def _load_jsonl(path: Path) -> list[dict]:
    """Tolerant JSONL loader — skips blank/corrupt lines, never raises."""
    events: list[dict] = []
    if not path.exists():
        return events
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                parsed = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(parsed, dict):
                events.append(parsed)
    return events


def followup_stats(events: list[dict]) -> dict:
    """A4: aggregate op="recall_search" lines into the followup-rate view."""
    searches = [e for e in events if e.get("op") == "recall_search"]
    followups = sum(1 for e in searches if e.get("followup"))
    total = len(searches)
    windows = [
        e.get("window_seconds") for e in searches
        if isinstance(e.get("window_seconds"), (int, float))
    ]
    return {
        "searches": total,
        "followups": followups,
        "rate": (followups / total) if total else 0.0,
        "window_seconds": windows[-1] if windows else 30.0,
    }


def render_followup(stats: dict, since: str) -> str:
    """A4: one-screen followup-rate report."""
    total = stats["searches"]
    if not total:
        return (
            f"No tracked recall searches in the last {since}.\n"
            "(recall.py records op=recall_search lines only for session-"
            "anchored, non-empty searches — run some recalls first.)"
        )
    pct = 100.0 * stats["rate"]
    lines = [
        f"recall followup rate — last {since}",
        "",
        f"  searches tracked : {total}",
        f"  followups        : {stats['followups']} "
        f"(re-search within {stats['window_seconds']:.0f}s, disjoint results)",
        f"  followup rate    : {pct:.0f}%",
        "",
        "High rate = the first recall didn't satisfy — tune rerank weights, "
        "give the graph arm more budget, or lower the OOD threshold. "
        "Directional only: rapid topic switches can overcount.",
    ]
    return "\n".join(lines)


def _parse_since(s: str) -> timedelta | None:
    if not s:
        return None
    s = s.strip().lower()
    try:
        if s.endswith("d"):
            return timedelta(days=int(s[:-1]))
        if s.endswith("h"):
            return timedelta(hours=int(s[:-1]))
        if s.endswith("w"):
            return timedelta(weeks=int(s[:-1]))
        return timedelta(days=int(s))
    except ValueError:
        return None


def _load_events(sd: Path) -> list[dict]:
    events: list[dict] = []
    for name in ("drain-cost.jsonl", "drain-cost-backfill.jsonl"):
        f = sd / name
        if not f.exists():
            continue
        with open(f, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    events.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    return events


def _event_ts(e: dict) -> datetime | None:
    raw = e.get("ts", "")
    if not raw:
        return None
    try:
        dt = datetime.fromisoformat(raw.replace("Z", "+00:00"))
        return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)
    except ValueError:
        return None


def _int(e: dict, k: str) -> int:
    try:
        return int(e.get(k, 0) or 0)
    except (TypeError, ValueError):
        return 0


def _float(e: dict, k: str) -> float:
    try:
        return float(e.get(k, 0) or 0)
    except (TypeError, ValueError):
        return 0.0


def _bucket_key(e: dict, by: str) -> str:
    if by == "day":
        return str(e.get("day") or (e.get("ts", "")[:10]) or "?")
    if by == "transcript":
        return Path(str(e.get("transcript", "?"))).name or "?"
    if by == "model":
        return str(e.get("model") or "?")
    if by == "outcome":
        return str(e.get("outcome") or "?")
    if by == "writer":
        return str(e.get("writer_class") or "?")
    return "?"


def _fmt(n: int) -> str:
    if n >= 1_000_000:
        return f"{n/1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n/1_000:.0f}K"
    return str(n)


def aggregate(events: list[dict], by: str) -> dict[str, dict]:
    agg: dict[str, dict] = defaultdict(
        lambda: {"runs": 0, "tokens": 0, "cache_read": 0, "cache_creation": 0,
                 "io": 0, "cost": 0.0, "max_run_tokens": 0}
    )
    for e in events:
        k = _bucket_key(e, by)
        row = agg[k]
        row["runs"] += 1
        tok = _int(e, "tokens")
        cr = _int(e, "cache_read")
        cc = _int(e, "cache_creation")
        io = _int(e, "input") + _int(e, "output")
        row["tokens"] += tok
        row["cache_read"] += cr
        row["cache_creation"] += cc
        row["io"] += io
        # Prefer the authoritative recorded cost; fall back to an estimate.
        recorded = _float(e, "cost_usd")
        row["cost"] += recorded if recorded > 0 else _est_cost(e)
        row["max_run_tokens"] = max(row["max_run_tokens"], tok)
    return agg


def render(agg: dict[str, dict], by: str, top: int, outlier_tokens: int) -> str:
    keys = sorted(agg.keys())
    if by in ("transcript", "model", "outcome", "writer"):
        keys = sorted(agg.keys(), key=lambda k: agg[k]["tokens"], reverse=True)
        if top:
            keys = keys[:top]

    header = f"{by:<22} {'runs':>5} {'tokens':>9} {'cache_rd':>9} {'cache_wr':>9} {'io':>7} {'$est':>8}"
    lines = [header, "─" * len(header)]
    tot = {"runs": 0, "tokens": 0, "cache_read": 0, "cache_creation": 0, "io": 0, "cost": 0.0}
    flagged = []
    for k in keys:
        r = agg[k]
        for f in tot:
            tot[f] += r[f]
        flag = "  ⚠" if r["max_run_tokens"] > outlier_tokens else ""
        lines.append(
            f"{k:<22} {r['runs']:>5} {_fmt(r['tokens']):>9} {_fmt(r['cache_read']):>9} "
            f"{_fmt(r['cache_creation']):>9} {_fmt(r['io']):>7} {r['cost']:>7.2f}{flag}"
        )
        if r["max_run_tokens"] > outlier_tokens:
            flagged.append((k, r["max_run_tokens"]))

    lines.append("─" * len(header))
    cached_pct = (100 * tot["cache_read"] / tot["tokens"]) if tot["tokens"] else 0
    lines.append(
        f"{'TOTAL':<22} {tot['runs']:>5} {_fmt(tot['tokens']):>9} {_fmt(tot['cache_read']):>9} "
        f"{_fmt(tot['cache_creation']):>9} {_fmt(tot['io']):>7} {tot['cost']:>7.2f}"
    )
    lines.append("")
    lines.append(
        f"cache reuse: {cached_pct:.0f}% of tokens were cache reads "
        f"(low % + high cache_wr = caching not amortizing — the 41.5M failure mode)"
    )
    if flagged:
        lines.append("")
        lines.append(f"⚠ outlier runs (> {_fmt(outlier_tokens)} tokens in one run):")
        for k, mx in flagged:
            lines.append(f"    {k}: {_fmt(mx)}")
    return "\n".join(lines)


def main() -> None:
    ap = argparse.ArgumentParser(description="reflect drain cost report")
    ap.add_argument("--since", default="30d", help="window, e.g. 30d / 7d / 24h")
    ap.add_argument("--by", default="day", choices=["day", "transcript", "model", "outcome", "writer"])
    ap.add_argument("--top", type=int, default=15, help="limit rows for transcript/model/outcome/writer")
    ap.add_argument("--outlier-tokens", type=int, default=5_000_000,
                    help="flag any single run above this many tokens")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--state-dir", default="")
    ap.add_argument("--followup", action="store_true",
                    help="A4: report the recall followup rate from "
                         "metrics.jsonl instead of drain spend")
    ap.add_argument("--metrics-path", default="",
                    help="A4: metrics.jsonl location (default "
                         "~/.learnings/metrics.jsonl or $REFLECT_METRICS_PATH)")
    ap.add_argument("--quota", action="store_true",
                    help="M3: report the subscription-quota windows and "
                         "writer-gate state instead of drain spend")
    args = ap.parse_args()

    window = _parse_since(args.since)

    if args.quota:  # M3: subscription-quota view
        sd = state_dir(args.state_dir)
        try:
            sys.path.insert(0, str(Path(__file__).resolve().parent))
            import quota_store  # noqa: PLC0415
        except ImportError:
            print("quota_store.py not found — update the reflect plugin (M3+).")
            return
        if args.json:
            print(json.dumps(quota_store.status_payload(sd), indent=2))
        else:
            print(quota_store.render_status(sd))
        return

    if args.followup:  # A4: recall-quality diagnostic view
        mp = metrics_path(args.metrics_path)
        metric_events = _load_jsonl(mp)
        if window is not None:
            cutoff = datetime.now(timezone.utc) - window
            metric_events = [
                e for e in metric_events
                if (_event_ts(e) or datetime.min.replace(tzinfo=timezone.utc))
                >= cutoff
            ]
        stats = followup_stats(metric_events)
        if args.json:
            print(json.dumps(stats, indent=2))
        else:
            print(render_followup(stats, args.since))
        return

    sd = state_dir(args.state_dir)
    events = _load_events(sd)

    if window is not None:
        cutoff = datetime.now(timezone.utc) - window
        events = [e for e in events if (_event_ts(e) or datetime.min.replace(tzinfo=timezone.utc)) >= cutoff]

    if not events:
        print(f"No reflect cost events in the last {args.since} (looked in {sd}).")
        return

    agg = aggregate(events, args.by)

    if args.json:
        print(json.dumps({k: dict(v) for k, v in agg.items()}, indent=2))
        return

    print(f"reflect cost — last {args.since}, by {args.by}  ({len(events)} events, {sd})\n")
    print(render(agg, args.by, args.top, args.outlier_tokens))
    print("\n($est for events without a recorded cost is approximate: token "
          "buckets × ballpark list prices — order-of-magnitude, not a bill.)")


if __name__ == "__main__":
    main()
