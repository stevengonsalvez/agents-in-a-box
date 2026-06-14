# ABOUTME: Behavioral proof for SG3 — session idle trigger for natural reflection. Drives the
# ABOUTME: real reflect_gate.idle_sweep module (no recall, no torch, no LLM): a transcript quiet
# ABOUTME: PAST the idle threshold is enqueued with trigger='idle'/speculative=True, while the
# ABOUTME: SAME-content transcript quiet UNDER the threshold is never scanned and never enqueued.
# ABOUTME: The mtime age relative to the threshold is the only knob flipped between the two arms.
"""SG3: session idle trigger for natural reflection (the idle SWEEP).

Port SG3 (commit e60bf30c, "feat(reflect): idle-session sweep with speculative
down-rank") has TWO surfaces that the commit message conflates: the idle SWEEP
(a signal/scheduling trigger in ``plugins/reflect/scripts/reflect_gate.py``) and
a speculative down-rank multiplier in recall.py. The bead title (SG3 = "session
idle trigger for natural reflection") and the surface tag (signal) name the
SWEEP. RECALL_SPECULATIVE_ALPHA is the *consequence* of the sweep's tag, not the
trigger itself — so the real SG3 to prove here is ``reflect_gate.idle_sweep``,
the function the launchd timer (``hooks/idle_reflect.sh --idle-sweep``) calls.

This is a SIGNAL/SCHEDULING port: there is no retrieval, no embedding model, and
no LLM. The proof therefore drives the REAL module functions the hook calls
(``scan_idle_transcripts`` + ``idle_sweep``) against hermetic on-disk transcript,
queue and state files, with mtimes pinned via os.utime to LITERAL ages.

The TRUE invariant (read off the real diff):

    A transcript is in the idle window iff
        threshold_sec <= (now - mtime) <= max_age_sec
    Only transcripts in that window are scanned, and an in-window transcript that
    passes the (cheap, deterministic) signal gate is enqueued with
    trigger='idle' and speculative=True. A transcript whose quiet gap is BELOW
    the threshold is not even a candidate — it is never scanned and never
    enqueued. The default threshold is 600s (REFLECT_IDLE_THRESHOLD_SEC), and
    the threshold is a real parameter of idle_sweep/scan_idle_transcripts.

DECISIVE knob ON vs OFF (idle gap vs threshold; same transcript CONTENT both arms):

  ARM (knob ON  — gap PAST threshold):
      a signal-bearing transcript with mtime 700s old, threshold 600s
    => scan_idle_transcripts finds it; idle_sweep enqueues exactly ONE entry
       with trigger='idle', speculative=True, session_id = transcript stem.

  CONTROL (knob OFF — gap UNDER threshold):
      the SAME signal-bearing transcript content with mtime 30s old, threshold 600s
    => scan_idle_transcripts finds nothing; idle_sweep enqueues ZERO entries and
       writes no queue lines.

If SG3 were absent (no idle sweep wired) the ARM would enqueue nothing and the
test would FAIL. If the threshold gate were broken (level-triggered on any
transcript) the CONTROL would ALSO enqueue and the test would FAIL. The
configurability arm additionally pins that the threshold is a real knob: raising
the threshold above the gap turns the SAME transcript from enqueued to skipped.
No LLM participates — the pinned mtimes plus the threshold fully determine every
assertion (detect_signals, the only dependency, is a deterministic regex gate).

PORT: SG3
"""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

# reflect_gate lives in the reflect plugin scripts, alongside reflect-kb/.
# Resolve it the same way the SG7 capture-layer proof resolves todo_state, so
# this runs from either checkout layout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import reflect_gate  # noqa: E402
from reflect_gate import idle_sweep, scan_idle_transcripts  # noqa: E402

# A literal "now" so every age is a pinned constant, not wall-clock dependent.
NOW = 1_700_000_000.0
THRESHOLD = 600          # the default idle threshold (REFLECT_IDLE_THRESHOLD_SEC)
GAP_OVER = 700           # quiet PAST the threshold -> idle
GAP_UNDER = 30           # quiet UNDER the threshold -> still active


