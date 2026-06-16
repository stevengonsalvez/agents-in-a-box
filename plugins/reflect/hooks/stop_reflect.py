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

Slot reflect (A1, gated by REFLECT_SLOTS)
-----------------------------------------

Before the enqueue decision, a deterministic (no-LLM) pass scans the
transcript and updates the memory slots: discovered TODOs append to
``pending_items``, tool-usage counts summarize into
``session_patterns``, and touched file paths accumulate in
``project_context``. Pure regex/JSON-walk — $0, and orthogonal to the
reflection enqueue (it runs even when the gate skips the transcript).

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
import re
import sys
import time
import traceback
from datetime import datetime, timezone
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

# Cross-harness stdin readers (snake_case claude/codex, camelCase copilot).
try:
    from hook_input import get_session_id, get_transcript_path, get_cwd  # noqa: E402
except ImportError:
    def get_session_id(data, default=""):  # type: ignore[no-redef]
        if not isinstance(data, dict):
            return default
        for k in ("session_id", "sessionId"):
            if k in data:
                return data[k]
        return default
    def get_transcript_path(data, default=""):  # type: ignore[no-redef]
        if not isinstance(data, dict):
            return default
        for k in ("transcript_path", "transcriptPath"):
            if k in data:
                return data[k]
        return default
    def get_cwd(data, default=""):  # type: ignore[no-redef]
        return data["cwd"] if isinstance(data, dict) and "cwd" in data else default


def state_dir() -> Path:
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def queue_file() -> Path:
    return state_dir() / "pending_reflections.jsonl"


