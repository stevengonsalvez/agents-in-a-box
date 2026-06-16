# ABOUTME: Regression tests for port SG4 — test-outcome parsing from Bash tool output.
# ABOUTME: Pins acceptance: 4 runners parsed, fix => HIGH learning, tier promotion on consistent pass, state cleanup.
"""Port SG4: parse pytest/jest/cargo/go output from Bash tool_response; a
per-session failure-count state machine turns N->0 into a HIGH-confidence
learning, 0->N into a test-regression contradiction arm, and an all-pass
after multiple failures into a tier promotion of the session's learnings."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
sys.path.insert(0, str(PLUGIN_ROOT / "scripts"))

import test_outcome_parser as top  # noqa: E402
from test_outcome_parser import (  # noqa: E402
    cleanup_session,
    cleanup_stale,
    observe_bash,
    parse_test_output,
    promote_session_learnings,
    record_outcome,
)

POSTTOOL_HOOK = PLUGIN_ROOT / "hooks" / "posttooluse_minilearning.py"
STOP_HOOK = PLUGIN_ROOT / "hooks" / "stop_reflect.py"


@pytest.fixture(autouse=True)
def _isolated_state(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    monkeypatch.setenv("REFLECT_LEARNINGS_DIR", str(tmp_path / "learnings"))
    yield tmp_path


# ---------- acceptance 1: pytest/jest/cargo/go all parsed ----------

PYTEST_FAIL = (
    "FAILED tests/test_x.py::test_a - AssertionError\n"
    "=========== 3 failed, 10 passed in 1.23s ===========\n"
)
PYTEST_PASS_Q = "13 passed in 0.84s\n"  # pytest -q shape (no = rails)
JEST_FAIL = (
    "Test Suites: 1 failed, 2 passed, 3 total\n"
    "Tests:       2 failed, 7 passed, 9 total\n"
    "Snapshots:   0 total\n"
)
JEST_PASS = "Tests:       9 passed, 9 total\n"
CARGO_FAIL = "test result: FAILED. 8 passed; 2 failed; 0 ignored; 0 measured\n"
CARGO_PASS = (
    "test result: ok. 8 passed; 0 failed; 0 ignored\n"
    "test result: ok. 4 passed; 0 failed; 0 ignored\n"
)
GO_FAIL_V = (
    "--- PASS: TestAlpha (0.01s)\n"
    "--- FAIL: TestBeta (0.02s)\n"
    "--- FAIL: TestGamma (0.02s)\n"
    "FAIL\ngithub.com/x/y\t0.05s\n"
)
GO_PASS_PKG = "ok  \tgithub.com/x/y\t0.512s\n"


def test_pytest_parsed():
    o = parse_test_output(PYTEST_FAIL)
    assert o and o.runner == "pytest" and o.failed == 3 and o.passed == 10
    o2 = parse_test_output(PYTEST_PASS_Q)
    assert o2 and o2.runner == "pytest" and o2.failed == 0 and o2.passed == 13


def test_jest_parsed():
    o = parse_test_output(JEST_FAIL)
    assert o and o.runner == "jest" and o.failed == 2 and o.passed == 7
    o2 = parse_test_output(JEST_PASS)
    assert o2 and o2.failed == 0 and o2.passed == 9


def test_cargo_parsed_and_summed_across_binaries():
    o = parse_test_output(CARGO_FAIL)
    assert o and o.runner == "cargo" and o.failed == 2 and o.passed == 8
    o2 = parse_test_output(CARGO_PASS)
    assert o2 and o2.failed == 0 and o2.passed == 12


def test_go_parsed_verbose_and_package():
    o = parse_test_output(GO_FAIL_V)
    assert o and o.runner == "go" and o.failed == 2 and o.passed == 1
    o2 = parse_test_output(GO_PASS_PKG)
    assert o2 and o2.runner == "go" and o2.failed == 0 and o2.passed == 1


def test_non_test_output_is_ignored():
    assert parse_test_output("total 48\ndrwxr-xr-x 12 user staff\n") is None
    assert parse_test_output("ok\n") is None  # bare 'ok' (no go package shape)
    assert parse_test_output("") is None
    assert parse_test_output(None) is None  # type: ignore[arg-type]


# ---------- acceptance 2: fix transition => HIGH-confidence learning ----------

def _learning_files(tmp_path: Path) -> list[Path]:
    d = tmp_path / "learnings"
    return sorted(d.glob("lrn-test-fix-*.md")) if d.is_dir() else []


def test_fix_transition_writes_high_confidence_learning(tmp_path):
    sid = "sess-fix"
    cmd = {"command": "uv run pytest -q"}
    assert observe_bash(sid, cmd, {"stdout": PYTEST_FAIL, "stderr": ""}) is None
    hit = observe_bash(sid, cmd, {"stdout": PYTEST_PASS_Q, "stderr": ""})
    assert hit and hit["kind"] == "fix" and hit["runner"] == "pytest"
    files = _learning_files(tmp_path)
    assert len(files) == 1, "fix must write exactly one learning"
    body = files[0].read_text()
    assert "confidence: high" in body
    assert "source: test-outcome" in body
    assert f"session_id: {sid}" in body
    assert "uv run pytest -q" in body


def test_no_transition_without_prior_failure(tmp_path):
    sid = "sess-greenfield"
    hit = observe_bash(sid, {}, {"stdout": PYTEST_PASS_Q})
    assert hit is None
    assert _learning_files(tmp_path) == []


def test_regression_transition_signals_contradiction():
    sid = "sess-reg"
    assert observe_bash(sid, {}, {"stdout": JEST_PASS}) is None
    hit = observe_bash(sid, {}, {"stdout": JEST_FAIL})
    assert hit and hit["kind"] == "regression"
    assert hit["new_failed"] == 2 and hit["prev_failed"] == 0


def test_runners_tracked_independently():
    """pytest failures then a jest pass is NOT a fix — state is per-runner."""
    sid = "sess-mixed"
    assert observe_bash(sid, {}, {"stdout": PYTEST_FAIL}) is None
    assert observe_bash(sid, {}, {"stdout": JEST_PASS}) is None


# ---------- acceptance 3: tier promotion fires on consistent pass ----------

def _write_mini_learning(tmp_path: Path, sid: str) -> Path:
    d = tmp_path / "learnings"
    d.mkdir(parents=True, exist_ok=True)
    p = d / f"lrn-mini-123-{sid[:8]}.md"
    p.write_text(
        f"---\nid: {p.stem}\nconfidence: low\nsource: posttooluse-minilearning\n"
        f"session_id: {sid}\ncaptured_at: 2026-06-10T00:00:00Z\n---\n\n# Mini\n",
        encoding="utf-8",
    )
    return p


def test_tier_promotion_on_all_pass_after_multiple_failures(tmp_path):
    sid = "sess-promote"
    mini = _write_mini_learning(tmp_path, sid)
    # Two failing runs ("multiple failures"), then green.
    assert observe_bash(sid, {}, {"stdout": PYTEST_FAIL}) is None
    assert observe_bash(sid, {}, {"stdout": "1 failed, 12 passed in 0.9s\n"}) is None
    hit = observe_bash(sid, {}, {"stdout": PYTEST_PASS_Q})
    assert hit and hit["kind"] == "fix"
    assert hit["promoted"] == 1, "prior session learning must be promoted"
    body = mini.read_text()
    assert "validated: true" in body
    assert "confidence: medium" in body  # low -> medium (one tier up)


def test_no_promotion_on_single_failure_fix(tmp_path):
    sid = "sess-single"
    mini = _write_mini_learning(tmp_path, sid)
    assert observe_bash(sid, {}, {"stdout": PYTEST_FAIL}) is None
    hit = observe_bash(sid, {}, {"stdout": PYTEST_PASS_Q})
    assert hit and hit["kind"] == "fix" and hit["promoted"] == 0
    assert "validated" not in mini.read_text()


def test_promotion_skips_other_sessions_and_own_fix_learnings(tmp_path):
    sid = "sess-promo2"
    other = _write_mini_learning(tmp_path, "sess-other")
    mine = _write_mini_learning(tmp_path, sid)
    assert promote_session_learnings(sid) == 1
    assert "validated: true" in mine.read_text()
    assert "validated" not in other.read_text()
    # Idempotent — already-validated learnings aren't double-promoted.
    assert promote_session_learnings(sid) == 0
    assert mine.read_text().count("validated: true") == 1


# ---------- acceptance 4: state cleaned up at session end ----------

def test_state_persisted_per_session(tmp_path):
    sid = "sess-state"
    observe_bash(sid, {}, {"stdout": PYTEST_FAIL})
    sf = tmp_path / "state" / "test-state" / f"{sid}.json"
    assert sf.exists()
    data = json.loads(sf.read_text())
    assert data["runners"]["pytest"][-1]["failed"] == 3


def test_cleanup_session_removes_state(tmp_path):
    sid = "sess-clean"
    observe_bash(sid, {}, {"stdout": PYTEST_FAIL})
    sf = tmp_path / "state" / "test-state" / f"{sid}.json"
    assert sf.exists()
    cleanup_session(sid)
    assert not sf.exists()
    cleanup_session(sid)  # idempotent, never raises
    cleanup_session("")


def test_cleanup_stale_reaps_expired_keeps_fresh(tmp_path):
    observe_bash("sess-old", {}, {"stdout": PYTEST_FAIL})
    observe_bash("sess-new", {}, {"stdout": PYTEST_FAIL})
    d = tmp_path / "state" / "test-state"
    old = d / "sess-old.json"
    expired = time.time() - top._STATE_TTL_S - 60
    os.utime(old, (expired, expired))
    assert cleanup_stale() == 1
    assert not old.exists()
    assert (d / "sess-new.json").exists()


def test_ttl_expired_state_treated_as_fresh_session():
    """An expired history must not produce a bogus fix transition."""
    sid = "sess-ttl"
    o = parse_test_output(PYTEST_FAIL)
    assert record_outcome(sid, o) is None
    # Manually expire the state file's 'updated' stamp.
    sf = top._state_path(sid)
    data = json.loads(sf.read_text())
    data["updated"] = time.time() - top._STATE_TTL_S - 60
    sf.write_text(json.dumps(data))
    o2 = parse_test_output(PYTEST_PASS_Q)
    assert record_outcome(sid, o2) is None  # no prior history => no transition


def test_never_raises_on_garbage():
    assert observe_bash("", {}, {"stdout": PYTEST_PASS_Q}) is None
    assert observe_bash("s", None, None) is None
    assert observe_bash("s", object(), object()) is None
    assert record_outcome("s", None) is None  # type: ignore[arg-type]
    assert promote_session_learnings("") == 0


# ---------- integration: hooks ----------

def _env(tmp_path: Path) -> dict:
    return {
        **os.environ,
        "REFLECT_STATE_DIR": str(tmp_path / "state"),
        "REFLECT_LEARNINGS_DIR": str(tmp_path / "learnings"),
    }


def _fire(hook: Path, tmp_path: Path, event: dict) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(hook)],
        input=json.dumps(event), capture_output=True, text=True,
        env=_env(tmp_path), timeout=20,
    )


def test_hook_regression_arms_with_test_regression_reason(tmp_path):
    sid = "sess-hookreg"
    pass_event = {
        "session_id": sid,
        "tool": "Bash",
        "tool_input": {"command": "pytest -q"},
        "tool_response": {"exit_code": 0, "stdout": PYTEST_PASS_Q},
    }
    fail_event = {
        "session_id": sid,
        "tool": "Bash",
        "tool_input": {"command": "pytest -q"},
        "tool_response": {"exit_code": 1, "stdout": PYTEST_FAIL},
    }
    r = _fire(POSTTOOL_HOOK, tmp_path, pass_event)
    assert r.returncode == 0, r.stderr
    assert not (tmp_path / "state" / "armed" / f"{sid}.json").exists()
    r = _fire(POSTTOOL_HOOK, tmp_path, fail_event)
    assert r.returncode == 0, r.stderr
    payload = json.loads((tmp_path / "state" / "armed" / f"{sid}.json").read_text())
    assert payload["reason"] == "test-regression"
    assert payload["test_outcome"]["kind"] == "regression"
    assert payload["test_outcome"]["new_failed"] == 3


def test_hook_fix_writes_learning_without_arming(tmp_path):
    sid = "sess-hookfix"
    fail_event = {
        "session_id": sid,
        "tool": "Bash",
        "tool_input": {"command": "pytest -q"},
        "tool_response": {"exit_code": 1, "stdout": PYTEST_FAIL},
    }
    pass_event = {
        "session_id": sid,
        "tool": "Bash",
        "tool_input": {"command": "pytest -q"},
        "tool_response": {"exit_code": 0, "stdout": PYTEST_PASS_Q},
    }
    r = _fire(POSTTOOL_HOOK, tmp_path, fail_event)
    assert r.returncode == 0, r.stderr
    # Failure arms (existing behavior) — clear it to isolate the fix step.
    armed = tmp_path / "state" / "armed" / f"{sid}.json"
    assert armed.exists()
    armed.unlink()
    r = _fire(POSTTOOL_HOOK, tmp_path, pass_event)
    assert r.returncode == 0, r.stderr
    assert not armed.exists(), "fix must not arm"
    files = list((tmp_path / "learnings").glob("lrn-test-fix-*.md"))
    assert len(files) == 1
    assert "confidence: high" in files[0].read_text()


def test_stop_hook_sweeps_stale_test_state(tmp_path):
    observe_bash_env = _env(tmp_path)
    d = tmp_path / "state" / "test-state"
    d.mkdir(parents=True, exist_ok=True)
    stale = d / "sess-dead.json"
    stale.write_text(json.dumps({"updated": 0, "runners": {}}))
    expired = time.time() - top._STATE_TTL_S - 60
    os.utime(stale, (expired, expired))
    fresh = d / "sess-live.json"
    fresh.write_text(json.dumps({"updated": time.time(), "runners": {}}))
    r = subprocess.run(
        [sys.executable, str(STOP_HOOK)],
        input=json.dumps({"session_id": "sess-live", "transcript_path": ""}),
        capture_output=True, text=True, env=observe_bash_env, timeout=20,
    )
    assert r.returncode == 0, r.stderr
    assert not stale.exists(), "stop hook must reap expired test state"
    assert fresh.exists(), "live session state must survive the sweep"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
