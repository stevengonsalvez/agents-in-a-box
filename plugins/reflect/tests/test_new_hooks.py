"""Behavior tests for the three new hooks shipped in 3.6.0.

  * user_prompt_submit_recall.py — prompt-aware recall + per-session
    dedupe + inline mini-learning capture from armed state
  * posttooluse_minilearning.py — arms watcher on tool failure
  * stop_reflect.py — enqueue transcript on agent finish; dedupe vs
    PreCompact entries already in the queue

Silent-fail invariants are covered separately in test_hooks_silent_fail
— this file focuses on the new hooks' BEHAVIOR (what they write, what
they dedupe, what they skip).
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
USER_PROMPT_HOOK = PLUGIN_ROOT / "skills" / "recall" / "hooks" / "user_prompt_submit_recall.py"
POSTTOOLUSE_HOOK = PLUGIN_ROOT / "hooks" / "posttooluse_minilearning.py"
STOP_HOOK = PLUGIN_ROOT / "hooks" / "stop_reflect.py"


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


# --- PostToolUse arming -----------------------------------------------------


def test_posttooluse_skips_when_tool_succeeded(tmp_path):
    """No armed file should be written when the tool exited cleanly."""
    payload = json.dumps({
        "session_id": "sess-ok",
        "tool": "Bash",
        "tool_input": "ls",
        "tool_response": {"exit_code": 0, "stdout": "file1\n", "stderr": ""},
    })
    result = _run(POSTTOOLUSE_HOOK, payload, tmp_path)
    assert result.returncode == 0
    assert result.stdout == ""  # side-effect only
    assert not (tmp_path / "armed" / "sess-ok.json").exists()


def test_posttooluse_arms_on_nonzero_exit(tmp_path):
    """Bash exit≠0 should arm the watcher."""
    payload = json.dumps({
        "session_id": "sess-fail",
        "tool": "Bash",
        "tool_input": "curl https://bad.example",
        "tool_response": {"exit_code": 7, "stderr": "Could not resolve host"},
    })
    result = _run(POSTTOOLUSE_HOOK, payload, tmp_path)
    assert result.returncode == 0
    assert result.stdout == ""

    armed = tmp_path / "armed" / "sess-fail.json"
    assert armed.exists()
    data = json.loads(armed.read_text())
    assert data["tool"] == "Bash"
    assert "curl" in data["tool_input"]
    assert isinstance(data["ts"], (int, float))


def test_posttooluse_arms_on_is_error_flag(tmp_path):
    """``is_error: true`` should also count as failure."""
    payload = json.dumps({
        "session_id": "sess-err",
        "tool": "Edit",
        "tool_response": {"is_error": True, "error": "File not found"},
    })
    _run(POSTTOOLUSE_HOOK, payload, tmp_path)
    assert (tmp_path / "armed" / "sess-err.json").exists()


def test_posttooluse_no_session_id_is_noop(tmp_path):
    """Without a session_id we have no key to arm against — skip."""
    payload = json.dumps({"tool": "Bash", "tool_response": {"exit_code": 1}})
    result = _run(POSTTOOLUSE_HOOK, payload, tmp_path)
    assert result.returncode == 0
    assert result.stdout == ""
    assert not (tmp_path / "armed").exists()


def test_posttooluse_empty_stdout_always(tmp_path):
    """PostToolUse is side-effect only. Verify stdout is always empty
    regardless of input shape — codex schema sensitivity."""
    for payload in [
        "{}",
        '{"session_id":"x","tool_response":{}}',
        '{"session_id":"x","tool_response":{"exit_code":1}}',
        "not json at all",
    ]:
        result = _run(POSTTOOLUSE_HOOK, payload, tmp_path)
        assert result.returncode == 0
        assert result.stdout == "", f"stdout pollution on payload={payload!r}: {result.stdout!r}"


# --- Stop reflection enqueue -----------------------------------------------


def test_stop_enqueues_transcript(tmp_path):
    payload = json.dumps({
        "session_id": "sess-1",
        "transcript_path": "/tmp/transcript-1.jsonl",
        "cwd": "/some/project",
    })
    result = _run(STOP_HOOK, payload, tmp_path)
    assert result.returncode == 0
    assert result.stdout == ""

    queue = tmp_path / "pending_reflections.jsonl"
    assert queue.exists()
    lines = [l for l in queue.read_text().splitlines() if l.strip()]
    assert len(lines) == 1
    entry = json.loads(lines[0])
    assert entry["session_id"] == "sess-1"
    assert entry["transcript_path"] == "/tmp/transcript-1.jsonl"
    assert entry["trigger"] == "stop"


def test_stop_dedupes_against_precompact_entry(tmp_path):
    """If PreCompact already enqueued this session, Stop must skip
    (don't re-enqueue the same session for double drain cost)."""
    queue = tmp_path / "pending_reflections.jsonl"
    queue.write_text(json.dumps({
        "ts": "2026-05-21T10:00:00",
        "session_id": "long-session",
        "transcript_path": "/tmp/transcript-long.jsonl",
        "trigger": "auto",  # PreCompact-style entry
    }) + "\n")

    payload = json.dumps({
        "session_id": "long-session",
        "transcript_path": "/tmp/transcript-long.jsonl",
    })
    result = _run(STOP_HOOK, payload, tmp_path)
    assert result.returncode == 0

    lines = [l for l in queue.read_text().splitlines() if l.strip()]
    # Still exactly ONE entry — Stop dedupe'd.
    assert len(lines) == 1


def test_stop_enqueues_when_different_session(tmp_path):
    """Existing queue entry for a DIFFERENT session shouldn't block."""
    queue = tmp_path / "pending_reflections.jsonl"
    queue.write_text(json.dumps({
        "session_id": "other-session",
        "transcript_path": "/tmp/other.jsonl",
        "trigger": "auto",
    }) + "\n")

    payload = json.dumps({
        "session_id": "new-session",
        "transcript_path": "/tmp/new.jsonl",
    })
    _run(STOP_HOOK, payload, tmp_path)

    lines = [l for l in queue.read_text().splitlines() if l.strip()]
    assert len(lines) == 2


def test_stop_no_transcript_path_skips(tmp_path):
    payload = json.dumps({"session_id": "sess"})  # no transcript_path
    result = _run(STOP_HOOK, payload, tmp_path)
    assert result.returncode == 0
    assert not (tmp_path / "pending_reflections.jsonl").exists()


def test_stop_empty_stdout_always(tmp_path):
    """Stop schema in codex has no HookSpecificOutputWire — empty stdout required."""
    for payload in [
        "{}",
        '{"session_id":"x"}',
        '{"session_id":"x","transcript_path":"/tmp/y"}',
        "garbage",
    ]:
        result = _run(STOP_HOOK, payload, tmp_path)
        assert result.returncode == 0
        assert result.stdout == ""


# --- UserPromptSubmit recall + dedupe + mini-learning -----------------------


def test_user_prompt_recall_emits_valid_json(tmp_path):
    """Even with no learnings available, output must be valid JSON
    with hookEventName=UserPromptSubmit."""
    payload = json.dumps({
        "session_id": "sess-1",
        "prompt": "help me debug this OAuth bug in production",
    })
    result = _run(USER_PROMPT_HOOK, payload, tmp_path)
    assert result.returncode == 0
    obj = json.loads(result.stdout.strip() or "{}")
    assert obj["hookSpecificOutput"]["hookEventName"] == "UserPromptSubmit"
    # additionalContext can be empty (no graphrag in test env) — that's fine.
    assert "additionalContext" in obj["hookSpecificOutput"]


def test_user_prompt_recall_skips_short_prompts(tmp_path):
    """Prompts shorter than MIN_PROMPT_CHARS should inject empty
    context — too noisy to query graphrag on."""
    payload = json.dumps({"session_id": "s", "prompt": "hi"})
    result = _run(USER_PROMPT_HOOK, payload, tmp_path)
    assert result.returncode == 0
    obj = json.loads(result.stdout.strip())
    assert obj["hookSpecificOutput"]["additionalContext"] == ""


def test_user_prompt_recall_captures_mini_learning_on_correction(tmp_path):
    """Armed state + correction-pattern prompt → mini-learning written to disk."""
    # Pre-arm the watcher (PostToolUse would normally do this).
    armed = tmp_path / "armed" / "sess-correct.json"
    armed.parent.mkdir(parents=True, exist_ok=True)
    armed.write_text(json.dumps({
        "tool": "Bash",
        "tool_input": "curl https://api.example.com",
        "tool_response": '{"exit_code":1,"stderr":"401 Unauthorized"}',
        "ts": 1779456000.0,  # ~now
    }))

    learnings_dir = tmp_path / "learnings-out"
    payload = json.dumps({
        "session_id": "sess-correct",
        "prompt": "try --insecure instead, this is local dev",
    })
    result = _run(
        USER_PROMPT_HOOK, payload, tmp_path,
        extra_env={"REFLECT_LEARNINGS_DIR": str(learnings_dir)},
    )
    assert result.returncode == 0

    # Armed file should be cleared (single-shot).
    assert not armed.exists()

    # Mini-learning written.
    files = list(learnings_dir.glob("lrn-mini-*-sess-cor*.md"))
    assert len(files) == 1
    body = files[0].read_text()
    assert "confidence: low" in body
    assert "Bash" in body
    assert "--insecure" in body  # the user's correction text appears


def test_user_prompt_recall_clears_old_armed_state(tmp_path):
    """Armed state older than 10 min must be cleared even on a
    non-correction prompt — avoids stale arms firing later."""
    armed = tmp_path / "armed" / "sess-stale.json"
    armed.parent.mkdir(parents=True, exist_ok=True)
    armed.write_text(json.dumps({
        "tool": "Bash",
        "ts": 1.0,  # ancient
    }))

    payload = json.dumps({
        "session_id": "sess-stale",
        "prompt": "just keep going, no correction here",
    })
    _run(USER_PROMPT_HOOK, payload, tmp_path)
    assert not armed.exists()


def test_user_prompt_recall_keeps_recent_armed_state_on_non_correction(tmp_path):
    """Fresh armed state with a non-correction prompt should be PRESERVED
    so the NEXT prompt has a chance to trigger the mini-learning."""
    import time as _t
    armed = tmp_path / "armed" / "sess-keep.json"
    armed.parent.mkdir(parents=True, exist_ok=True)
    armed.write_text(json.dumps({
        "tool": "Bash",
        "ts": _t.time(),  # fresh
    }))

    payload = json.dumps({
        "session_id": "sess-keep",
        "prompt": "ok let me look at the logs",  # not a correction
    })
    _run(USER_PROMPT_HOOK, payload, tmp_path)
    assert armed.exists()  # preserved for next prompt


def test_user_prompt_recall_writes_dedupe_state_only_on_real_inject(tmp_path):
    """If no recall hits (no graphrag in test env), dedupe state must
    NOT be written — an empty session-injected file would prevent
    legitimate first-time injections after the index is populated."""
    payload = json.dumps({
        "session_id": "sess-nohits",
        "prompt": "longer prompt that meets the minimum char floor here",
    })
    _run(USER_PROMPT_HOOK, payload, tmp_path)
    # No graphrag → no hits → no state file written.
    assert not (tmp_path / "session-injected" / "sess-nohits.json").exists()
