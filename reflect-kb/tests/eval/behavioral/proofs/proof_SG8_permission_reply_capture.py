# ABOUTME: Behavioral proof for SG8 — permission prompt + reply capture as a SIGNAL. Drives the
# ABOUTME: two REAL hooks end-to-end as subprocesses (no recall, no torch, no LLM): a permission_prompt
# ABOUTME: Notification arms ~/.reflect/permission-armed/<sid>.json (Phase 1), then the next
# ABOUTME: UserPromptSubmit classifies the user's allow/deny reply and writes a source:permission-pattern
# ABOUTME: learning (Phase 2). A non-permission notification arms NOTHING -> the identical reply captures NOTHING.
"""SG8: permission prompt + reply capture signal.

Port SG8 is a SIGNAL/CAPTURE port (commit 2942ccf6,
``feat(reflect): capture permission prompt replies as policy learnings``).
Its behaviour lives entirely in the hook path — NOT in recall.py — so there is
no retrieval and no embedding model. This proof therefore drives the TWO REAL
hooks the port ships, exactly as Claude Code invokes them (JSON on stdin, env
isolation via REFLECT_STATE_DIR / REFLECT_LEARNINGS_DIR):

  Phase 1  plugins/reflect/hooks/notification_reflect.py            (Notification)
  Phase 2  plugins/reflect/skills/recall/hooks/user_prompt_submit_recall.py
                                                                    (UserPromptSubmit)

TRUE invariant (read off the real diff):

  A permission moment is captured in TWO phases. (1) When a Notification is a
  PERMISSION PROMPT (explicit ``notification_type == permission_prompt`` OR a
  message matching the known phrasings, e.g. "Claude needs your permission to
  use Bash"), the Notification hook writes an *armed* file keyed by session_id.
  (2) On the NEXT UserPromptSubmit, if the typed prompt reads like a permission
  decision, the recall hook classifies it and writes a learning with
  ``source: permission-pattern`` whose frontmatter ``confidence`` and body
  ``Decision`` encode WHICH decision:

      no never / always deny ........ deny-always   (HIGH confidence)
      yes always / always allow ..... allow-always  (HIGH confidence)
      only for X .................... allow-scoped   (HIGH confidence)
      plain yes/approve/allow ....... allow-once     (MEDIUM confidence)
      plain no/deny/reject .......... deny-once      (MEDIUM confidence)

  The armed file is the load-bearing knob. A Notification that is NOT a
  permission prompt arms NOTHING, so the very same approve-shaped reply on the
  next prompt captures NOTHING — emission is caused by the real two-phase diff,
  not by the text of the reply.

DECISIVE arms (each seeds a FRESH hermetic state+learnings dir; no shared state):

  ARM 1 (knob ON, allow + deny, end-to-end through both real hooks):
    Notification(permission_prompt, tool=Bash) -> armed file exists ->
    UserPromptSubmit("yes, go ahead") -> exactly ONE learning, decision
    ``allow-once``, source ``permission-pattern``, MEDIUM, names tool Bash, and
    the armed file is consumed (single-shot). Repeat with a fresh dir and a
    deny reply -> decision ``deny-once``.

  ARM 2 (HIGH vs MEDIUM, decisive confidence toggle on reply shape):
    With the watcher armed, a DURABLE reply ("no, never run that here") yields
    ``deny-always`` at HIGH confidence; a plain reply ("no") yields ``deny-once``
    at MEDIUM. Same armed file, the reply WORDS flip confidence — the port's own
    classifier, not luck.

  ARM 3 (knob OFF, control — falsifiable half):
    A NON-permission Notification (an idle "waiting for your input" message) arms
    NOTHING; the IDENTICAL approve reply on the next prompt writes ZERO learning
    files. If SG8's detection were broken (arming on any notification), this arm
    would also emit and the test would FAIL.

Why no LLM: the notification payloads, the typed replies, and the env dirs are
all fixed literals. The Notification hook's detection regex, the recall hook's
``classify_permission_reply`` regex, and the on-disk armed/learning files fully
determine every assertion. No model is loaded and nothing an LLM decided is
asserted.

PORT: SG8
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

# Resolve the two REAL hooks the same way SG7's proof resolves todo_state, so
# this runs from either checkout layout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect",
]
_PLUGIN_ROOT = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])

NOTIFICATION_HOOK = _PLUGIN_ROOT / "hooks" / "notification_reflect.py"
USER_PROMPT_HOOK = (
    _PLUGIN_ROOT / "skills" / "recall" / "hooks" / "user_prompt_submit_recall.py"
)


def _run_hook(hook: Path, payload: dict, state_dir: Path, learnings_dir: Path):
    """Invoke a real hook as Claude Code does: JSON on stdin, hermetic env. The
    hooks honor REFLECT_STATE_DIR (armed files) and REFLECT_LEARNINGS_DIR
    (captured learnings), so this never touches the developer's ~/.reflect."""
    env = {
        **os.environ,
        "REFLECT_STATE_DIR": str(state_dir),
        "REFLECT_LEARNINGS_DIR": str(learnings_dir),
    }
    return subprocess.run(
        [sys.executable, str(hook)],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )


