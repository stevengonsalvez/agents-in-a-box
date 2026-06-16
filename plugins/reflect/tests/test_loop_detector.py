# ABOUTME: Regression tests for port SG5 — agent tool-loop detection.
# ABOUTME: Pins acceptance: 3x identical arms, A-B oscillation arms, next prompt fires mini-learning path.
"""Port SG5: per-session sliding window over (tool, arg-hash); a repeat or
oscillation arms the existing mini-learning watcher with reason='loop'."""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
sys.path.insert(0, str(PLUGIN_ROOT / "scripts"))

import loop_detector  # noqa: E402
from loop_detector import record_call  # noqa: E402

HOOK = PLUGIN_ROOT / "hooks" / "posttooluse_minilearning.py"


@pytest.fixture(autouse=True)
def _isolated_state(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    yield tmp_path


# ---------- unit: detector ----------

def test_three_identical_calls_detects_repeat():
    sid = "s1"
    assert record_call(sid, "Bash", {"command": "ls"}) is None
    assert record_call(sid, "Bash", {"command": "ls"}) is None
    hit = record_call(sid, "Bash", {"command": "ls"})
    assert hit and hit["kind"] == "repeat" and hit["tool"] == "Bash"


def test_different_args_do_not_detect():
    sid = "s2"
    assert record_call(sid, "Bash", {"command": "ls a"}) is None
    assert record_call(sid, "Bash", {"command": "ls b"}) is None
    assert record_call(sid, "Bash", {"command": "ls c"}) is None


def test_oscillation_detects():
    sid = "s3"
    a = ("Read", {"file": "x.py"})
    b = ("Edit", {"file": "x.py", "change": "y"})
    assert record_call(sid, *a) is None
    assert record_call(sid, *b) is None
    assert record_call(sid, *a) is None
    hit = record_call(sid, *b)
    assert hit and hit["kind"] == "oscillation"
    assert "Read" in hit["tool"] and "Edit" in hit["tool"]


def test_window_resets_after_detection():
    sid = "s4"
    for _ in range(2):
        record_call(sid, "Bash", {"command": "x"})
    assert record_call(sid, "Bash", {"command": "x"})  # detection
    # window reset — takes 3 more identical calls to re-arm
    assert record_call(sid, "Bash", {"command": "x"}) is None
    assert record_call(sid, "Bash", {"command": "x"}) is None
    assert record_call(sid, "Bash", {"command": "x"})


def test_state_survives_processes(tmp_path):
    """Each PostToolUse is a fresh process — state must persist on disk."""
    sid = "s5"
    record_call(sid, "Grep", {"q": "foo"})
    state_file = tmp_path / "loops" / "s5.json"
    assert state_file.exists()
    calls = json.loads(state_file.read_text())["calls"]
    assert len(calls) == 1 and calls[0]["tool"] == "Grep"


def test_never_raises_on_garbage():
    assert record_call("", "Bash", {}) is None
    assert record_call("s6", "", {}) is None
    assert record_call("s6", "Bash", object()) is None  # unhashable-ish input


# ---------- integration: hook arms with reason=loop ----------

def _fire_hook(tmp_path: Path, event: dict) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(HOOK)],
        input=json.dumps(event), capture_output=True, text=True,
        env={**os.environ, "REFLECT_STATE_DIR": str(tmp_path)},
        timeout=20,
    )


def test_hook_arms_on_successful_loop(tmp_path):
    """3 identical SUCCESSFUL calls arm the watcher (failure not required)."""
    event = {
        "session_id": "sess-loop",
        "tool": "Bash",
        "tool_input": {"command": "npm test"},
        "tool_response": {"exit_code": 0, "stdout": "ok"},
    }
    for _ in range(3):
        r = _fire_hook(tmp_path, event)
        assert r.returncode == 0, r.stderr
    armed = tmp_path / "armed" / "sess-loop.json"
    assert armed.exists(), "loop of successes must arm"
    payload = json.loads(armed.read_text())
    assert payload["reason"] == "loop"
    assert payload["loop"]["kind"] == "repeat"


def test_hook_failure_path_still_arms_with_reason_failure(tmp_path):
    event = {
        "session_id": "sess-fail",
        "tool": "Bash",
        "tool_input": {"command": "false"},
        "tool_response": {"exit_code": 1, "stderr": "boom"},
    }
    r = _fire_hook(tmp_path, event)
    assert r.returncode == 0, r.stderr
    payload = json.loads((tmp_path / "armed" / "sess-fail.json").read_text())
    assert payload["reason"] == "failure"
    assert "loop" not in payload


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
