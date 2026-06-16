# ABOUTME: Regression tests for port SG8 — permission prompt + reply capture.
# ABOUTME: Pins acceptance: (1) permission_prompt notification fires arming
# ABOUTME: file, (2) approval/deny regex captures the reply as a learning,
# ABOUTME: (3) 'always'/'never' replies are marked HIGH confidence.
"""Port SG8: two-phase permission-decision capture (agentmemory pattern).

Phase 1: notification_reflect.py (Notification hook) arms
``~/.reflect/permission-armed/<sid>.json`` when the notification is a
permission prompt.

Phase 2: user_prompt_submit_recall.py reads the armed file on the next
UserPromptSubmit and, if the prompt reads like a permission decision,
writes a ``source: permission-pattern`` learning. Durable replies
('yes always' / 'no never' / 'only for X') are HIGH confidence; plain
approve/deny is MEDIUM.
"""

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
NOTIFICATION_HOOK = PLUGIN_ROOT / "hooks" / "notification_reflect.py"
USER_PROMPT_HOOK = PLUGIN_ROOT / "skills" / "recall" / "hooks" / "user_prompt_submit_recall.py"

sys.path.insert(0, str(USER_PROMPT_HOOK.parent))
from user_prompt_submit_recall import classify_permission_reply  # noqa: E402


def _run(hook: Path, stdin: str, state_dir: Path, *, extra_env=None) -> subprocess.CompletedProcess[str]:
    env = {**os.environ, "REFLECT_STATE_DIR": str(state_dir)}
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [sys.executable, str(hook)],
        input=stdin,
        capture_output=True,
        text=True,
        env=env,
        timeout=20,
    )


def _arm(state_dir: Path, session_id: str, *, tool: str = "Bash", ts: float | None = None) -> Path:
    """Pre-write a permission-armed file as the Notification hook would."""
    armed = state_dir / "permission-armed" / f"{session_id}.json"
    armed.parent.mkdir(parents=True, exist_ok=True)
    armed.write_text(json.dumps({
        "tool": tool,
        "message": f"Claude needs your permission to use {tool}",
        "title": "Permission required",
        "cwd": "/tmp/project",
        "ts": ts if ts is not None else time.time(),
    }), encoding="utf-8")
    return armed


def _learnings(learnings_dir: Path) -> list[Path]:
    return sorted(learnings_dir.glob("lrn-perm-*.md"))


# --- Acceptance 1: permission_prompt fires arming file ----------------------


def test_notification_permission_prompt_arms(tmp_path):
    """Explicit notification_type=permission_prompt writes the armed file."""
    payload = json.dumps({
        "session_id": "sess-perm",
        "notification_type": "permission_prompt",
        "message": "Claude needs your permission to use Bash",
        "title": "Permission required",
        "cwd": "/tmp/project",
    })
    result = _run(NOTIFICATION_HOOK, payload, tmp_path)
    assert result.returncode == 0
    assert result.stdout == ""  # side-effect only

    armed = tmp_path / "permission-armed" / "sess-perm.json"
    assert armed.exists()
    data = json.loads(armed.read_text())
    assert data["tool"] == "Bash"
    assert "permission" in data["message"].lower()
    assert isinstance(data["ts"], (int, float))


def test_notification_message_shape_fallback_arms(tmp_path):
    """Without an explicit notification_type, the well-known permission
    phrasing in `message` is enough to arm (Claude Code doesn't always
    send the type field)."""
    payload = json.dumps({
        "session_id": "sess-msg",
        "message": "Claude needs your permission to use WebFetch",
    })
    _run(NOTIFICATION_HOOK, payload, tmp_path)
    armed = tmp_path / "permission-armed" / "sess-msg.json"
    assert armed.exists()
    assert json.loads(armed.read_text())["tool"] == "WebFetch"


def test_notification_non_permission_does_not_arm(tmp_path):
    """Idle / waiting-for-input notifications must NOT arm."""
    for message in [
        "Claude is waiting for your input",
        "Task completed",
    ]:
        payload = json.dumps({"session_id": "sess-idle", "message": message})
        result = _run(NOTIFICATION_HOOK, payload, tmp_path)
        assert result.returncode == 0
        assert result.stdout == ""
    assert not (tmp_path / "permission-armed").exists()


def test_notification_explicit_other_type_does_not_arm(tmp_path):
    """An explicit non-permission notification_type wins over message shape."""
    payload = json.dumps({
        "session_id": "sess-other",
        "notification_type": "idle_timeout",
        "message": "Claude needs your permission to use Bash",
    })
    _run(NOTIFICATION_HOOK, payload, tmp_path)
    assert not (tmp_path / "permission-armed" / "sess-other.json").exists()


def test_notification_no_session_id_is_noop(tmp_path):
    payload = json.dumps({
        "notification_type": "permission_prompt",
        "message": "Claude needs your permission to use Bash",
    })
    result = _run(NOTIFICATION_HOOK, payload, tmp_path)
    assert result.returncode == 0
    assert result.stdout == ""
    assert not (tmp_path / "permission-armed").exists()


def test_notification_silent_on_garbage_input(tmp_path):
    """Silent-fail invariant: garbage stdin → exit 0, empty stdout."""
    for payload in ["", "not json", "[]", "42"]:
        result = _run(NOTIFICATION_HOOK, payload, tmp_path)
        assert result.returncode == 0
        assert result.stdout == "", f"stdout pollution on {payload!r}: {result.stdout!r}"


# --- Acceptance 2: approval/deny regex captures reply ------------------------