def _armed_path(state_dir: Path, sid: str) -> Path:
    return state_dir / "permission-armed" / f"{sid}.json"


def _perm_learnings(learnings_dir: Path) -> list[Path]:
    return sorted(learnings_dir.glob("lrn-perm-*.md")) if learnings_dir.is_dir() else []


def _hooks_present() -> None:
    assert NOTIFICATION_HOOK.exists(), f"missing Notification hook: {NOTIFICATION_HOOK}"
    assert USER_PROMPT_HOOK.exists(), f"missing UserPromptSubmit hook: {USER_PROMPT_HOOK}"


def _arm_via_notification(state_dir: Path, learnings_dir: Path, sid: str, *, tool: str = "Bash"):
    """Phase 1: drive the REAL Notification hook with a permission_prompt and
    return its result. Asserts side-effect-only (empty stdout, exit 0)."""
    payload = {
        "session_id": sid,
        "notification_type": "permission_prompt",
        "message": f"Claude needs your permission to use {tool}",
        "title": "Permission required",
        "cwd": "/tmp/project",
    }
    res = _run_hook(NOTIFICATION_HOOK, payload, state_dir, learnings_dir)
    assert res.returncode == 0, f"notification hook nonzero: {res.stderr[-400:]}"
    assert res.stdout == "", f"notification hook must be side-effect only; got {res.stdout!r}"
    return res


def _reply_via_user_prompt(state_dir: Path, learnings_dir: Path, sid: str, prompt: str):
    """Phase 2: drive the REAL UserPromptSubmit hook with a typed reply."""
    payload = {"session_id": sid, "prompt": prompt, "cwd": "/tmp/project"}
    res = _run_hook(USER_PROMPT_HOOK, payload, state_dir, learnings_dir)
    assert res.returncode == 0, f"user_prompt hook nonzero: {res.stderr[-400:]}"
    return res


# --------------------------------------------------------------------------
# ARM 1: knob ON — full two-phase capture of an allow reply and a deny reply.
# --------------------------------------------------------------------------

def test_SG8_permission_allow_reply_captured_end_to_end(tmp_path):
    """Notification(permission_prompt) arms; the next 'yes, go ahead' reply is
    captured as an ``allow-once`` / MEDIUM permission-pattern learning, and the
    armed file is consumed (single-shot)."""
    _hooks_present()
    state_dir = tmp_path / "state"
    learnings_dir = tmp_path / "learnings"
    sid = "sess-sg8-allow"

    # Phase 1: the real Notification hook arms the watcher.
    _arm_via_notification(state_dir, learnings_dir, sid, tool="Bash")
    armed = _armed_path(state_dir, sid)
    assert armed.exists(), "permission_prompt notification must arm the watcher file"
    armed_data = json.loads(armed.read_text(encoding="utf-8"))
    assert armed_data["tool"] == "Bash"

    # Phase 2: the real UserPromptSubmit hook classifies the reply and writes it.
    _reply_via_user_prompt(state_dir, learnings_dir, sid, "yes, go ahead")

    files = _perm_learnings(learnings_dir)
    assert len(files) == 1, f"expected exactly one permission learning, got {[f.name for f in files]}"
    text = files[0].read_text(encoding="utf-8")
    assert "source: permission-pattern" in text, "capture must carry source: permission-pattern"
    assert "confidence: medium" in text, "plain approve is MEDIUM confidence"
    assert "`allow-once`" in text, "plain approve must classify as allow-once"
    assert "Bash" in text, "the armed tool must be attributed in the learning"

    # Single-shot: the armed file is consumed once the reply is captured.
    assert not armed.exists(), "armed file must be cleared after a capture (single-shot)"