def _signal_transcript(path: Path) -> Path:
    """A minimal transcript carrying a correction signal so it passes the gate.

    The signal gate (detect_signals) is a deterministic regex detector; this
    content is identical in both arms so the ONLY thing that differs between
    ARM and CONTROL is the mtime gap relative to the threshold."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(json.dumps({"message": {
            "role": "user",
            "content": "No, never use var here. The root cause was a missing index.",
        }}) + "\n")
        fh.write(json.dumps({"message": {
            "role": "assistant",
            "content": "Understood — switching to const and adding the index.",
        }}) + "\n")
    return path


def _set_age(path: Path, age_sec: float) -> None:
    ts = NOW - age_sec
    os.utime(path, (ts, ts))


def _sweep(root: Path, state: Path, **kw) -> dict:
    """Drive the real idle_sweep with a hermetic queue / cost / state triple."""
    state.mkdir(parents=True, exist_ok=True)
    return idle_sweep(
        root,
        state / "pending_reflections.jsonl",
        state / "drain-cost.jsonl",
        state / "idle-state.json",
        now=NOW,
        **kw,
    )


def _queue_entries(state: Path) -> list[dict]:
    q = state / "pending_reflections.jsonl"
    if not q.exists():
        return []
    return [json.loads(ln) for ln in q.read_text(encoding="utf-8").splitlines() if ln.strip()]


def test_SG3_idle_gap_past_threshold_enqueues_speculative_reflection(tmp_path):
    """ARM (knob ON): a transcript quiet PAST the threshold is enqueued exactly
    once with trigger='idle' and speculative=True — the natural-reflection
    trigger fired without any explicit Stop/PreCompact."""
    root = tmp_path / "projects"
    proj = root / "-Users-x-dev-proj"
    t = _signal_transcript(proj / "idle-session.jsonl")
    _set_age(t, GAP_OVER)  # 700s quiet, past the 600s threshold

    # The scan-level invariant: only the in-window transcript is a candidate.
    found = [p for p, _ in scan_idle_transcripts(root, threshold_sec=THRESHOLD, now=NOW)]
    assert found == [t], (
        f"a {GAP_OVER}s-quiet transcript must be in the idle window "
        f"(got {[p.name for p in found]})"
    )

    state = tmp_path / "state"
    summary = _sweep(root, state, threshold_sec=THRESHOLD)

    # The sweep enqueued exactly one reflection.
    assert summary["scanned"] == 1
    assert summary["enqueued"] == 1, f"idle sweep must enqueue the quiet session: {summary}"

    entries = _queue_entries(state)
    assert len(entries) == 1, f"exactly one queue line expected, got {entries}"
    entry = entries[0]
    # The trigger is the idle trigger, not an explicit session-end trigger.
    assert entry["trigger"] == "idle", "idle-sweep enqueue must carry trigger='idle'"
    # It is tagged speculative (the session may resume) — the SG3 down-rank hook.
    assert entry["speculative"] is True, "idle reflection must be tagged speculative"
    # The enqueued unit IS the quiet transcript.
    assert entry["session_id"] == "idle-session"
    assert entry["transcript_path"] == str(t)


def test_SG3_idle_gap_under_threshold_enqueues_nothing(tmp_path):
    """CONTROL (knob OFF): the SAME transcript content quiet UNDER the threshold
    is never scanned and never enqueued. This is the falsifiable half — it rules
    out a level-triggered 'reflect on any transcript' failure mode and proves the
    threshold gap is what causes the trigger."""
    root = tmp_path / "projects"
    proj = root / "-Users-x-dev-proj"
    t = _signal_transcript(proj / "active-session.jsonl")
    _set_age(t, GAP_UNDER)  # 30s quiet — still being worked on

    # Scan-level: a sub-threshold transcript is not even a candidate.
    found = scan_idle_transcripts(root, threshold_sec=THRESHOLD, now=NOW)
    assert found == [], (
        f"a {GAP_UNDER}s-quiet transcript must NOT be in the idle window "
        f"(got {[p.name for p, _ in found]})"
    )

    state = tmp_path / "state"
    summary = _sweep(root, state, threshold_sec=THRESHOLD)

    assert summary["scanned"] == 0
    assert summary["enqueued"] == 0, f"a sub-threshold session must not be enqueued: {summary}"
    assert _queue_entries(state) == [], "no queue line may be written for an active session"


def test_SG3_threshold_is_a_real_knob(tmp_path):
    """The threshold is a genuine parameter, not a hardcoded constant: with the
    SAME transcript at the SAME mtime, a threshold BELOW the gap enqueues it and a
    threshold ABOVE the gap skips it. Flipping only the threshold flips the
    outcome — the documented REFLECT_IDLE_THRESHOLD_SEC knob is load-bearing."""
    gap = 400  # quiet for 400s

    # Knob below the gap (300 < 400): enqueued.
    root_on = tmp_path / "on" / "projects"
    t_on = _signal_transcript(root_on / "-Users-x-dev-proj" / "s.jsonl")
    _set_age(t_on, gap)
    state_on = tmp_path / "on" / "state"
    summary_on = _sweep(root_on, state_on, threshold_sec=300)
    assert summary_on["enqueued"] == 1, (
        f"threshold below the {gap}s gap must enqueue: {summary_on}"
    )

    # Knob above the gap (600 > 400): the SAME transcript is skipped.
    root_off = tmp_path / "off" / "projects"
    t_off = _signal_transcript(root_off / "-Users-x-dev-proj" / "s.jsonl")
    _set_age(t_off, gap)
    state_off = tmp_path / "off" / "state"
    summary_off = _sweep(root_off, state_off, threshold_sec=600)
    assert summary_off["enqueued"] == 0, (
        f"threshold above the {gap}s gap must skip the same transcript: {summary_off}"
    )


def test_SG3_idle_period_evaluated_once_until_resume(tmp_path):
    """Dedup is part of the trigger contract: a session that STAYS idle is gated
    at most once per idle period (the per-(path, mtime) idle-state file), so the
    launchd timer firing every minute does not re-enqueue the same quiet session.
    A second sweep at the same mtime enqueues nothing more."""
    root = tmp_path / "projects"
    t = _signal_transcript(root / "-Users-x-dev-proj" / "idle.jsonl")
    _set_age(t, GAP_OVER)
    state = tmp_path / "state"

    first = _sweep(root, state, threshold_sec=THRESHOLD)
    assert first["enqueued"] == 1, f"first sweep must enqueue: {first}"

    # Second sweep, same mtime, same state file — the idle period is unchanged.
    second = _sweep(root, state, threshold_sec=THRESHOLD)
    assert second["enqueued"] == 0, (
        f"a still-idle session must not be re-enqueued on the next tick: {second}"
    )
    # Still exactly one queue line on disk.
    assert len(_queue_entries(state)) == 1
