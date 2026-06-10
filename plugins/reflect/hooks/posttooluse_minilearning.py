#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
PostToolUse Mini-Learning Arming Hook.

Fires after every tool call. If the tool FAILED (non-zero exit, error
status, etc), arm a watcher at ``~/.reflect/armed/<session_id>.json``.
The next UserPromptSubmit hook reads this file — if the user's prompt
looks like a correction (``try X instead``, ``no, use Y``, …), it writes
a low-confidence mini-learning to disk without spending an LLM call.

Two-phase capture means we don't pay for /reflect on the dozens of
tool failures that DON'T have an obvious correction follow-up.

Usage in hooks config:
{
  "hooks": {
    "PostToolUse": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "uv run <HOME_TOOL_DIR>/hooks/posttooluse_minilearning.py"
      }]
    }]
  }
}

Output: ALWAYS empty stdout. Codex has ``PostToolUseHookSpecificOutputWire``
in its schema but our use case has no useful response to inject — we only
want the side-effect (armed file). Empty stdout = success in both
harnesses.

Exit behavior: always 0. Silent-fail wrapped.
"""

from __future__ import annotations

import json
import os
import sys
import time
import traceback
from pathlib import Path


# Shared silent-fail mechanics.
_HOOK_NAME = "posttooluse_minilearning"
_PLUGIN_ROOT = Path(__file__).resolve().parents[1]  # hooks/<this> → plugins/reflect/
sys.path.insert(0, str(_PLUGIN_ROOT / "scripts"))
try:
    from silent_fail import write_last_event, forensics_log, scrub_secrets  # noqa: E402
except ImportError:
    def write_last_event(**kwargs):  # type: ignore[no-redef]
        pass
    def forensics_log(*args, **kwargs):  # type: ignore[no-redef]
        pass
    def scrub_secrets(text):  # type: ignore[no-redef]
        return text


def state_dir() -> Path:
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def armed_path(session_id: str) -> Path:
    return state_dir() / "armed" / f"{session_id}.json"


def tool_failed(tool_response: dict, tool_name: str) -> bool:
    """Best-effort detector for tool failure.

    Both Claude and Codex send the tool_response on PostToolUse but the
    shape differs slightly. We look for the most common failure markers:

      * Non-zero ``exit_code`` / ``exitCode`` / ``returncode``
      * ``is_error`` / ``isError`` truthy
      * ``stderr`` non-empty (heuristic — many tools write warnings here
        too, so we ONLY use this as a tiebreaker when exit_code is absent)
      * ``error`` field present and truthy

    The detector is intentionally conservative — false positives arm
    a watcher that does nothing (the next prompt either looks like a
    correction or doesn't), so over-arming is cheap.
    """
    if not isinstance(tool_response, dict):
        return False

    for k in ("exit_code", "exitCode", "returncode"):
        v = tool_response.get(k)
        if isinstance(v, int) and v != 0:
            return True

    for k in ("is_error", "isError"):
        if tool_response.get(k):
            return True

    err = tool_response.get("error")
    if err and not (isinstance(err, str) and err.strip() == ""):
        return True

    # Bash-specific: if stdout is empty but stderr has content AND
    # there's no exit_code field, treat stderr as a failure signal.
    # Conservative: skip for read-only tools where stderr is just info.
    if tool_name and tool_name.lower() in ("bash", "shell", "execute"):
        if not tool_response.get("stdout") and tool_response.get("stderr"):
            return True

    return False


def _main_body() -> None:
    raw = ""
    try:
        raw = sys.stdin.read()
    except Exception:
        pass
    try:
        data = json.loads(raw) if raw else {}
    except json.JSONDecodeError:
        data = {}

    session_id = str(data.get("session_id", "") or "").strip()
    if not session_id:
        return  # Nothing to arm — no session_id to key against.

    tool_name = str(data.get("tool", "") or data.get("tool_name", "") or "")
    tool_input = data.get("tool_input", "")
    tool_response = data.get("tool_response", data.get("response", {}))

    # SG5: loop detection runs BEFORE the failure check — a loop of
    # *successful* identical calls is still a stall, and the user's next
    # prompt correcting it is the highest-signal learning in the session.
    loop_hit = None
    try:
        from loop_detector import record_call
        loop_hit = record_call(session_id, tool_name, tool_input)
    except ImportError:  # pragma: no cover
        pass

    # SG4: test-outcome parsing runs on Bash output BEFORE the failure-arm
    # path. A confirmed fix (failures N->0) writes a HIGH-confidence learning
    # directly inside observe_bash — nothing to arm. A regression (0->N) is a
    # contradiction signal: arm with reason="test-regression" so the next
    # corrective prompt is captured with test context attached.
    test_hit = None
    if tool_name and tool_name.lower() in ("bash", "shell", "execute"):
        try:
            from test_outcome_parser import observe_bash
            test_hit = observe_bash(session_id, tool_input, tool_response)
        except ImportError:  # pragma: no cover
            pass
    regression = bool(test_hit and test_hit.get("kind") == "regression")

    if not loop_hit and not regression and not tool_failed(tool_response, tool_name):
        return  # Successful, non-looping tool calls don't arm.

    # Write armed file. Truncate large payloads — only need enough for
    # the mini-learning context, not full transcripts.
    try:
        # M6: armed payloads later land in LLM-bound mini-learnings — strip
        # <private> spans before scrubbing/truncating. Best-effort import so a
        # missing filter can never break the hook.
        try:
            from privacy_filter import strip_private
        except ImportError:  # pragma: no cover
            def strip_private(text: str) -> str:  # type: ignore[no-redef]
                return text
        path = armed_path(session_id)
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "tool": tool_name,
            "tool_input": scrub_secrets(strip_private(str(tool_input))[:500]),
            "tool_response": scrub_secrets(strip_private(json.dumps(tool_response))[:500]),
            "ts": time.time(),
            # SG5/SG4: why we armed — lets the mini-learning tag its source
            # ('loop-correction' / 'test-regression' vs failure-correction).
            "reason": (
                "loop" if loop_hit
                else "test-regression" if regression
                else "failure"
            ),
        }
        if loop_hit:
            payload["loop"] = dict(loop_hit)
        if regression:
            payload["test_outcome"] = dict(test_hit)
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(payload), encoding="utf-8")
        tmp.replace(path)
        forensics_log(
            _HOOK_NAME,
            f"armed for session={session_id[:8]} tool={tool_name} "
            f"reason={payload['reason']}",
        )
    except Exception:
        # If even the armed write fails, we silently move on.
        pass


def main() -> None:
    try:
        _main_body()
    except SystemExit:
        raise
    except BaseException as exc:  # noqa: BLE001
        detail = str(exc) or traceback.format_exc(limit=2)
        write_last_event(
            hook_name=_HOOK_NAME,
            event="error",
            kind=type(exc).__name__,
            detail=detail,
        )
        forensics_log(_HOOK_NAME, f"{type(exc).__name__}: {detail}")
    # Always exit 0 with empty stdout regardless of success / failure.
    sys.exit(0)


if __name__ == "__main__":
    main()