def test_SG8_permission_deny_reply_captured_end_to_end(tmp_path):
    """Fresh dir: the same arming followed by a plain 'no' reply is captured as a
    ``deny-once`` / MEDIUM learning — the decision tracks the reply, not a constant."""
    _hooks_present()
    state_dir = tmp_path / "state"
    learnings_dir = tmp_path / "learnings"
    sid = "sess-sg8-deny"

    _arm_via_notification(state_dir, learnings_dir, sid, tool="Bash")
    assert _armed_path(state_dir, sid).exists()

    _reply_via_user_prompt(state_dir, learnings_dir, sid, "no")

    files = _perm_learnings(learnings_dir)
    assert len(files) == 1, f"expected exactly one permission learning, got {[f.name for f in files]}"
    text = files[0].read_text(encoding="utf-8")
    assert "source: permission-pattern" in text
    assert "confidence: medium" in text
    assert "`deny-once`" in text, "plain refusal must classify as deny-once"


# --------------------------------------------------------------------------
# ARM 2: HIGH vs MEDIUM — durable policy replies are HIGH confidence.
# --------------------------------------------------------------------------

def test_SG8_durable_reply_is_high_confidence(tmp_path):
    """A durable-policy reply ('no, never run that here') is captured as
    ``deny-always`` at HIGH confidence — it states project policy."""
    _hooks_present()
    state_dir = tmp_path / "state"
    learnings_dir = tmp_path / "learnings"
    sid = "sess-sg8-durable"

    _arm_via_notification(state_dir, learnings_dir, sid, tool="Bash")
    _reply_via_user_prompt(state_dir, learnings_dir, sid, "no, never run that here")

    files = _perm_learnings(learnings_dir)
    assert len(files) == 1, f"expected one learning, got {[f.name for f in files]}"
    text = files[0].read_text(encoding="utf-8")
    assert "source: permission-pattern" in text
    assert "confidence: high" in text, "durable 'never' reply must be HIGH confidence"
    assert "`deny-always`" in text, "'no, never ...' must classify as deny-always, not deny-once"


def test_SG8_plain_reply_is_medium_confidence(tmp_path):
    """Control twin for ARM 2: a PLAIN refusal ('no') under the identical arming
    is MEDIUM / deny-once — so it is the REPLY WORDS that flip confidence, proving
    the classifier (not the armed file) owns the high/medium distinction."""
    _hooks_present()
    state_dir = tmp_path / "state"
    learnings_dir = tmp_path / "learnings"
    sid = "sess-sg8-plain"

    _arm_via_notification(state_dir, learnings_dir, sid, tool="Bash")
    _reply_via_user_prompt(state_dir, learnings_dir, sid, "no")

    files = _perm_learnings(learnings_dir)
    assert len(files) == 1, f"expected one learning, got {[f.name for f in files]}"
    text = files[0].read_text(encoding="utf-8")
    assert "confidence: medium" in text, "a plain 'no' must be MEDIUM, not HIGH"
    assert "`deny-once`" in text


# --------------------------------------------------------------------------
# ARM 3: knob OFF — a non-permission notification arms nothing -> captures nothing.
# --------------------------------------------------------------------------

def test_SG8_non_permission_notification_arms_nothing_captures_nothing(tmp_path):
    """CONTROL (falsifiable half): an idle 'waiting for your input' Notification is
    NOT a permission prompt, so the real hook arms NOTHING. The IDENTICAL approve
    reply on the next prompt therefore captures NOTHING. This rules out the trivial
    'arm on any notification' failure mode: same reply text as ARM 1, only the
    notification shape changed — and emission flips off."""
    _hooks_present()
    state_dir = tmp_path / "state"
    learnings_dir = tmp_path / "learnings"
    sid = "sess-sg8-control"

    # A non-permission notification: must NOT match notification_type or the
    # permission message phrasings.
    payload = {
        "session_id": sid,
        "notification_type": "idle",
        "message": "Claude is waiting for your input",
        "title": "Waiting",
        "cwd": "/tmp/project",
    }
    res = _run_hook(NOTIFICATION_HOOK, payload, state_dir, learnings_dir)
    assert res.returncode == 0 and res.stdout == ""
    assert not _armed_path(state_dir, sid).exists(), (
        "a non-permission notification must NOT arm the permission watcher"
    )

    # The SAME approve reply ARM 1 used — but with no armed file it captures nothing.
    _reply_via_user_prompt(state_dir, learnings_dir, sid, "yes, go ahead")
    assert _perm_learnings(learnings_dir) == [], (
        "no permission learning may be written without a prior permission_prompt arming"
    )
