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

Idle sweep (SG3): a launchd timer (idle_reflect.sh) calls --idle-sweep to
walk ~/.claude/projects/*/*.jsonl transcript mtimes. Sessions quiet for
longer than the idle threshold (but younger than the max age) are enqueued
with trigger='idle' — the drain tags their learnings 'speculative' since the
session may still resume. Dedup is layered: a per-(path, mtime) idle-state
file stops re-evaluation while a session stays idle, and should_enqueue()
stops re-enqueueing after a resume (already-queued / already-processed).

CLI:
    reflect_gate.py --evaluate <transcript.jsonl>     # JSON verdict
    reflect_gate.py --should-enqueue <transcript.jsonl> \
        [--queue F] [--cost F]                        # exit 0=enqueue 1=skip
    reflect_gate.py --idle-sweep [--projects-root D] \
        [--queue F] [--cost F] [--idle-state F] \
        [--threshold N] [--max-age N] [--max-per-sweep N]  # JSON summary
"""

from __future__ import annotations

import json
import os
import sys
import time
from dataclasses import asdict, dataclass
from datetime import datetime
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


# ── idle sweep (SG3) ────────────────────────────────────────────────────────
# Sessions that go quiet without a Stop/PreCompact (user stepped away,
# switched context) are "lost" reflection opportunities. The sweep watches
# transcript mtimes and enqueues quiet-but-recent sessions with
# trigger='idle'; the drain prompt tags their learnings 'speculative' so
# recall ranks them below explicit-session-end learnings.

# Idle window: quiet for at least threshold, but not older than max-age —
# the max-age stops a fresh install from backfilling months of dead sessions.
DEFAULT_IDLE_THRESHOLD_SEC = 600
DEFAULT_IDLE_MAX_AGE_SEC = 86_400
DEFAULT_IDLE_MAX_PER_SWEEP = 5


def _idle_env_int(name: str, default: int) -> int:
    """Parse a non-negative int from env; fall back on garbage/negative."""
    try:
        value = int(os.environ.get(name, default))
    except (TypeError, ValueError):
        return default
    return value if value >= 0 else default


def load_idle_state(state_file: str | Path) -> dict[str, float]:
    """{transcript_path: mtime_last_evaluated}. Missing/corrupt → empty."""
    try:
        with open(state_file, encoding="utf-8") as fh:
            data = json.load(fh)
        if isinstance(data, dict):
            return {str(k): float(v) for k, v in data.items()}
    except (OSError, ValueError, TypeError):
        pass
    return {}


def save_idle_state(state_file: str | Path, state: dict[str, float]) -> None:
    """Atomic (tmp + rename) so a crashed sweep can't corrupt the state."""
    sf = Path(state_file)
    try:
        sf.parent.mkdir(parents=True, exist_ok=True)
        tmp = sf.with_suffix(sf.suffix + ".tmp")
        tmp.write_text(json.dumps(state, indent=0), encoding="utf-8")
        tmp.replace(sf)
    except OSError:
        pass


def scan_idle_transcripts(
    projects_root: str | Path,
    *,
    threshold_sec: int = DEFAULT_IDLE_THRESHOLD_SEC,
    max_age_sec: int = DEFAULT_IDLE_MAX_AGE_SEC,
    now: float | None = None,
) -> list[tuple[Path, float]]:
    """(path, mtime) of transcripts in the idle window, newest first.

    Walks ``<projects_root>/*/*.jsonl`` (the Claude Code transcript layout:
    one munged-cwd dir per project, one ``<session-id>.jsonl`` per session).
    A transcript is idle when its mtime is at least *threshold_sec* old but
    no older than *max_age_sec*.
    """
    now = time.time() if now is None else now
    root = Path(projects_root)
    out: list[tuple[Path, float]] = []
    if not root.is_dir():
        return out
    try:
        candidates = sorted(root.glob("*/*.jsonl"))
    except OSError:
        return out
    for path in candidates:
        try:
            mtime = path.stat().st_mtime
        except OSError:
            continue
        age = now - mtime
        if threshold_sec <= age <= max_age_sec:
            out.append((path, mtime))
    out.sort(key=lambda pm: pm[1], reverse=True)
    return out


def idle_sweep(
    projects_root: str | Path,
    queue_file: str | Path,
    cost_file: str | Path,
    idle_state_file: str | Path,
    *,
    threshold_sec: int = DEFAULT_IDLE_THRESHOLD_SEC,
    max_age_sec: int = DEFAULT_IDLE_MAX_AGE_SEC,
    max_per_sweep: int = DEFAULT_IDLE_MAX_PER_SWEEP,
    now: float | None = None,
) -> dict:
    """One idle sweep: scan → dedup → gate → enqueue with trigger='idle'.

    Double-process protection is layered:

    * idle-state file — a (path, mtime) pair is evaluated at most once, so
      a still-idle session isn't re-gated every timer tick. A resume bumps
      the mtime and makes the transcript eligible again.
    * should_enqueue() — the queue/cost-log dedup then rejects anything
      already queued (e.g. by PreCompact) or already processed (the idle
      entry from BEFORE the resume reached a terminal outcome), so a
      resume-after-idle can never enqueue the same transcript twice.

    Conversely the Stop hook's session_already_queued() skips its own
    enqueue while an idle entry for the session is still pending — the two
    producers can't double-queue each other.

    Returns a JSON-able summary: ``{"scanned", "enqueued", "entries"}``.
    """
    now = time.time() if now is None else now
    state = load_idle_state(idle_state_file)
    candidates = scan_idle_transcripts(
        projects_root, threshold_sec=threshold_sec, max_age_sec=max_age_sec,
        now=now,
    )

    # Prune state entries that fell out of the idle window so the file
    # stays bounded by the live candidate set.
    live = {str(p) for p, _ in candidates}
    state = {k: v for k, v in state.items() if k in live}

    enqueued: list[dict] = []
    qf = Path(queue_file)
    for path, mtime in candidates:
        if len(enqueued) >= max_per_sweep:
            break
        key = str(path)
        if state.get(key) == mtime:
            continue  # this idle period was already evaluated
        state[key] = mtime
        ok, _reason = should_enqueue(path, queue_file, cost_file)
        if not ok:
            continue
        entry = {
            "ts": datetime.now().isoformat(),
            "session_id": path.stem,  # Claude names transcripts <session-id>.jsonl
            "transcript_path": key,
            "trigger": "idle",
            "speculative": True,
            # Neutral cwd: the munged project-dir name is not reliably
            # reversible to the original path (W5 pins the drain cwd anyway).
            "cwd": str(Path.home()),
        }
        try:
            qf.parent.mkdir(parents=True, exist_ok=True)
            with open(qf, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(entry) + "\n")
        except OSError:
            continue
        enqueued.append(entry)

    save_idle_state(idle_state_file, state)
    return {
        "scanned": len(candidates),
        "enqueued": len(enqueued),
        "entries": enqueued,
    }


# ── CLI ─────────────────────────────────────────────────────────────────────

def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Reflect enqueue gate + dedup")
    ap.add_argument("--evaluate", metavar="TRANSCRIPT")
    ap.add_argument("--should-enqueue", metavar="TRANSCRIPT")
    ap.add_argument("--queue", default="")
    ap.add_argument("--cost", default="")
    ap.add_argument("--idle-sweep", action="store_true")
    ap.add_argument("--projects-root", default="")
    ap.add_argument("--idle-state", default="")
    ap.add_argument("--threshold", type=int, default=-1)
    ap.add_argument("--max-age", type=int, default=-1)
    ap.add_argument("--max-per-sweep", type=int, default=-1)
    args = ap.parse_args()

    if args.evaluate:
        print(json.dumps(asdict(evaluate(args.evaluate)), indent=2))
        return
    if args.should_enqueue:
        ok, reason = should_enqueue(args.should_enqueue, args.queue, args.cost)
        print(json.dumps({"enqueue": ok, "reason": reason}))
        sys.exit(0 if ok else 1)
    if args.idle_sweep:
        # Flags win; env (REFLECT_IDLE_*) next; module defaults last — so
        # both the shell hook and a bare launchd invocation are configurable.
        state_dir = Path(
            os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect"))
        )
        projects_root = args.projects_root or os.environ.get(
            "REFLECT_IDLE_PROJECTS_ROOT",
            str(Path.home() / ".claude" / "projects"),
        )
        queue = args.queue or str(state_dir / "pending_reflections.jsonl")
        cost = args.cost or str(state_dir / "drain-cost.jsonl")
        idle_state = args.idle_state or str(state_dir / "idle-state.json")
        threshold = args.threshold if args.threshold >= 0 else _idle_env_int(
            "REFLECT_IDLE_THRESHOLD_SEC", DEFAULT_IDLE_THRESHOLD_SEC)
        max_age = args.max_age if args.max_age >= 0 else _idle_env_int(
            "REFLECT_IDLE_MAX_AGE_SEC", DEFAULT_IDLE_MAX_AGE_SEC)
        max_per = args.max_per_sweep if args.max_per_sweep >= 0 else _idle_env_int(
            "REFLECT_IDLE_MAX_PER_SWEEP", DEFAULT_IDLE_MAX_PER_SWEEP)
        summary = idle_sweep(
            projects_root, queue, cost, idle_state,
            threshold_sec=threshold, max_age_sec=max_age,
            max_per_sweep=max_per,
        )
        print(json.dumps(summary))
        return
    ap.error("one of --evaluate / --should-enqueue / --idle-sweep is required")


if __name__ == "__main__":
    main()
