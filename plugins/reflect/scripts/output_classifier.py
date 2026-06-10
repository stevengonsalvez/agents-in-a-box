#!/usr/bin/env python3
# ABOUTME: Writer-agent output classifier + drift circuit-breaker state (port M2, pattern from claude-mem).
# ABOUTME: Classifies drain writer stdout into {valid,prose,idle,poisoned,malformed}; 3 consecutive invalids => respawn.
"""Writer-output classifier for the reflect drainer (port M2).

Pattern source: claude-mem's observer-output classifier + respawn threshold
(``src/sdk/output-classifier.ts`` / ``ResponseProcessor.ts``). Clean-room
reimplementation adapted to the drain context: the "writer" here is each
``claude -p --output-format json`` subprocess the drainer spawns per queue
entry, and "valid output" is the JSON result envelope it is supposed to print.

Why: a drifting writer model (prose instead of the envelope, schema rejection,
a wedged "prompt is too long" session) used to fail *silently* — each bad run
just bumped the generic retry counter and the queue slowly rotted. This module
makes writer liveness explicit:

* ``classify(raw)`` buckets any output into exactly one of
  ``{valid, prose, idle, poisoned, malformed}``:

  - **valid**     — a parseable JSON result envelope (the shape ``claude -p
                    --output-format json`` prints). Whether the run *succeeded*
                    is the drainer's business (is_error/retry path); this is
                    the structural gate only.
  - **idle**      — empty / whitespace-only output (timeout, killed process).
  - **poisoned**  — a known wedged-session marker ("prompt is too long",
                    "context window", …). Deterministic per transcript, so it
                    triggers an immediate respawn instead of burning retries.
  - **malformed** — JSON-intent output that is broken or the wrong shape
                    (truncated envelope, bare list/string, non-envelope dict).
  - **prose**     — any other conversational text.

* ``track(...)`` maintains a per-transcript *consecutive-invalid* streak in a
  JSONL sidecar (same most-recent-wins replay pattern as the drainer's
  retry-count.jsonl). A **valid** output resets the streak to zero; after
  ``threshold`` consecutive invalids (default 3, env
  ``REFLECT_DRAIN_INVALID_THRESHOLD``) — or a single **poisoned** output — it
  reports ``respawn=True`` with the categories of the offending outputs so the
  drainer can kill + archive the entry (the drain-flavoured "kill + respawn":
  each writer is a fresh subprocess, so respawn = stop feeding the drifting
  writer and let the next entry spawn a clean one).

CLI (consumed by reflect-drain-bg.sh):
    output_classifier.py classify                 # raw output on stdin -> category
    output_classifier.py track --state FILE --transcript T --category C
                              [--threshold N]     # -> JSON WriterHealth
"""

from __future__ import annotations

import json
import os
import sys
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path

# The closed category set. ``classify`` returns exactly one of these for any
# input — tests pin this invariant.
CATEGORIES = ("valid", "prose", "idle", "poisoned", "malformed")

DEFAULT_THRESHOLD = 3

# Markers of a wedged / exhausted writer session. These failures are
# deterministic for a given transcript (e.g. the prompt simply does not fit),
# so retrying is pure waste — one sighting is enough to respawn. Matched
# case-insensitively against the raw output. Only consulted when the output is
# NOT a healthy envelope (see classify), so a successful learning summary that
# merely *mentions* context windows is never misclassified.
POISON_MARKERS = (
    "prompt is too long",
    "context window",
    "maximum context length",
    "context length exceeded",
    "conversation is too long",
    "session exhausted",
    "session limit reached",
    "no longer able to continue",
    "credit balance is too low",
)

# Internal pseudo-category used for streak-reset records in the state file.
_RESET = "_reset"


@dataclass
class WriterHealth:
    """Streak verdict for one classified writer output."""

    transcript: str
    category: str
    consecutive: int                 # invalid streak length AFTER this output
    categories: list[str] = field(default_factory=list)  # streak's categories
    respawn: bool = False
    threshold: int = DEFAULT_THRESHOLD


def _is_result_envelope(obj: object) -> bool:
    """True if a parsed JSON value looks like the ``claude -p`` result envelope."""
    if not isinstance(obj, dict):
        return False
    return obj.get("type") == "result" or "is_error" in obj or "result" in obj


