#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
Stop Reflection Enqueue Hook.

Fires when the agent finishes a turn. Enqueues the transcript for
asynchronous reflection — same queue as ``precompact_reflect.py``,
but for short sessions that ended before PreCompact ever fired.

Dedupe vs PreCompact
--------------------

Long sessions hit PreCompact first (context fills up) → that hook
enqueues. Then Stop fires too. Without dedupe, we'd enqueue the same
session twice and the drainer would burn LLM calls processing it
twice. So we scan ``pending_reflections.jsonl`` first and skip if any
existing entry shares this ``session_id``.

Usage in hooks config:
{
  "hooks": {
    "Stop": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "uv run <HOME_TOOL_DIR>/hooks/stop_reflect.py"
      }]
    }]
  }
}

Output: ALWAYS empty stdout. Codex 0.131 has no ``StopHookSpecificOutputWire``
(same lesson as PreCompact — see plugins/reflect/.claude-plugin/plugin.json
PR #151). Empty stdout = success in both harnesses.

Exit behavior: always 0. Silent-fail wrapped.
"""

from __future__ import annotations

import json
import os
import sys
import time
import traceback
from datetime import datetime
from pathlib import Path


# Shared silent-fail mechanics.
_HOOK_NAME = "stop_reflect"
_PLUGIN_ROOT = Path(__file__).resolve().parents[1]  # hooks/<this> → plugins/reflect/
sys.path.insert(0, str(_PLUGIN_ROOT / "scripts"))
try:
    from silent_fail import write_last_event, forensics_log  # noqa: E402
except ImportError:
    def write_last_event(**kwargs):  # type: ignore[no-redef]
        pass
    def forensics_log(*args, **kwargs):  # type: ignore[no-redef]
        pass


def state_dir() -> Path:
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def queue_file() -> Path:
    return state_dir() / "pending_reflections.jsonl"


def session_already_queued(qfile: Path, session_id: str) -> bool:
    """Scan the JSONL queue for any existing entry with this session_id.

    Returns ``True`` if found, ``False`` otherwise (or on any read error
    — better to enqueue twice than to skip silently).
    """
    if not session_id or not qfile.exists():
        return False
    try:
        with open(qfile, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    e = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if e.get("session_id") == session_id:
                    return True
    except OSError:
        return False
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
    transcript_path = str(data.get("transcript_path", "") or "").strip()

    if not transcript_path:
        # Nothing useful to queue without a transcript path.
        return

    qf = queue_file()
    if session_already_queued(qf, session_id):
        forensics_log(_HOOK_NAME, f"skip (already queued by PreCompact): session={session_id[:8]}")
        return

    try:
        qf.parent.mkdir(parents=True, exist_ok=True)
        entry = {
            "ts": datetime.now().isoformat(),
            "session_id": session_id or "unknown",
            "transcript_path": transcript_path,
            "trigger": "stop",
            "cwd": data.get("cwd", os.getcwd()),
        }
        # Append, not atomic — JSONL queue is append-only and the
        # drainer tolerates partial lines.
        with open(qf, "a", encoding="utf-8") as f:
            f.write(json.dumps(entry) + "\n")
        forensics_log(_HOOK_NAME, f"enqueued: session={session_id[:8]}")
    except OSError:
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
    sys.exit(0)


if __name__ == "__main__":
    main()
