#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Notification Permission-Prompt Arming Hook (port SG8).

Fires on every Notification event. If the notification is a PERMISSION
PROMPT (Claude asking the user to approve a sensitive tool action), arm
a watcher at ``~/.reflect/permission-armed/<session_id>.json``. The next
UserPromptSubmit hook reads this file — if the user's typed reply looks
like a permission decision (``yes always``, ``no never``, ``only for
tests``, plain approve/deny), it writes a learning to disk with
``source: permission-pattern`` without spending an LLM call.

Permission moments are decision moments: the pattern of grants/denies is
durable project policy ("user always denies bash:rm -rf here"). This is
the same two-phase capture shape as the tool-failure arming pattern in
``posttooluse_minilearning.py``.

Detection: Claude Code does not always send an explicit
``notification_type`` field, so we accept BOTH:

  * ``notification_type`` / ``notificationType`` == ``permission_prompt``
    (agentmemory-style explicit typing), and
  * a ``message`` that matches the well-known permission phrasings
    ("Claude needs your permission to use Bash", "requesting permission
    to ...", "<tool> requires approval").

Usage in hooks config:
{
  "hooks": {
    "Notification": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "uv run <HOME_TOOL_DIR>/hooks/notification_reflect.py"
      }]
    }]
  }
}

Output: ALWAYS empty stdout — side-effect only (the armed file).
Exit behavior: always 0. Silent-fail wrapped.
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import traceback
from pathlib import Path


# Shared silent-fail mechanics.
_HOOK_NAME = "notification_reflect"
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


def permission_armed_path(session_id: str) -> Path:
    return state_dir() / "permission-armed" / f"{session_id}.json"


# --- Permission-prompt detection -----------------------------------------

# Message-shape fallback for harnesses that don't send notification_type.
# Matches Claude Code's known permission phrasings; intentionally narrow —
# a false positive only arms a watcher that the next prompt either matches
# or clears, so over-arming is cheap but we still avoid idle-timeout and
# "waiting for your input" notifications.
_PERMISSION_MESSAGE_PATTERNS = [
    re.compile(r"(?i)\bneeds?\s+your\s+permission\b"),
    re.compile(r"(?i)\bpermission\s+to\s+(?:use|run|execute)\b"),
    re.compile(r"(?i)\brequest(?:ing|s)?\s+permission\b"),
    re.compile(r"(?i)\b(?:requires?|awaiting)\s+(?:your\s+)?approval\b"),
]

# Extract the tool being approved from the message, e.g.
# "Claude needs your permission to use Bash" → "Bash".
_TOOL_FROM_MESSAGE = re.compile(
    r"(?i)\bpermission\s+to\s+(?:use|run|execute)\s+([A-Za-z0-9_.:\-]+)"
)


def is_permission_prompt(data: dict) -> bool:
    """True iff this Notification payload is a permission prompt."""
    ntype = data.get("notification_type", data.get("notificationType"))
    if isinstance(ntype, str):
        return ntype == "permission_prompt"
    message = str(data.get("message", "") or "")
    return any(p.search(message) for p in _PERMISSION_MESSAGE_PATTERNS)


def extract_tool(message: str) -> str:
    m = _TOOL_FROM_MESSAGE.search(message)
    return m.group(1) if m else "unknown"


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
    if not isinstance(data, dict):
        return

    session_id = str(data.get("session_id", "") or "").strip()
    if not session_id:
        return  # Nothing to arm — no session_id to key against.

    if not is_permission_prompt(data):
        return  # Idle / input-needed / other notifications don't arm.

    message = str(data.get("message", "") or "")
    title = str(data.get("title", "") or "")

    try:
        path = permission_armed_path(session_id)
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "tool": extract_tool(message),
            "message": scrub_secrets(message[:500]),
            "title": scrub_secrets(title[:200]),
            "cwd": str(data.get("cwd", "") or ""),
            "ts": time.time(),
        }
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(payload), encoding="utf-8")
        tmp.replace(path)
        forensics_log(
            _HOOK_NAME,
            f"permission-armed for session={session_id[:8]} tool={payload['tool']}",
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