def classify(raw: object) -> str:
    """Bucket a writer's raw stdout into exactly one of CATEGORIES."""
    if not isinstance(raw, str):
        return "idle"
    text = raw.strip()
    if not text:
        return "idle"

    parsed: object = None
    parse_ok = False
    try:
        parsed = json.loads(text)
        parse_ok = True
    except (json.JSONDecodeError, ValueError):
        parse_ok = False

    lower = text.lower()
    poisoned = any(marker in lower for marker in POISON_MARKERS)

    if parse_ok and _is_result_envelope(parsed):
        # A healthy envelope is structurally valid even if it mentions a
        # marker in its summary text; only an *error* envelope carrying a
        # wedge marker (e.g. {"is_error": true, "result": "Prompt is too
        # long…"}) is a poisoned writer.
        if poisoned and bool(parsed.get("is_error", False)):  # type: ignore[union-attr]
            return "poisoned"
        return "valid"

    if poisoned:
        return "poisoned"

    if parse_ok:
        return "malformed"  # parseable JSON, wrong shape (list/string/non-envelope dict)
    if text[0] in "{[":
        return "malformed"  # JSON-intent but broken (truncated envelope etc.)

    return "prose"


def default_threshold() -> int:
    """Respawn threshold: env REFLECT_DRAIN_INVALID_THRESHOLD, default 3."""
    raw = os.environ.get("REFLECT_DRAIN_INVALID_THRESHOLD", "")
    try:
        n = int(raw)
        return n if n > 0 else DEFAULT_THRESHOLD
    except ValueError:
        return DEFAULT_THRESHOLD


def _replay_streak(state_path: Path, transcript: str) -> tuple[int, list[str]]:
    """Most-recent-wins replay of the streak for one transcript (mirrors the
    drainer's retry-count.jsonl pattern). Malformed lines are skipped."""
    consecutive = 0
    categories: list[str] = []
    try:
        with open(state_path, encoding="utf-8", errors="replace") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    e = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if e.get("transcript") != transcript:
                    continue
                try:
                    consecutive = int(e.get("consecutive", 0) or 0)
                except (TypeError, ValueError):
                    consecutive = 0
                cats = e.get("categories", [])
                categories = [str(c) for c in cats] if isinstance(cats, list) else []
    except FileNotFoundError:
        pass
    return consecutive, categories


def _append(state_path: Path, record: dict) -> None:
    state_path.parent.mkdir(parents=True, exist_ok=True)
    with open(state_path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(record) + "\n")


def track(state_path: str | Path, transcript: str, category: str,
          threshold: int | None = None) -> WriterHealth:
    """Record one classified output and return the streak verdict.

    valid    -> streak resets to 0 (acceptance: valid output resets counter).
    invalid  -> streak += 1; respawn when streak >= threshold.
    poisoned -> respawn immediately (deterministic wedge; retries are waste).

    On respawn the streak is also reset, so a re-enqueued transcript starts
    from a clean slate after the archive.
    """
    if category not in CATEGORIES:
        category = "malformed"
    if threshold is None:
        threshold = default_threshold()
    state_path = Path(state_path)

    consecutive, categories = _replay_streak(state_path, transcript)

    if category == "valid":
        consecutive, categories, respawn = 0, [], False
    else:
        consecutive += 1
        categories = categories + [category]
        respawn = (category == "poisoned") or consecutive >= threshold

    ts = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    _append(state_path, {
        "ts": ts, "transcript": transcript, "category": category,
        "consecutive": consecutive, "categories": categories, "respawn": respawn,
    })
    if respawn:
        # Reset record so the streak doesn't survive past the archive.
        _append(state_path, {
            "ts": ts, "transcript": transcript, "category": _RESET,
            "consecutive": 0, "categories": [], "respawn": False,
        })

    return WriterHealth(
        transcript=transcript, category=category, consecutive=consecutive,
        categories=categories, respawn=respawn, threshold=threshold,
    )


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Writer-output classifier (port M2)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    sub.add_parser("classify", help="classify raw writer output read from stdin")

    tp = sub.add_parser("track", help="record a classification; print streak JSON")
    tp.add_argument("--state", required=True, help="writer-health JSONL sidecar")
    tp.add_argument("--transcript", required=True)
    tp.add_argument("--category", required=True)
    tp.add_argument("--threshold", type=int, default=None,
                    help="consecutive-invalid respawn threshold "
                         "(default: $REFLECT_DRAIN_INVALID_THRESHOLD or 3)")

    args = ap.parse_args()

    if args.cmd == "classify":
        print(classify(sys.stdin.read()))
    elif args.cmd == "track":
        health = track(args.state, args.transcript, args.category, args.threshold)
        print(json.dumps(asdict(health)))


if __name__ == "__main__":
    main()
