"""Tests for the cascade pre-processing (W4): reflect_cascade.prepare + slicing.

The cascade's value is making /reflect's INPUT tiny and skipping worthless
runs for $0. prepare() is deterministic (no LLM) so it's fully testable here;
the actual Sonnet /reflect call is the drainer's job (integration-tested via
the drain script's dry-run cascade path).
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"
sys.path.insert(0, str(SCRIPTS))

import reflect_cascade  # noqa: E402


def _write(path: Path, turns: list[tuple[str, str]]) -> Path:
    with open(path, "w") as fh:
        for role, text in turns:
            fh.write(json.dumps({"message": {"role": role, "content": text}}) + "\n")
    return path


def _reflect_on_reflect(path: Path) -> Path:
    path.write_text(json.dumps({"message": {
        "role": "user",
        "content": "<command-name>reflect</command-name>\nProcess the transcript at: /x.jsonl",
    }}) + "\n")
    return path


# ── prepare: gate verdicts ───────────────────────────────────────────────────

def test_prepare_skips_reflect_on_reflect(tmp_path):
    prep = reflect_cascade.prepare(_reflect_on_reflect(tmp_path / "ror.jsonl"))
    assert prep.action == "skip" and prep.reason == "reflect-on-reflect"
    assert prep.slice_path is None


def test_prepare_skips_clean_session(tmp_path):
    t = _write(tmp_path / "clean.jsonl", [
        ("user", "Morning. Summarize the attached document for me."),
        ("assistant", "It covers three topics: weather, travel, cooking."),
    ])
    prep = reflect_cascade.prepare(t)
    assert prep.action == "skip" and prep.reason == "no-signal"


def test_prepare_reflects_and_slices(tmp_path):
    # Many filler lines + one real correction far down: the slice must be much
    # smaller than the original dialogue.
    filler = [("assistant", f"Step {i}: routine progress note number {i}.") for i in range(400)]
    turns = filler + [("user", "No, never use var here. The root cause was a missing index.")] + filler
    t = _write(tmp_path / "big.jsonl", turns)
    prep = reflect_cascade.prepare(t, out_path=str(tmp_path / "slice.txt"))
    assert prep.action == "reflect"
    assert prep.signal_count > 0
    assert prep.slice_path and Path(prep.slice_path).exists()
    # The whole point: the slice is a fraction of the original.
    assert prep.slice_tokens < prep.orig_tokens
    body = Path(prep.slice_path).read_text()
    assert "never use var" in body  # the signal survived into the slice


def test_prepare_dedups_by_signal_hash(tmp_path, monkeypatch):
    t = _write(tmp_path / "sig.jsonl", [
        ("user", "No, never use var. The root cause was a missing index."),
    ])
    monkeypatch.setattr(reflect_cascade, "_signal_hash_seen", lambda h: True)
    prep = reflect_cascade.prepare(t)
    assert prep.action == "skip" and prep.reason == "dup-signal-hash"


# ── slice_dialogue ───────────────────────────────────────────────────────────

class _Sig:
    def __init__(self, line_number, signal="x"):
        self.line_number = line_number
        self.signal = signal


def test_slice_keeps_windows_and_drops_filler():
    lines = [f"line{i}" for i in range(100)]
    text = "\n".join(lines)
    # line_number is 1-based (signal_detector); two windows with filler between.
    sliced = reflect_cascade.slice_dialogue(text, [_Sig(21), _Sig(81)], context_lines=2)
    assert "line20" in sliced and "line80" in sliced  # window centres kept
    assert "line50" not in sliced                      # filler between dropped
    assert "…" in sliced                               # gap marker between windows


def test_slice_respects_max_chars():
    lines = [f"line{i}" for i in range(1000)]
    text = "\n".join(lines)
    sigs = [_Sig(i) for i in range(0, 1000, 2)]  # signals everywhere
    sliced = reflect_cascade.slice_dialogue(text, sigs, context_lines=1, max_chars=200)
    assert len(sliced) <= 260  # cap + truncation marker slack


# ── drainer integration: cascade skip is $0 ─────────────────────────────────

def test_drainer_cascade_skip_is_free(tmp_path):
    state = tmp_path / "state"
    state.mkdir()
    ror = state / "ror.jsonl"
    _reflect_on_reflect(ror)
    (state / "pending_reflections.jsonl").write_text(
        json.dumps({"ts": "t", "session_id": "s", "transcript_path": str(ror),
                    "trigger": "stop", "cwd": "/"}) + "\n"
    )
    import os
    env = dict(os.environ)
    env.update({
        "REFLECT_STATE_DIR": str(state),
        "REFLECT_DRAIN_DRY_RUN": "1",
        "REFLECT_DRAIN_SKIP_REINDEX": "1",
        "REFLECT_DRAIN_DEBOUNCE_SEC": "0",
    })
    subprocess.run(["bash", str(DRAIN)], env=env, capture_output=True, text=True, timeout=60)
    cost = (state / "drain-cost.jsonl").read_text()
    rec = json.loads([l for l in cost.splitlines() if l.strip()][0])
    assert rec["outcome"] == "skip_reflect_on_reflect"
    assert rec["entries"] == 0  # no model call, no cap consumed
    # And it was removed from the queue.
    assert (state / "pending_reflections.jsonl").read_text().strip() == ""


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
