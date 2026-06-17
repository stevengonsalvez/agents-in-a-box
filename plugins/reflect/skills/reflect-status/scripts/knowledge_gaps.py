#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# ABOUTME: SG6 aggregator over ~/.reflect/knowledge-gaps.jsonl (0-result recalls
# ABOUTME: logged by recall.py) — surfaces queries empty in >=2 sessions for /reflect:status.
"""
Knowledge-gap aggregator (SG6: negative recall as knowledge-gap signal).

recall.py appends every 0-result recall to ``~/.reflect/knowledge-gaps.jsonl``
as ``{ts, query, normalized, session_id}``. This script groups the entries by
``normalized`` query (lowercased, stopword-filtered, sorted content terms — so
word-order variants of the same ask dedup into one gap), counts DISTINCT
sessions per gap, and surfaces those asked in >= --min-sessions sessions:

    knowledge gap — users keep asking about X with no learnings

That list is the KB's curation backlog: write learnings covering these topics
and the gaps stop recurring.

Usage:
    python3 knowledge_gaps.py                 # markdown report (>=2 sessions)
    python3 knowledge_gaps.py --min-sessions 1   # include one-off gaps
    python3 knowledge_gaps.py --format json      # machine-readable

Exit code is always 0 — a missing/corrupt log is an empty report, never an
error (read-only status surface; same never-block contract as the hooks).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

DEFAULT_MIN_SESSIONS = 2
GAPS_FILENAME = "knowledge-gaps.jsonl"


def gaps_path() -> Path:
    """The jsonl recall.py appends to. Honors REFLECT_STATE_DIR (tests)."""
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    return base / GAPS_FILENAME


@dataclass
class Gap:
    """One normalized query and every 0-result ask of it."""

    normalized: str
    query: str = ""  # most recent raw phrasing — what humans read
    sessions: set[str] = field(default_factory=set)
    asks: int = 0
    first_ts: str = ""
    last_ts: str = ""

    @property
    def session_count(self) -> int:
        return len(self.sessions)

    def to_dict(self) -> dict[str, Any]:
        return {
            "normalized": self.normalized,
            "query": self.query,
            "sessions": self.session_count,
            "asks": self.asks,
            "first_ts": self.first_ts,
            "last_ts": self.last_ts,
        }


def load_entries(path: Path) -> list[dict[str, Any]]:
    """Parse the jsonl, skipping malformed lines — the log is append-only
    from multiple silent-fail writers, so a torn line must not kill the
    whole report."""
    try:
        raw = path.read_text(encoding="utf-8")
    except OSError:
        return []
    entries: list[dict[str, Any]] = []
    for line in raw.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(entry, dict) and (entry.get("normalized") or entry.get("query")):
            entries.append(entry)
    return entries


def aggregate(entries: list[dict[str, Any]]) -> list[Gap]:
    """Group entries by normalized query; count distinct sessions per gap.

    Entries written before the ``normalized`` field existed (or hand-edited
    ones) fall back to a whitespace-collapsed lowercase of the raw query —
    weaker dedup, but they still count. Returned sorted by session count
    desc, then last-seen desc (hottest gaps first, deterministic).
    """
    gaps: dict[str, Gap] = {}
    for entry in entries:
        raw_query = str(entry.get("query", "") or "")
        normalized = str(entry.get("normalized", "") or "")
        if not normalized:
            normalized = " ".join(raw_query.lower().split())
        if not normalized:
            continue
        gap = gaps.setdefault(normalized, Gap(normalized=normalized))
        gap.asks += 1
        session = str(entry.get("session_id", "") or "unknown")
        gap.sessions.add(session)
        ts = str(entry.get("ts", "") or "")
        if ts and (not gap.first_ts or ts < gap.first_ts):
            gap.first_ts = ts
        if ts >= gap.last_ts:
            gap.last_ts = ts
            if raw_query:
                gap.query = raw_query
        if not gap.query and raw_query:
            gap.query = raw_query
    return sorted(
        gaps.values(), key=lambda g: (g.session_count, g.last_ts), reverse=True
    )


def repeat_gaps(gaps: list[Gap], min_sessions: int = DEFAULT_MIN_SESSIONS) -> list[Gap]:
    """The curation backlog: gaps hit in >= min_sessions DISTINCT sessions."""
    return [g for g in gaps if g.session_count >= min_sessions]


def render_markdown(gaps: list[Gap], total_gaps: int, min_sessions: int) -> str:
    """The /reflect:status section."""
    lines = ["## Knowledge Gaps (negative recall)", ""]
    if not gaps:
        lines.append(
            f"No repeat knowledge gaps ({total_gaps} distinct 0-result "
            f"queries on file, none in >={min_sessions} sessions)."
        )
        return "\n".join(lines)
    lines.append(
        "Knowledge gap — users keep asking about these with no learnings:"
    )
    lines.append("")
    for gap in gaps:
        last = f", last {gap.last_ts[:10]}" if gap.last_ts else ""
        lines.append(
            f"- **{gap.query or gap.normalized}** — "
            f"{gap.session_count} sessions, {gap.asks} asks{last}"
        )
    lines.append("")
    lines.append(
        "Curation backlog: capture learnings covering these topics "
        "(/reflect or /reflect:ingest) and the gaps stop recurring."
    )
    return "\n".join(lines)


def render_json(gaps: list[Gap], total_gaps: int, min_sessions: int) -> str:
    return json.dumps(
        {
            "min_sessions": min_sessions,
            "total_gaps": total_gaps,
            "repeat_gaps": [g.to_dict() for g in gaps],
        },
        indent=2,
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    ap.add_argument("--min-sessions", type=int, default=DEFAULT_MIN_SESSIONS,
                    help="distinct sessions before a gap surfaces (default 2)")
    ap.add_argument("--format", choices=["markdown", "json"], default="markdown")
    ap.add_argument("--limit", type=int, default=20,
                    help="max gaps to show (default 20)")
    args = ap.parse_args()

    entries = load_entries(gaps_path())
    all_gaps = aggregate(entries)
    surfaced = repeat_gaps(all_gaps, args.min_sessions)[: max(args.limit, 0)]

    if args.format == "json":
        print(render_json(surfaced, len(all_gaps), args.min_sessions))
    else:
        print(render_markdown(surfaced, len(all_gaps), args.min_sessions))
    return 0


if __name__ == "__main__":
    sys.exit(main())
