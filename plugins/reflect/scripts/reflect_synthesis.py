#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
reflect_synthesis.py — weekly Opus synthesis pass (W5 / decision: cascade + weekly Opus).

Per-transcript reflection runs cheap (cascade + Sonnet). Opus earns its keep in
a PERIODIC batch instead: scan recent learnings, find near-duplicate / mergeable
clusters and cross-cutting meta-patterns, and propose merges. This is the one
place a big model is worth it — bounded, weekly, high-leverage — not per
transcript.

Two layers:
  * cluster()   — deterministic near-dupe grouping by title/tag token overlap
                  (no LLM, fully testable). This is the candidate finder.
  * synthesize  — hand the clusters to one bounded Opus call to propose merges.
                  Optional; --dry-run reports clusters without calling a model.

C2 auto-trigger (Hindsight ``enable_auto_consolidation`` shape): a periodic
``--check-auto`` tick counts learnings created since the last consolidation
pass (mirrored into the ``learnings_since_last_consolidation`` metric) and
runs the synthesis EARLY when the threshold (default 30) is crossed. Quiet
projects keep the weekly cadence via the --max-age fallback; active projects
get a fresher KB without waiting a week.

CLI:
    reflect_synthesis.py [--docs-dir DIR] [--since 7d] [--threshold 0.5]
                         [--dry-run] [--model opus]
                         [--check-auto] [--auto-threshold N] [--max-age 7d]
