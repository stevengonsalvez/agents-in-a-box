# ABOUTME: Regression tests for port SG7 — TodoWrite completion as capture signal.
# ABOUTME: Pins acceptance: completed todo + file events => candidate learning, duration tracked, confidence=MEDIUM.
"""Port SG7: PostToolUse diffs per-session todo state on TodoWrite calls.
Items transitioning to status='completed' produce a candidate learning
('how I accomplished X') carrying the item content, the prior in_progress
duration, and the files touched while the item was in progress — tagged
category=Process / confidence=medium / source=todo-completion."""

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

import todo_state as ts  # noqa: E402
from todo_state import (  # noqa: E402
    cleanup_session,
    cleanup_stale,
    observe_todowrite,
    record_file_event,
    write_todo_learning,
)

POSTTOOL_HOOK = PLUGIN_ROOT / "hooks" / "posttooluse_minilearning.py"
STOP_HOOK = PLUGIN_ROOT / "hooks" / "stop_reflect.py"


@pytest.fixture(autouse=True)
def _isolated_state(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    monkeypatch.setenv("REFLECT_LEARNINGS_DIR", str(tmp_path / "learnings"))
    yield tmp_path


def _todos(*items: tuple[str, str]) -> dict:
    return {"todos": [{"content": c, "status": s, "activeForm": c} for c, s in items]}


def _learning_files(tmp_path: Path) -> list[Path]:
    d = tmp_path / "learnings"
    return sorted(d.glob("lrn-todo-done-*.md")) if d.is_dir() else []


# ---------- acceptance 1: completed todo + file events => candidate learning ----------

def test_completion_with_file_events_produces_learning(tmp_path):
    sid = "sess-todo1"
    # pending -> in_progress -> (files touched) -> completed
    assert observe_todowrite(sid, _todos(("Fix auth bug", "pending"))) is None
    assert observe_todowrite(sid, _todos(("Fix auth bug", "in_progress"))) is None
    record_file_event(sid, "Edit", {"file_path": "/repo/src/auth.py"})
    record_file_event(sid, "Write", {"file_path": "/repo/tests/test_auth.py"})
    hit = observe_todowrite(sid, _todos(("Fix auth bug", "completed")))
    assert hit and len(hit["completed"]) == 1
    done = hit["completed"][0]
    assert done["content"] == "Fix auth bug"
    assert done["files"] == ["/repo/src/auth.py", "/repo/tests/test_auth.py"]
    assert done["learning"], "completion must write a candidate learning"

    files = _learning_files(tmp_path)
    assert len(files) == 1
    body = files[0].read_text()
    assert "source: todo-completion" in body
    assert f"session_id: {sid}" in body
    assert "Fix auth bug" in body
    assert "/repo/src/auth.py" in body
    assert "/repo/tests/test_auth.py" in body


def test_files_before_in_progress_are_excluded(tmp_path):
    """Only events since the item went in_progress are attributed to it."""
    sid = "sess-window"
    observe_todowrite(sid, _todos(("Task A", "pending")))
    record_file_event(sid, "Edit", {"file_path": "/repo/unrelated.py"})
    time.sleep(0.01)
    observe_todowrite(sid, _todos(("Task A", "in_progress")))
    record_file_event(sid, "Edit", {"file_path": "/repo/related.py"})
    hit = observe_todowrite(sid, _todos(("Task A", "completed")))
    assert hit
    assert hit["completed"][0]["files"] == ["/repo/related.py"]


def test_unseen_completed_item_has_no_baseline_and_is_skipped(tmp_path):
    """First-ever TodoWrite arriving already-completed isn't a transition."""
    sid = "sess-noprior"
    assert observe_todowrite(sid, _todos(("Did it already", "completed"))) is None
    assert _learning_files(tmp_path) == []


def test_repeated_completed_list_is_idempotent(tmp_path):
    sid = "sess-idem"
    observe_todowrite(sid, _todos(("Task B", "in_progress")))
    assert observe_todowrite(sid, _todos(("Task B", "completed"))) is not None
    # Same completed list resent — stored status is already 'completed'.
    assert observe_todowrite(sid, _todos(("Task B", "completed"))) is None
    assert len(_learning_files(tmp_path)) == 1


def test_multiple_completions_in_one_call(tmp_path):
    sid = "sess-multi"
    observe_todowrite(sid, _todos(("T1", "in_progress"), ("T2", "pending")))
    hit = observe_todowrite(sid, _todos(("T1", "completed"), ("T2", "completed")))
    assert hit and len(hit["completed"]) == 2
    assert len(_learning_files(tmp_path)) == 2


def test_file_events_ignored_until_session_has_todo_state(tmp_path):
    """record_file_event is a no-op before the first TodoWrite — sessions
    that never use todos never pay for a state file."""
    sid = "sess-notodos"
    record_file_event(sid, "Edit", {"file_path": "/repo/x.py"})
    assert not (tmp_path / "state" / "todo-state" / f"{sid}.json").exists()


def test_non_file_tools_do_not_log_events(tmp_path):
    sid = "sess-nonfile"
    observe_todowrite(sid, _todos(("T", "in_progress")))
    record_file_event(sid, "Bash", {"command": "ls"})
    record_file_event(sid, "Read", {"file_path": "/repo/readonly.py"})
    hit = observe_todowrite(sid, _todos(("T", "completed")))
    assert hit and hit["completed"][0]["files"] == []


# ---------- acceptance 2: duration tracked ----------

def test_in_progress_duration_tracked(tmp_path):
    sid = "sess-dur"
    observe_todowrite(sid, _todos(("Slow task", "in_progress")))
    time.sleep(0.05)
    hit = observe_todowrite(sid, _todos(("Slow task", "completed")))
    assert hit
    dur = hit["completed"][0]["duration_s"]
    assert dur is not None and dur >= 0.05
    body = _learning_files(tmp_path)[0].read_text()
    assert "duration_s: " in body  # frontmatter carries the tracked duration


def test_duration_untracked_when_item_never_in_progress(tmp_path):
    sid = "sess-nodur"
    observe_todowrite(sid, _todos(("Quick task", "pending")))
    hit = observe_todowrite(sid, _todos(("Quick task", "completed")))
    assert hit
    assert hit["completed"][0]["duration_s"] is None
    body = _learning_files(tmp_path)[0].read_text()
    assert "duration_s: " not in body
    assert "untracked" in body


def test_in_progress_since_survives_intermediate_todowrites(tmp_path):
    """Re-sending in_progress for an already-in_progress item must NOT
    reset the duration clock."""
    sid = "sess-clock"
    observe_todowrite(sid, _todos(("T", "in_progress")))
    time.sleep(0.05)
    observe_todowrite(sid, _todos(("T", "in_progress")))  # re-sent, same status
    hit = observe_todowrite(sid, _todos(("T", "completed")))
    assert hit and hit["completed"][0]["duration_s"] >= 0.05


# ---------- acceptance 3: confidence=MEDIUM (category=Process) ----------

def test_learning_is_medium_confidence_process_category(tmp_path):
    sid = "sess-conf"
    observe_todowrite(sid, _todos(("Tag check", "in_progress")))
    observe_todowrite(sid, _todos(("Tag check", "completed")))
    body = _learning_files(tmp_path)[0].read_text()
    assert "confidence: medium" in body
    assert "category: Process" in body
    assert "source: todo-completion" in body


def test_write_todo_learning_direct(tmp_path):
    slug = write_todo_learning("sess-direct", "Do thing", 42.0, ["/a.py"])
    assert slug and slug.startswith("lrn-todo-done-")
    body = (tmp_path / "learnings" / f"{slug}.md").read_text()
    assert "confidence: medium" in body
    assert "duration_s: 42" in body
    assert "/a.py" in body


# ---------- robustness ----------

def test_never_raises_on_garbage():
    assert observe_todowrite("", _todos(("x", "completed"))) is None
    assert observe_todowrite("s", None) is None
    assert observe_todowrite("s", {"todos": "not-a-list"}) is None
    assert observe_todowrite("s", {"todos": [None, 42, {"content": ""}]}) is None
    record_file_event("", "Edit", {"file_path": "/x"})
    record_file_event("s", "", {"file_path": "/x"})
    record_file_event("s", "Edit", "not-a-dict")
    assert write_todo_learning("s", "", None, []) is not None


def test_ttl_expired_state_treated_as_fresh(tmp_path):
    """Expired state must not produce a bogus completion transition."""
    sid = "sess-ttl"
    observe_todowrite(sid, _todos(("T", "in_progress")))
    sf = ts._state_path(sid)
    data = json.loads(sf.read_text())
    data["updated"] = time.time() - ts._STATE_TTL_S - 60
    sf.write_text(json.dumps(data))
    assert observe_todowrite(sid, _todos(("T", "completed"))) is None


def test_cleanup_session_and_stale(tmp_path):
    observe_todowrite("sess-old", _todos(("a", "pending")))
    observe_todowrite("sess-new", _todos(("b", "pending")))
    d = tmp_path / "state" / "todo-state"
    old = d / "sess-old.json"
    expired = time.time() - ts._STATE_TTL_S - 60
    os.utime(old, (expired, expired))
    assert cleanup_stale() == 1
    assert not old.exists()
    assert (d / "sess-new.json").exists()
    cleanup_session("sess-new")
    assert not (d / "sess-new.json").exists()
    cleanup_session("sess-new")  # idempotent
    cleanup_session("")


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


def test_hook_end_to_end_completion_capture(tmp_path):
    """Full PostToolUse flow: TodoWrite in_progress -> Edit -> TodoWrite
    completed produces the MEDIUM-confidence learning, without arming."""
    sid = "sess-hook"
    r = _fire(POSTTOOL_HOOK, tmp_path, {
        "session_id": sid,
        "tool": "TodoWrite",
        "tool_input": _todos(("Ship feature", "in_progress")),
        "tool_response": {"success": True},
    })
    assert r.returncode == 0 and r.stdout == "", r.stderr
    r = _fire(POSTTOOL_HOOK, tmp_path, {
        "session_id": sid,
        "tool": "Edit",
        "tool_input": {"file_path": "/repo/feature.py", "old_string": "a", "new_string": "b"},
        "tool_response": {"success": True},
    })
    assert r.returncode == 0, r.stderr
    r = _fire(POSTTOOL_HOOK, tmp_path, {
        "session_id": sid,
        "tool": "TodoWrite",
        "tool_input": _todos(("Ship feature", "completed")),
        "tool_response": {"success": True},
    })
    assert r.returncode == 0 and r.stdout == "", r.stderr
    files = list((tmp_path / "learnings").glob("lrn-todo-done-*.md"))
    assert len(files) == 1
    body = files[0].read_text()
    assert "confidence: medium" in body
    assert "category: Process" in body
    assert "Ship feature" in body
    assert "/repo/feature.py" in body
    # Successful TodoWrite must NOT arm the mini-learning watcher.
    assert not (tmp_path / "state" / "armed" / f"{sid}.json").exists()


def test_stop_hook_sweeps_stale_todo_state(tmp_path):
    d = tmp_path / "state" / "todo-state"
    d.mkdir(parents=True, exist_ok=True)
    stale = d / "sess-dead.json"
    stale.write_text(json.dumps({"updated": 0, "todos": {}, "files": []}))
    expired = time.time() - ts._STATE_TTL_S - 60
    os.utime(stale, (expired, expired))
    fresh = d / "sess-live.json"
    fresh.write_text(json.dumps({"updated": time.time(), "todos": {}, "files": []}))
    r = subprocess.run(
        [sys.executable, str(STOP_HOOK)],
        input=json.dumps({"session_id": "sess-live", "transcript_path": ""}),
        capture_output=True, text=True, env=_env(tmp_path), timeout=20,
    )
    assert r.returncode == 0, r.stderr
    assert not stale.exists()
    assert fresh.exists()


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