def session_already_queued(qfile: Path, session_id: str) -> bool:
    """Scan the JSONL queue for any existing entry with this session_id.

    Returns ``True`` if found, ``False`` otherwise (or on any read error
    — better to enqueue twice than to skip silently).

    The scan is linear but bounded: ``reflect-drain-bg.sh`` caps the
    queue at ``REFLECT_DRAIN_DAILY_MAX`` (default 20) entries before
    flushing, so worst-case this reads 20 short JSONL lines — fine for
    a hook that runs once per agent finish.
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


# --- A1: deterministic slot reflect (no LLM) -------------------------------

# Caps keep the scan bounded: a Stop hook must stay instant even on a
# multi-MB transcript.
_SLOT_SCAN_MAX_RECORDS = 5000
_SLOT_MAX_PENDING = 20
_SLOT_MAX_FILES = 20
_SLOT_LINE_CAP = 160
_TODO_LINE_RE = re.compile(r"\bTODO\b", re.IGNORECASE)
_FILE_TOOLS = ("edit", "write", "multiedit", "notebookedit")
_PATH_KEYS = ("file_path", "notebook_path", "path")


def slots_enabled() -> bool:
    """A1 opt-in flag (mirrors agentmemory's SLOTS=on gate)."""
    return os.environ.get("REFLECT_SLOTS", "").strip().lower() in (
        "1", "true", "yes", "on",
    )


def scan_transcript_for_slots(transcript_path: str) -> dict:
    """Deterministic transcript scan: pending TODOs, tool counts, files.

    Pure JSON-walk + regex — no model call. Sources:

    * ``pending`` — TodoWrite tool_use inputs whose items are still
      pending/in_progress, plus dialogue lines mentioning TODO;
    * ``patterns`` — counts of Bash commands and errored tool_results;
    * ``files`` — paths named by file-editing tools (Edit/Write/...).

    Never raises; an unreadable transcript returns empty buckets.
    """
    pending: list[str] = []
    seen_pending: set[str] = set()
    files: list[str] = []
    seen_files: set[str] = set()
    counts = {"commands": 0, "errors": 0}

    def note_pending(text: str) -> None:
        line = "- " + " ".join(str(text).split())[:_SLOT_LINE_CAP]
        if line != "- " and line not in seen_pending and len(pending) < _SLOT_MAX_PENDING:
            seen_pending.add(line)
            pending.append(line)

    try:
        with open(transcript_path, encoding="utf-8", errors="replace") as fh:
            for i, raw in enumerate(fh):
                if i >= _SLOT_SCAN_MAX_RECORDS:
                    break
                raw = raw.strip()
                if not raw:
                    continue
                try:
                    rec = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                msg = rec.get("message")
                if not isinstance(msg, dict):
                    continue
                content = msg.get("content")
                if isinstance(content, str):
                    for line in content.splitlines():
                        if _TODO_LINE_RE.search(line):
                            note_pending(line.strip())
                    continue
                if not isinstance(content, list):
                    continue
                for block in content:
                    if not isinstance(block, dict):
                        continue
                    btype = block.get("type")
                    if btype == "text":
                        for line in str(block.get("text", "")).splitlines():
                            if _TODO_LINE_RE.search(line):
                                note_pending(line.strip())
                    elif btype == "tool_use":
                        name = str(block.get("name", ""))
                        low = name.lower()
                        tool_input = block.get("input")
                        tool_input = tool_input if isinstance(tool_input, dict) else {}
                        if low == "bash":
                            counts["commands"] += 1
                        elif low == "todowrite":
                            todos = tool_input.get("todos")
                            for item in todos if isinstance(todos, list) else []:
                                if not isinstance(item, dict):
                                    continue
                                if item.get("status") in ("pending", "in_progress"):
                                    note_pending(item.get("content", ""))
                        elif low in _FILE_TOOLS:
                            for key in _PATH_KEYS:
                                path = tool_input.get(key)
                                if (
                                    isinstance(path, str) and path
                                    and path not in seen_files
                                    and len(files) < _SLOT_MAX_FILES
                                ):
                                    seen_files.add(path)
                                    files.append(path)
                                    break
                    elif btype == "tool_result" and block.get("is_error"):
                        counts["errors"] += 1
    except OSError:
        pass
    return {"pending": pending, "counts": counts, "files": files}


def apply_slot_reflect(scan: dict, project_id: str) -> int:
    """Write the scan results into the slots. Returns slots updated.

    Deterministic writers only (reflect_db.slot_auto_*): dedupe + tail-
    truncate semantics, read-only slots skipped, size caps respected.
    """
    import reflect_db  # scripts/ already on sys.path

    conn = reflect_db.get_conn()
    reflect_db.ensure_default_slots(project_id, conn=conn)
    applied = 0

    if scan["pending"]:
        if reflect_db.slot_auto_append(
            "pending_items", scan["pending"], project_id=project_id, conn=conn,
        ):
            applied += 1

    counts = {k: v for k, v in scan["counts"].items() if v}
    if counts:
        summary_lines = [
            f"last slot-reflect: {datetime.now(timezone.utc).isoformat()}"
        ]
        summary_lines.extend(
            f"- {kind}: {count} in last session"
            for kind, count in sorted(counts.items())
        )
        if reflect_db.slot_auto_replace(
            "session_patterns", "\n".join(summary_lines),
            project_id=project_id, conn=conn,
        ):
            applied += 1

    if scan["files"]:
        slot = reflect_db.get_slot(
            "project_context", project_id=project_id, conn=conn,
        )
        existing = slot["content"] if slot else ""
        lines = []
        if not existing.strip():
            lines.append("Files touched in recent sessions:")
        lines.extend(f"- {f}" for f in scan["files"] if f not in existing)
        if reflect_db.slot_auto_append(
            "project_context", lines, project_id=project_id, conn=conn,
        ):
            applied += 1
    return applied


def _slot_reflect(transcript_path: str, cwd: str) -> None:
    """Best-effort A1 pass: never raises, never blocks the enqueue path."""
    try:
        import reflect_db

        scan = scan_transcript_for_slots(transcript_path)
        if not (scan["pending"] or scan["files"] or any(scan["counts"].values())):
            return
        project_id = reflect_db.derive_slot_project_id(Path(cwd))
        applied = apply_slot_reflect(scan, project_id)
        if applied:
            forensics_log(_HOOK_NAME, f"slot-reflect updated {applied} slot(s)")
    except Exception:  # noqa: BLE001
        pass


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

    session_id = str(get_session_id(data) or "").strip()
    transcript_path = str(get_transcript_path(data) or "").strip()

    # SG4: sweep TTL-expired test-outcome state. Stop is the closest thing to
    # a session-end hook the plugin wires; deleting the CURRENT session's
    # state here would break cross-turn fix tracking (Stop fires per turn),
    # so we only reap stale files — the live session's file is fresh and
    # survives the sweep. Best-effort: never blocks the enqueue path.
    try:
        from test_outcome_parser import cleanup_stale  # noqa: E402
        n = cleanup_stale()
        if n:
            forensics_log(_HOOK_NAME, f"test-state sweep removed {n} stale file(s)")
    except Exception:  # noqa: BLE001
        pass

    # SG7: same sweep for TodoWrite todo-completion state.
    try:
        from todo_state import cleanup_stale as todo_cleanup_stale  # noqa: E402
        n = todo_cleanup_stale()
        if n:
            forensics_log(_HOOK_NAME, f"todo-state sweep removed {n} stale file(s)")
    except Exception:  # noqa: BLE001
        pass

    # A1: deterministic slot reflect (flag-gated, $0, no LLM). Runs before
    # the enqueue gate because slot upkeep is orthogonal to reflection —
    # a "clean / no-signal" session can still leave TODOs behind.
    if transcript_path and slots_enabled():
        _slot_reflect(transcript_path, str(data.get("cwd", os.getcwd())))

    if not transcript_path:
        # Nothing useful to queue without a transcript path.
        return

    qf = queue_file()
    cf = state_dir() / "drain-cost.jsonl"

    # Session-id fast-path dedup (long sessions: PreCompact enqueued first).
    if session_already_queued(qf, session_id):
        forensics_log(_HOOK_NAME, f"skip (already queued by PreCompact): session={session_id[:8]}")
        return

    # Enqueue gate + transcript-path dedup ($0 regex). Skip reflect-on-reflect
    # / clean / no-signal transcripts and anything already queued/processed.
    # Fail-open: a gate error must never block a genuine enqueue.
    try:
        from reflect_gate import should_enqueue  # noqa: E402
        ok, reason = should_enqueue(transcript_path, qf, cf)
        if not ok:
            forensics_log(_HOOK_NAME, f"gate skip ({reason}): session={session_id[:8]}")
            return
    except Exception:  # noqa: BLE001
        pass

    try:
        qf.parent.mkdir(parents=True, exist_ok=True)
        entry = {
            "ts": datetime.now().isoformat(),
            "session_id": session_id or "unknown",
            "transcript_path": transcript_path,
            "trigger": "stop",
            "cwd": get_cwd(data, os.getcwd()),
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