"""

from __future__ import annotations

import os
import re
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

_DOC_GLOBS = [
    "~/.learnings/documents",
    "~/.claude/global-learnings/documents",
]
_STOP = {"the", "a", "an", "to", "of", "in", "on", "for", "and", "or", "is",
         "via", "with", "when", "not", "use", "vs", "but"}


@dataclass
class Doc:
    path: Path
    title: str
    tags: list[str] = field(default_factory=list)
    mtime: float = 0.0


def _parse_frontmatter(text: str) -> dict:
    if not text.startswith("---"):
        return {}
    end = text.find("\n---", 3)
    if end == -1:
        return {}
    fm: dict = {}
    for line in text[3:end].splitlines():
        if ":" in line:
            k, _, v = line.partition(":")
            fm[k.strip()] = v.strip().strip('"')
    return fm


def load_docs(docs_dir: Optional[str], since_seconds: float) -> list[Doc]:
    roots = [Path(docs_dir).expanduser()] if docs_dir else \
        [Path(p).expanduser() for p in _DOC_GLOBS]
    cutoff = _now() - since_seconds if since_seconds else 0
    docs: list[Doc] = []
    for root in roots:
        if not root.is_dir():
            continue
        for md in root.glob("**/*.md"):
            try:
                mtime = md.stat().st_mtime
            except OSError:
                continue
            if cutoff and mtime < cutoff:
                continue
            try:
                text = md.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            fm = _parse_frontmatter(text)
            title = fm.get("title") or md.stem
            tags = []
            raw_tags = fm.get("tags", "")
            if raw_tags:
                tags = [t.strip().strip("[]") for t in raw_tags.split(",") if t.strip()]
            docs.append(Doc(md, title, tags, mtime))
    return docs


def _tokens(s: str) -> set[str]:
    return {w for w in re.findall(r"[a-z0-9]+", s.lower()) if w not in _STOP and len(w) > 2}


def _similarity(a: Doc, b: Doc) -> float:
    ta, tb = _tokens(a.title) | set(a.tags), _tokens(b.title) | set(b.tags)
    if not ta or not tb:
        return 0.0
    return len(ta & tb) / len(ta | tb)  # Jaccard


def cluster(docs: list[Doc], threshold: float = 0.5) -> list[list[Doc]]:
    """Group docs whose title/tag token sets overlap >= threshold (Jaccard).
    Simple union-find style single-linkage clustering."""
    n = len(docs)
    parent = list(range(n))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(x, y):
        parent[find(x)] = find(y)

    for i in range(n):
        for j in range(i + 1, n):
            if _similarity(docs[i], docs[j]) >= threshold:
                union(i, j)

    groups: dict[int, list[Doc]] = {}
    for i in range(n):
        groups.setdefault(find(i), []).append(docs[i])
    # Only clusters with >1 member are interesting (merge candidates).
    return [g for g in groups.values() if len(g) > 1]


def _now() -> float:
    return time.time()


def _since_seconds(s: str) -> float:
    s = (s or "7d").strip().lower()
    try:
        if s.endswith("d"):
            return int(s[:-1]) * 86400
        if s.endswith("w"):
            return int(s[:-1]) * 604800
        if s.endswith("h"):
            return int(s[:-1]) * 3600
        return int(s) * 86400
    except ValueError:
        return 7 * 86400


# ---------------------------------------------------------------------------
# C2: auto-trigger consolidation on N learnings.
#
# Hindsight gates consolidation on a count of unconsolidated memory units
# (``enable_auto_consolidation`` + ``submit_async_consolidation``); the port
# counts learnings rows created since the last synthesis pass. The counter is
# computed from the learnings table (not writer-side increments) so it can
# never drift, and is mirrored into the ``learnings_since_last_consolidation``
# metric — the acceptance-pinned observable surfaced by metrics_updater.
# ---------------------------------------------------------------------------

AUTO_TRIGGER_THRESHOLD_DEFAULT = 30
LAST_CONSOLIDATION_KEY = "last_consolidation_at"
PENDING_LEARNINGS_KEY = "learnings_since_last_consolidation"


def _load_reflect_db():
    """Lazy sibling import so the pure clustering path stays import-free."""
    scripts_dir = str(Path(__file__).resolve().parent)
    if scripts_dir not in sys.path:
        sys.path.insert(0, scripts_dir)
    import reflect_db

    return reflect_db


def auto_trigger_threshold() -> int:
    """Trigger count: $REFLECT_SYNTHESIS_AUTO_THRESHOLD, else 30."""
    raw = os.environ.get("REFLECT_SYNTHESIS_AUTO_THRESHOLD", "")
    try:
        value = int(raw)
        if value > 0:
            return value
    except ValueError:
        pass
    return AUTO_TRIGGER_THRESHOLD_DEFAULT


def _iso_to_epoch(s: str) -> Optional[float]:
    """ISO-8601 timestamp → epoch seconds; None when unparseable."""
    try:
        from datetime import datetime

        return datetime.fromisoformat(s).timestamp()
    except (ValueError, TypeError):
        return None


def learnings_since_last_consolidation(*, conn=None, db=None) -> int:
    """Count learnings created since the last consolidation pass.

    No baseline yet (fresh install) counts everything — those rows are
    genuinely unconsolidated, the Hindsight ``consolidated_at IS NULL``
    shape. The value is mirrored into the metrics table on every read so
    ``learnings_since_last_consolidation`` is always inspectable.
    """
    db = db or _load_reflect_db()
    conn = conn or db.get_conn()
    last = db.get_metric(LAST_CONSOLIDATION_KEY, "", conn=conn) or ""
    if last:
        row = conn.execute(
            "SELECT COUNT(*) FROM learnings WHERE created_at > ?", (last,)
        ).fetchone()
    else:
        row = conn.execute("SELECT COUNT(*) FROM learnings").fetchone()
    count = int(row[0])
    db.set_metric(PENDING_LEARNINGS_KEY, count, conn=conn)
    return count


def record_consolidation_run(*, when: Optional[str] = None, conn=None, db=None) -> None:
    """Mark a completed synthesis pass: stamp the baseline, zero the counter."""
    db = db or _load_reflect_db()
    conn = conn or db.get_conn()
    if when is None:
        from datetime import datetime, timezone

        when = datetime.now(timezone.utc).isoformat()
    db.set_metric(LAST_CONSOLIDATION_KEY, when, conn=conn)
    db.set_metric(PENDING_LEARNINGS_KEY, 0, conn=conn)


def should_auto_trigger(
    threshold: Optional[int] = None,
    max_age_seconds: float = 7 * 86400,
    *,
    conn=None,
    db=None,
) -> tuple[bool, str, int]:
    """Decide whether the periodic tick should run synthesis NOW.

    Returns ``(triggered, reason, pending_count)``. Fires when:
      * pending learnings >= threshold — the early path (active projects), or
      * the last pass is older than *max_age_seconds* — the weekly fallback
        (quiet projects keep the periodic cadence). With no baseline the age
        anchor is the oldest learning, so a fresh-but-stale KB still gets a
        first pass within the window.
    """
    db = db or _load_reflect_db()
    conn = conn or db.get_conn()
    threshold = int(threshold or auto_trigger_threshold())
    count = learnings_since_last_consolidation(conn=conn, db=db)
    if count >= threshold:
        return True, f"threshold crossed ({count} >= {threshold})", count

    anchor = db.get_metric(LAST_CONSOLIDATION_KEY, "", conn=conn) or ""
    if not anchor:
        row = conn.execute("SELECT MIN(created_at) FROM learnings").fetchone()
        anchor = row[0] or ""
    if anchor:
        epoch = _iso_to_epoch(anchor)
        if epoch is not None and _now() - epoch >= max_age_seconds:
            age_days = (_now() - epoch) / 86400
            return True, f"age fallback ({age_days:.1f}d since last run)", count
    return False, f"below threshold ({count} < {threshold})", count


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Weekly Opus synthesis of learnings")
    ap.add_argument("--docs-dir", default=None)
    ap.add_argument("--since", default="7d")
    ap.add_argument("--threshold", type=float, default=0.5)
    ap.add_argument("--model", default="opus")
    ap.add_argument("--dry-run", action="store_true",
                    help="report merge-candidate clusters; do not call a model")
    ap.add_argument("--check-auto", action="store_true",
                    help="C2: run only when the pending-learnings threshold is "
                         "crossed or the last run is older than --max-age")
    ap.add_argument("--auto-threshold", type=int, default=0,
                    help="pending-learnings trigger count (default: "
                         "$REFLECT_SYNTHESIS_AUTO_THRESHOLD or 30)")
    ap.add_argument("--max-age", default="7d",
                    help="age fallback for --check-auto (weekly cadence floor)")
    args = ap.parse_args()

    since_seconds = _since_seconds(args.since)
    db = None
    if args.check_auto:
        try:
            db = _load_reflect_db()
            triggered, reason, count = should_auto_trigger(
                args.auto_threshold or None, _since_seconds(args.max_age), db=db,
            )
        except Exception as exc:  # noqa: BLE001 — silent-fail shaped: a broken
            # DB must never crash the launchd tick into a respawn loop.
            print(f"reflect synthesis: auto-check skipped ({exc})", file=sys.stderr)
            return
        print(f"reflect synthesis auto-check: {count} learning(s) pending — {reason}")
        if not triggered:
            return
        # Widen the docs window to cover everything since the last pass
        # (--since stays the floor) so an early run can't miss fresh notes.
        last = db.get_metric(LAST_CONSOLIDATION_KEY, "") or ""
        epoch = _iso_to_epoch(last) if last else None
        if epoch is not None:
            since_seconds = max(since_seconds, _now() - epoch + 3600)

    docs = load_docs(args.docs_dir, since_seconds)
    clusters = cluster(docs, args.threshold)
    print(f"reflect synthesis: {len(docs)} learnings in window, "
          f"{len(clusters)} merge-candidate cluster(s)")
    for i, group in enumerate(clusters, 1):
        print(f"  cluster {i} ({len(group)}):")
        for d in group:
            print(f"    - {d.title}  [{d.path.name}]")

    if not args.dry_run:
        # C2: a completed (non-dry-run) pass IS a consolidation — stamp the
        # baseline and zero the counter so the trigger re-arms. Recorded
        # before the model phase so a hung Opus call can't double-trigger
        # the next tick. Best-effort: metrics being unavailable must never
        # fail the synthesis itself.
        try:
            record_consolidation_run(db=db)
        except Exception:  # noqa: BLE001
            pass

    if args.dry_run or not clusters:
        return

    # Live synthesis: one bounded Opus call per cluster to propose a merge.
    # Kept intentionally small — this is where the big model is justified.
    try:
        import subprocess
        for group in clusters:
            titles = "\n".join(f"- {d.title}" for d in group)
            prompt = (
                "These learning notes look like near-duplicates. Propose ONE "
                "merged note (or say 'keep separate' with a one-line reason):\n"
                f"{titles}"
            )
            subprocess.run(
                ["claude", "-p", prompt, "--model", args.model,
                 "--output-format", "json", "--max-turns", "3"],
                capture_output=True, text=True, timeout=180,
            )
    except Exception as exc:  # noqa: BLE001
        print(f"  (synthesis model call skipped: {exc})", file=sys.stderr)


if __name__ == "__main__":
    main()