def test_plain_approval_captures_medium_learning(tmp_path):
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    armed = _arm(state, "sess-yes")

    payload = json.dumps({"session_id": "sess-yes", "prompt": "yes"})
    result = _run(USER_PROMPT_HOOK, payload, state,
                  extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})
    assert result.returncode == 0

    files = _learnings(ld)
    assert len(files) == 1
    body = files[0].read_text()
    assert "source: permission-pattern" in body
    assert "confidence: medium" in body
    assert "allow-once" in body
    assert "Bash" in body
    assert not armed.exists(), "armed file must be cleared after capture (single shot)"


def test_plain_deny_captures_medium_learning(tmp_path):
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    _arm(state, "sess-no")

    payload = json.dumps({"session_id": "sess-no", "prompt": "no"})
    _run(USER_PROMPT_HOOK, payload, state,
         extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})

    files = _learnings(ld)
    assert len(files) == 1
    body = files[0].read_text()
    assert "confidence: medium" in body
    assert "deny-once" in body


def test_non_reply_prompt_clears_armed_without_learning(tmp_path):
    """Permission replies are single-shot: a non-matching next prompt means
    the moment passed — clear the watcher, write nothing."""
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    armed = _arm(state, "sess-skip")

    payload = json.dumps({"session_id": "sess-skip", "prompt": "what time"})
    _run(USER_PROMPT_HOOK, payload, state,
         extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})

    assert _learnings(ld) == []
    assert not armed.exists()


def test_no_armed_file_no_capture(tmp_path):
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    payload = json.dumps({"session_id": "sess-cold", "prompt": "yes"})
    _run(USER_PROMPT_HOOK, payload, state,
         extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})
    assert _learnings(ld) == []


def test_stale_armed_file_is_discarded(tmp_path):
    """An armed file older than 10 minutes is stale — no capture even if
    the prompt looks like a reply."""
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    armed = _arm(state, "sess-old", ts=time.time() - 3600)

    payload = json.dumps({"session_id": "sess-old", "prompt": "yes always"})
    _run(USER_PROMPT_HOOK, payload, state,
         extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})

    assert _learnings(ld) == []
    assert not armed.exists()


def test_corrupt_armed_file_is_cleared(tmp_path):
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    armed = state / "permission-armed" / "sess-bad.json"
    armed.parent.mkdir(parents=True, exist_ok=True)
    armed.write_text("not json", encoding="utf-8")

    payload = json.dumps({"session_id": "sess-bad", "prompt": "yes always"})
    result = _run(USER_PROMPT_HOOK, payload, state,
                  extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})
    assert result.returncode == 0
    assert _learnings(ld) == []
    assert not armed.exists()


# --- Acceptance 3: 'always'/'never' replies marked HIGH confidence -----------


def test_yes_always_is_high_confidence(tmp_path):
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    _arm(state, "sess-alw")

    payload = json.dumps({"session_id": "sess-alw", "prompt": "yes always"})
    _run(USER_PROMPT_HOOK, payload, state,
         extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})

    files = _learnings(ld)
    assert len(files) == 1
    body = files[0].read_text()
    assert "confidence: high" in body
    assert "allow-always" in body


def test_no_never_is_high_confidence(tmp_path):
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    _arm(state, "sess-nev")

    payload = json.dumps({"session_id": "sess-nev", "prompt": "no never"})
    _run(USER_PROMPT_HOOK, payload, state,
         extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})

    files = _learnings(ld)
    assert len(files) == 1
    body = files[0].read_text()
    assert "confidence: high" in body
    assert "deny-always" in body


def test_only_for_is_high_confidence(tmp_path):
    state = tmp_path / "state"
    ld = tmp_path / "learnings"
    _arm(state, "sess-scp")

    payload = json.dumps({"session_id": "sess-scp", "prompt": "only for ci"})
    _run(USER_PROMPT_HOOK, payload, state,
         extra_env={"REFLECT_LEARNINGS_DIR": str(ld)})

    files = _learnings(ld)
    assert len(files) == 1
    body = files[0].read_text()
    assert "confidence: high" in body
    assert "allow-scoped" in body


# --- classify_permission_reply unit coverage --------------------------------


@pytest.mark.parametrize("prompt,expected", [
    ("yes always", ("allow-always", "high")),
    ("Yes, always allow this", ("allow-always", "high")),
    ("always allow bash in this repo", ("allow-always", "high")),
    ("no never", ("deny-always", "high")),
    ("No, never do that again", ("deny-always", "high")),
    ("don't ever run rm -rf here", ("deny-always", "high")),
    ("only for the tests directory", ("allow-scoped", "high")),
    ("yes", ("allow-once", "medium")),
    ("ok go ahead", ("allow-once", "medium")),
    ("approve", ("allow-once", "medium")),
    ("no", ("deny-once", "medium")),
    ("deny", ("deny-once", "medium")),
    ("cancel", ("deny-once", "medium")),
])
def test_classify_permission_reply(prompt, expected):
    assert classify_permission_reply(prompt) == expected


@pytest.mark.parametrize("prompt", [
    "what time is it",
    "refactor the parser to use a state machine",
    "can you explain this function",
    "",
])
def test_classify_rejects_non_replies(prompt):
    assert classify_permission_reply(prompt) is None


def test_no_never_outranks_plain_deny():
    """'no, never ...' must classify as deny-always (HIGH), not be swallowed
    by the plain '^no' deny pattern."""
    decision, confidence = classify_permission_reply("no, never allow that")
    assert decision == "deny-always"
    assert confidence == "high"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-q"]))
