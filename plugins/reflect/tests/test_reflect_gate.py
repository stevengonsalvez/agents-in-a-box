"""Tests for the enqueue gate + dedup (W2).

Two layers:
  * unit  — reflect_gate.evaluate / dedup / should_enqueue verdicts
  * integ — precompact_reflect.py + stop_reflect.py actually skip what the
            gate rejects and dedup repeats (no double-enqueue)

Policy under test (locked decision #5, "middle"):
  reflect-on-reflect -> skip · no-signal/clean -> skip · ANY signal -> reflect
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
PRECOMPACT = PLUGIN_ROOT / "hooks" / "precompact_reflect.py"
STOP = PLUGIN_ROOT / "hooks" / "stop_reflect.py"

sys.path.insert(0, str(SCRIPTS))
import reflect_gate  # noqa: E402


# ── fixtures ────────────────────────────────────────────────────────────────

def _write_transcript(path: Path, turns: list[tuple[str, str]]) -> Path:
    """turns = [(role, text), ...] -> a minimal Claude-style jsonl transcript."""
    with open(path, "w") as fh:
        for role, text in turns:
            fh.write(json.dumps({"message": {"role": role, "content": text}}) + "\n")
    return path


def _reflect_on_reflect(path: Path) -> Path:
    with open(path, "w") as fh:
        fh.write(json.dumps({
            "message": {
                "role": "user",
                "content": "<command-name>reflect</command-name>\n"
                           "Process the transcript at: /some/other.jsonl",
            }
        }) + "\n")
    return path


def _clean(path: Path) -> Path:
    # No correction/approval/knowledge trigger words anywhere.
    return _write_transcript(path, [
        ("user", "Morning. Summarize the attached document for me."),
        ("assistant", "The document covers three topics: weather, travel, and cooking."),
    ])


def _has_signal(path: Path) -> Path:
    return _write_transcript(path, [
        ("user", "No, never use var here. The root cause was a missing index."),
        ("assistant", "Understood — switching to const and adding the index."),
    ])


# ── unit: evaluate ──────────────────────────────────────────────────────────

def test_evaluate_reflect_on_reflect_skips(tmp_path):
    v = reflect_gate.evaluate(_reflect_on_reflect(tmp_path / "ror.jsonl"))
    assert v.action == "skip" and v.reason == "reflect-on-reflect"


def test_evaluate_clean_session_skips(tmp_path):
    v = reflect_gate.evaluate(_clean(tmp_path / "clean.jsonl"))
    assert v.action == "skip" and v.reason == "no-signal"


def test_reflect_on_reflect_marker_past_first_records(tmp_path):
    """The bg-drainer marker can land after several preamble turns — the gate
    must still catch it (scan window widened beyond the first 4 user records)."""
    p = tmp_path / "late.jsonl"
    turns = [("user", f"warming up turn {i}") for i in range(8)]
    turns.append(("user", "Process the transcript at: /some/other.jsonl"))
    _write_transcript(p, turns)
    v = reflect_gate.evaluate(p)
    assert v.action == "skip" and v.reason == "reflect-on-reflect"


def test_human_message_mentioning_reflect_is_not_skipped(tmp_path):
    """A human session whose text merely starts with '/reflect' (no machine
    command-tag, no drainer marker) carries a real lesson and must NOT be
    dropped as reflect-on-reflect — regression guard for DE review H2."""
    p = tmp_path / "human.jsonl"
    _write_transcript(p, [
        ("user", "/reflect later, but first: never use var here, the root cause was a missing index."),
        ("assistant", "Got it — switching to const and adding the index."),
    ])
    v = reflect_gate.evaluate(p)
    assert v.action == "reflect", f"human multi-intent message over-skipped: {v}"


def test_evaluate_signal_session_reflects(tmp_path):
    v = reflect_gate.evaluate(_has_signal(tmp_path / "sig.jsonl"))
    assert v.action == "reflect" and v.signal_count > 0


def test_evaluate_missing_file_fails_open(tmp_path):
    v = reflect_gate.evaluate(tmp_path / "nope.jsonl")
    assert v.action == "reflect"  # never silently drop


def test_tool_output_noise_does_not_trip_gate(tmp_path):
    """A clean session whose TOOL output is full of error/fixed/wrong noise
    must still be a skip — the gate reads dialogue, not tool results."""
    p = tmp_path / "toolnoise.jsonl"
    with open(p, "w") as fh:
        fh.write(json.dumps({"message": {"role": "user", "content": "Morning, list the items."}}) + "\n")
        fh.write(json.dumps({"message": {"role": "assistant", "content": [
            {"type": "text", "text": "Here are the items."},
            {"type": "tool_use", "name": "Bash", "input": {"command": "echo wrong; never; error fixed"}},
        ]}}) + "\n")
        fh.write(json.dumps({"message": {"role": "user", "content": [
            {"type": "tool_result", "content": "ERROR: wrong, must fix, root cause was X"},
        ]}}) + "\n")
    v = reflect_gate.evaluate(p)
    assert v.action == "skip", f"tool noise leaked into gate: {v}"


# ── unit: dedup ─────────────────────────────────────────────────────────────

def test_already_queued(tmp_path):
    t = _has_signal(tmp_path / "t.jsonl")
    q = tmp_path / "queue.jsonl"
    q.write_text(json.dumps({"transcript_path": str(t)}) + "\n")
    assert reflect_gate.already_queued(t, q)
    assert not reflect_gate.already_queued(tmp_path / "other.jsonl", q)


def test_already_processed_terminal_outcomes(tmp_path):
    t = _has_signal(tmp_path / "t.jsonl")
    c = tmp_path / "cost.jsonl"
    c.write_text(json.dumps({"transcript": str(t), "outcome": "ok"}) + "\n")
    assert reflect_gate.already_processed(t, c)


def test_already_processed_ignores_retryable(tmp_path):
    t = _has_signal(tmp_path / "t.jsonl")
    c = tmp_path / "cost.jsonl"
    c.write_text(json.dumps({"transcript": str(t), "outcome": "fail_is_error"}) + "\n")
    assert not reflect_gate.already_processed(t, c)


def test_should_enqueue_combines_dedup_and_gate(tmp_path):
    q = tmp_path / "queue.jsonl"
    c = tmp_path / "cost.jsonl"
    q.write_text("")
    c.write_text("")
    # signal session, not seen -> enqueue
    sig = _has_signal(tmp_path / "sig.jsonl")
    ok, reason = reflect_gate.should_enqueue(sig, q, c)
    assert ok and reason == "has-signal"
    # reflect-on-reflect -> skip
    ror = _reflect_on_reflect(tmp_path / "ror.jsonl")
    ok, reason = reflect_gate.should_enqueue(ror, q, c)
    assert not ok and reason == "reflect-on-reflect"
    # already processed -> skip (dedup wins before gate)
    c.write_text(json.dumps({"transcript": str(sig), "outcome": "ok"}) + "\n")
    ok, reason = reflect_gate.should_enqueue(sig, q, c)
    assert not ok and reason == "dup-already-processed"


# ── integration: producers honour the gate ──────────────────────────────────

def _run_hook(hook: Path, payload: dict, state_dir: Path) -> None:
    env = dict(os.environ)
    env["REFLECT_STATE_DIR"] = str(state_dir)
    env["REFLECT_AUTO_REFLECT"] = "1"
    args = [sys.executable, str(hook)]
    if hook.name == "precompact_reflect.py":
        args.append("--auto")
    subprocess.run(args, input=json.dumps(payload), text=True,
                   capture_output=True, env=env, timeout=60)


def _queue_lines(state_dir: Path) -> list[str]:
    q = state_dir / "pending_reflections.jsonl"
    if not q.exists():
        return []
    return [ln for ln in q.read_text().splitlines() if ln.strip()]


@pytest.mark.parametrize("hook", [PRECOMPACT, STOP])
def test_producer_skips_reflect_on_reflect(tmp_path, hook):
    state = tmp_path / "state"
    state.mkdir()
    t = _reflect_on_reflect(tmp_path / "ror.jsonl")
    _run_hook(hook, {"session_id": "s1", "transcript_path": str(t),
                     "trigger": "auto", "cwd": "/"}, state)
    assert _queue_lines(state) == [], "reflect-on-reflect was enqueued"


@pytest.mark.parametrize("hook", [PRECOMPACT, STOP])
def test_producer_enqueues_signal_session(tmp_path, hook):
    state = tmp_path / "state"
    state.mkdir()
    t = _has_signal(tmp_path / "sig.jsonl")
    _run_hook(hook, {"session_id": "s2", "transcript_path": str(t),
                     "trigger": "auto", "cwd": "/"}, state)
    lines = _queue_lines(state)
    assert len(lines) == 1, f"expected 1 queued entry, got {lines}"


@pytest.mark.parametrize("hook", [PRECOMPACT, STOP])
def test_producer_dedups_repeat(tmp_path, hook):
    state = tmp_path / "state"
    state.mkdir()
    t = _has_signal(tmp_path / "sig.jsonl")
    payload = {"session_id": "s3", "transcript_path": str(t), "trigger": "auto", "cwd": "/"}
    _run_hook(hook, payload, state)
    _run_hook(hook, payload, state)
    assert len(_queue_lines(state)) == 1, "transcript was enqueued twice"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
