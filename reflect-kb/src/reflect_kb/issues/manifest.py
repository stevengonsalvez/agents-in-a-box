"""Source recent transcripts from the EXISTING reflect queue.

This is the ``issues`` mode's manifest builder. Rather than re-scanning
``~/.claude/projects`` (agent-deck's approach, with its hardcoded conductor
path), it reads the same ``~/.reflect/pending_reflections.jsonl`` queue that
``stop_reflect.py`` / ``precompact_reflect.py`` already append to. That queue
IS the list of recent, signal-bearing transcripts — reusing it is the whole
point of "new mode, not parallel pipeline".

Each queue line looks like::

    {"ts": "...", "session_id": "...", "transcript_path": "/abs/x.jsonl",
     "trigger": "stop", "cwd": "/abs/project"}

We de-duplicate by resolved transcript path (a long session can be enqueued by
both PreCompact and Stop), keep only paths that still exist on disk, and return
the most recent ``limit`` entries (newest first by queue order, which is append
order).
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Optional


@dataclass
class TranscriptRef:
    """A recent transcript to analyze, sourced from the reflect queue."""

    session_id: str
    transcript_path: Path
    trigger: str
    cwd: str
    ts: str


def state_dir() -> Path:
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def queue_file() -> Path:
    return state_dir() / "pending_reflections.jsonl"


def _resolve(path: str) -> Optional[Path]:
    try:
        return Path(path).expanduser().resolve()
    except (OSError, RuntimeError):
        return None


def gather_transcripts(
    *,
    queue: Optional[Path] = None,
    limit: int = 20,
    require_exists: bool = True,
) -> list[TranscriptRef]:
    """Return up to ``limit`` recent transcript refs from the reflect queue.

    De-duplicates by resolved transcript path (keeping the latest entry for a
    given path). When ``require_exists`` is True, paths missing on disk are
    skipped (a transcript can be rotated away between enqueue and run).
    """
    qf = queue or queue_file()
    if not qf.exists():
        return []

    # Keep the LAST occurrence per resolved path (most recent enqueue wins),
    # then trim to ``limit`` newest. We iterate in file (append) order.
    #
    # A plain ``dict[key] = value`` update keeps the key at its ORIGINAL
    # insertion position while only replacing the value, so a path enqueued
    # early then re-enqueued late would still sort as "old" — corrupting both
    # the de-dup result ordering and the ``limit`` tail trim. To make the order
    # reflect the most recent enqueue, we explicitly ``pop`` an existing key
    # before re-inserting it, moving it to the end of the insertion order.
    by_path: dict[str, TranscriptRef] = {}
    try:
        with open(qf, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    entry = json.loads(line)
                except json.JSONDecodeError:
                    continue
                raw = str(entry.get("transcript_path", "")).strip()
                if not raw:
                    continue
                resolved = _resolve(raw)
                if resolved is None:
                    continue
                if require_exists and not resolved.exists():
                    continue
                key = str(resolved)
                # Drop any earlier entry for this path so the re-insert below
                # lands the (newer) entry at the end of the ordered dict.
                by_path.pop(key, None)
                by_path[key] = TranscriptRef(
                    session_id=str(entry.get("session_id", "") or "unknown"),
                    transcript_path=resolved,
                    trigger=str(entry.get("trigger", "") or "unknown"),
                    cwd=str(entry.get("cwd", "") or ""),
                    ts=str(entry.get("ts", "") or ""),
                )
    except OSError:
        return []

    refs = list(by_path.values())
    # Newest last in append order → take the tail as "most recent".
    return refs[-limit:] if limit and limit > 0 else refs
