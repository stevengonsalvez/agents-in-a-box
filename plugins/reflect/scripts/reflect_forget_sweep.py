#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# ABOUTME: Hourly forget sweep (port A3, agentmemory auto-forget shape) —
# ABOUTME: archives learnings whose per-row `forget_after` ISO TTL has passed.
"""
Forget sweep for per-row TTL learnings (A3).

Each learning may carry an optional ``forget_after`` ISO-8601 timestamp
('this is only valid for the current migration / sprint / quarter').
This script — run hourly by launchd (``com.reflect.forget.plist``) —
archives every learning past its TTL:

  - DB side (``reflect_db.sweep_expired_learnings``): S6 history snapshot,
    ``status -> 'archived'`` + ``is_latest = 0``, ``learning_forgotten``
    audit event. Non-destructive — nothing is deleted.
  - File side (here): the learning's knowledge-note artifact and entity
    sidecar are moved into a ``.forgotten/`` sibling directory so the next
    reindex drops them from recall. Best-effort and silent-fail shaped —
    a missing or unmovable file never blocks the DB archive.

Absent ``forget_after`` = permanent (agentmemory ``Memory.forgetAfter``
semantics). Unparseable TTLs are treated as permanent: never archive on
bad data.

Usage:
    python3 reflect_forget_sweep.py            # archive expired learnings
    python3 reflect_forget_sweep.py --dry-run  # report only, mutate nothing
    python3 reflect_forget_sweep.py --now 2026-06-10T00:00:00+00:00  # testing
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

# Ensure sibling imports work when run standalone (launchd / cron).
_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

import reflect_db  # noqa: E402

FORGOTTEN_DIR_NAME = ".forgotten"


def archive_artifact_file(path_str: str) -> str:
    """Move an artifact file into a ``.forgotten/`` sibling directory.

    Returns the new path on success, '' when there was nothing to move or
    the move failed (silent-fail shaped: the DB archive already happened
    and is the source of truth; the file move only keeps the next reindex
    from resurfacing the note).
    """
    try:
        if not path_str:
            return ""
        src = Path(path_str).expanduser()
        if not src.is_file():
            return ""
        dest_dir = src.parent / FORGOTTEN_DIR_NAME
        dest_dir.mkdir(parents=True, exist_ok=True)
        dest = dest_dir / src.name
        if dest.exists():  # name collision: suffix with a counter
            for i in range(1, 1000):
                candidate = dest_dir / f"{src.stem}.{i}{src.suffix}"
                if not candidate.exists():
                    dest = candidate
                    break
        src.rename(dest)
        return str(dest)
    except Exception:
        return ""


def run_sweep(*, now: Any = None, dry_run: bool = False) -> dict[str, Any]:
    """Execute one sweep pass. Returns a JSON-serializable summary."""
    expired = reflect_db.sweep_expired_learnings(now=now, dry_run=dry_run)
    summary: dict[str, Any] = {
        "dry_run": dry_run,
        "archived": len(expired) if not dry_run else 0,
        "expired": len(expired),
        "learnings": [],
    }
    for row in expired:
        entry: dict[str, Any] = {
            "id": row["id"],
            "title": row["title"],
            "forget_after": row["forget_after"],
        }
        if not dry_run:
            moved = []
            for key in ("artifact_path", "sidecar_path"):
                new_path = archive_artifact_file(row.get(key) or "")
                if new_path:
                    moved.append(new_path)
            entry["files_archived"] = moved
        summary["learnings"].append(entry)
    return summary


def main() -> int:
    ap = argparse.ArgumentParser(description="Archive learnings past their forget_after TTL")
    ap.add_argument("--dry-run", action="store_true",
                    help="report expired learnings; do not mutate anything")
    ap.add_argument("--now", default=None,
                    help="override the sweep clock (ISO timestamp, for testing)")
    args = ap.parse_args()

    try:
        summary = run_sweep(now=args.now, dry_run=args.dry_run)
    except Exception as exc:  # noqa: BLE001 — cron context: report, exit clean
        print(json.dumps({"error": str(exc)}))
        return 1
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
