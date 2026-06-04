#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Reflect enqueue gate + dedup (W2).

The producer hooks (precompact_reflect.py, stop_reflect.py) can't run an LLM,
but they CAN run cheap regex over a transcript to decide whether reflecting on
it is worth any model spend at all. This module is that $0 decision.

Policy (locked decision #5 — "middle" aggressiveness):
    * reflect-on-reflect transcript        -> SKIP   (its own /reflect run)
    * no correction/approval/knowledge      -> SKIP   (clean / no-signal session)
      signal anywhere
    * ANY signal (incl LOW)                 -> REFLECT

Plus dedup so a transcript is never queued or processed twice (the 16x
reprocessing that drove the 2026-05-31 overspend):
    * already_queued()    — present in pending_reflections.jsonl
    * already_processed() — has a terminal outcome in drain-cost.jsonl

Fail-open: any error reading/parsing returns a REFLECT verdict (better to
spend than to silently drop a real lesson). The drainer's own caps bound the
cost of a false "reflect".

CLI:
    reflect_gate.py --evaluate <transcript.jsonl>     # JSON verdict
    reflect_gate.py --should-enqueue <transcript.jsonl> \
        [--queue F] [--cost F]                        # exit 0=enqueue 1=skip
"""

from __future__ import annotations

import json
import sys
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional

_SCRIPTS_DIR = Path(__file__).resolve().parent
if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

try:
    from signal_detector import detect_signals  # noqa: E402
except Exception:  # pragma: no cover - import guard
    detect_signals = None  # type: ignore[assignment]


# Cap how much dialogue we scan — regex over a few hundred KB is instant, and
# correction signals live in the dialogue, not in a 5MB tool-output tail.
_MAX_SCAN_CHARS = 600_000

# Outcomes in drain-cost.jsonl that mean "don't process again".
_TERMINAL_OUTCOMES_PREFIX = ("ok", "stale", "poison", "dry_run")


@dataclass
class GateVerdict:
    action: str            # "reflect" | "skip"
    reason: str            # machine-readable reason code
    signal_count: int      # signals found by signal_detector
    reflect_on_reflect: bool


# ── transcript parsing ──────────────────────────────────────────────────────

def _iter_records(path: Path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def _text_from_content(content) -> list[str]:
    """Pull human/assistant *dialogue* text out of one message's content.

    Deliberately skips tool_use, tool_result, thinking and image blocks — a
    correction lives in what the user/assistant SAID, not in tool output (which
    is full of "error"/"fixed"/"wrong" noise that would trip the detector).
    """
    out: list[str] = []
    if isinstance(content, str):
        out.append(content)
    elif isinstance(content, list):
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                txt = block.get("text", "")
                if txt:
                    out.append(txt)
    return out


def extract_dialogue(path: Path, max_chars: int = _MAX_SCAN_CHARS) -> str:
    """Concatenate user + assistant text turns, capped at *max_chars*."""
    parts: list[str] = []
    total = 0
    for rec in _iter_records(path):
        msg = rec.get("message")
        if not isinstance(msg, dict):
            continue
        if msg.get("role") not in ("user", "assistant"):
            continue
        for txt in _text_from_content(msg.get("content")):
            parts.append(txt)
            total += len(txt)
            if total >= max_chars:
                return "\n".join(parts)
    return "\n".join(parts)


# Machine-generated markers that unambiguously identify a /reflect run.
# Both are emitted by tooling — the bg-drainer prompt and Claude Code's
# slash-command expansion — never typed free-hand by a human. We deliberately
# do NOT match a bare "/reflect" text prefix: a human message such as
# "/reflect later, but first never use var again" carries a real lesson, and a
# prefix match would skip the whole transcript and silently drop that signal.
_REFLECT_RUN_MARKERS = (
    "<command-name>reflect</command-name>",
    "process the transcript at:",
)

# The markers live in the opening exchange, but interleaved system /
# queue-operation records can push the first user turns back. Scan a bounded
# prefix wide enough to clear them rather than only the first 4 user records.
_REFLECT_SCAN_MAX_RECORDS = 60
_REFLECT_SCAN_MAX_CHARS = 50_000


def is_reflect_on_reflect(path: Path) -> bool:
    """True when the transcript is itself a /reflect run.

    Detected from machine-generated markers in the opening records: the
    bg-drainer prompt ("Process the transcript at:") or a Claude Code
    `/reflect` slash-command expansion. These produce zero net-new learnings
    (the lesson was already harvested) — the exact case that burned 41.5M
    tokens. A human message that merely mentions /reflect is NOT matched.
    """
    records = 0
    scanned = 0
    for rec in _iter_records(path):
        records += 1
        msg = rec.get("message")
        content = None
        if isinstance(msg, dict) and msg.get("role") == "user":
            content = msg.get("content")
        elif rec.get("type") == "queue-operation":
            # bg-drainer prompt lands as a queue-operation record before the
            # first user message is materialised.
            content = rec.get("content")
        if content is not None:
            blob = content if isinstance(content, str) else json.dumps(content)
            low = blob.lower()
            scanned += len(low)
            if any(marker in low for marker in _REFLECT_RUN_MARKERS):
                return True
        if records >= _REFLECT_SCAN_MAX_RECORDS or scanned >= _REFLECT_SCAN_MAX_CHARS:
            break
    return False


# ── verdict ───────────────────────────────────────────────────────────────

def evaluate(path: str | Path) -> GateVerdict:
    p = Path(path)
    if not p.exists():
        # Stale path — let the drainer's stale handling drop it; don't skip here.
        return GateVerdict("reflect", "missing-on-disk", 0, False)
    try:
        if is_reflect_on_reflect(p):
            return GateVerdict("skip", "reflect-on-reflect", 0, True)
        if detect_signals is None:
            return GateVerdict("reflect", "detector-unavailable", 0, False)
        text = extract_dialogue(p)
        signals = detect_signals(text)
        n = len(signals)
        if n == 0:
            return GateVerdict("skip", "no-signal", 0, False)
        return GateVerdict("reflect", "has-signal", n, False)
    except Exception as exc:  # noqa: BLE001 - fail open
        return GateVerdict("reflect", f"gate-error:{type(exc).__name__}", 0, False)


# ── dedup ───────────────────────────────────────────────────────────────────

def _resolved(path: str | Path) -> str:
    try:
        return str(Path(path).resolve())
    except Exception:
        return str(path)


def already_queued(path: str | Path, queue_file: str | Path) -> bool:
    """True if this transcript path already has an entry in the queue."""
    qf = Path(queue_file)
    if not qf.exists():
        return False
    target = _resolved(path)
    try:
        for rec in _iter_records(qf):
            if _resolved(rec.get("transcript_path", "")) == target:
                return True
    except OSError:
        return False
    return False


def already_processed(path: str | Path, cost_file: str | Path) -> bool:
    """True if this transcript already reached a terminal outcome in the cost log."""
    cf = Path(cost_file)
    if not cf.exists():
        return False
    target = _resolved(path)
    try:
        for rec in _iter_records(cf):
            if _resolved(rec.get("transcript", "")) != target:
                continue
            outcome = str(rec.get("outcome", ""))
            if outcome.startswith(_TERMINAL_OUTCOMES_PREFIX):
                return True
    except OSError:
        return False
    return False


def should_enqueue(
    path: str | Path,
    queue_file: str | Path,
    cost_file: str | Path,
) -> tuple[bool, str]:
    """Combined enqueue decision: dedup first (cheap), then the signal gate.

    Returns (enqueue?, reason).
    """
    if already_queued(path, queue_file):
        return False, "dup-already-queued"
    if already_processed(path, cost_file):
        return False, "dup-already-processed"
    verdict = evaluate(path)
    return (verdict.action == "reflect"), verdict.reason


# ── CLI ─────────────────────────────────────────────────────────────────────

def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Reflect enqueue gate + dedup")
    ap.add_argument("--evaluate", metavar="TRANSCRIPT")
    ap.add_argument("--should-enqueue", metavar="TRANSCRIPT")
    ap.add_argument("--queue", default="")
    ap.add_argument("--cost", default="")
    args = ap.parse_args()

    if args.evaluate:
        print(json.dumps(asdict(evaluate(args.evaluate)), indent=2))
        return
    if args.should_enqueue:
        ok, reason = should_enqueue(args.should_enqueue, args.queue, args.cost)
        print(json.dumps({"enqueue": ok, "reason": reason}))
        sys.exit(0 if ok else 1)
    ap.error("one of --evaluate / --should-enqueue is required")


if __name__ == "__main__":
    main()
