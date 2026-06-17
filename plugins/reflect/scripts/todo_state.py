#!/usr/bin/env python3
# ABOUTME: TodoWrite completion capture (port SG7, agentmemory post-tool-use todo-diff pattern).
# ABOUTME: Per-session todo-state diff: completed transitions become MEDIUM-confidence Process learnings.
"""TodoWrite completion as capture signal.

Port SG7. Todo completions ARE the structured moments in a session — the
user (or agent) explicitly marked something done, bundling intent +
execution + outcome for free. This module gives the PostToolUse hook two
entry points:

* :func:`record_file_event` — on file-touching tools (Edit / Write /
  MultiEdit / NotebookEdit), append the touched path to the session's
  file-event log so completions can attribute "files touched during the
  todo". Cheap no-op until the session has used TodoWrite at least once.
* :func:`observe_todowrite` — on TodoWrite, diff the prior recorded todo
  state against the new list. Items transitioning to ``status='completed'``
  produce a candidate learning ("how I accomplished X") carrying the
  item's content, the prior in_progress duration, and the files touched
  since the item went in_progress. Tagged ``category: Process`` /
  ``confidence: medium`` / ``source: todo-completion``.

State lives at ``$REFLECT_STATE_DIR/todo-state/<session_id>.json`` (same
layout as the test-state/ and loops/ dirs):

    {"updated": ts,
     "todos": {"<content>": {"status": ..., "first_seen": ts,
                              "in_progress_since": ts|None}},
     "files": [{"path": ..., "ts": ...}]}

Everything here is stdlib-only and silent-fail shaped: any error returns
``None`` / no-ops rather than raising into the hook. State files expire
after ``_STATE_TTL_S`` and are swept by :func:`cleanup_stale` (wired into
the Stop hook, same as the SG4 test-state sweep).
"""
from __future__ import annotations

import json
import os
import time
from pathlib import Path
from typing import Optional

__all__ = [
    "FILE_TOOLS",
    "record_file_event",
    "observe_todowrite",
    "write_todo_learning",
    "cleanup_session",
    "cleanup_stale",
]

_STATE_TTL_S = 6 * 3600   # stale session state expires after 6h (matches loop/test state)
_FILES_MAX = 200          # bounded per-session file-event log
_TODOS_MAX = 100          # bounded tracked-todo map
_CONTENT_KEY_CAP = 300    # todo content is the diff key — cap for sane state files
_CONTENT_BODY_CAP = 500   # cap content rendered into the learning body

# Tools whose tool_input names a file being modified. Lowercased for the
# case-insensitive match the hook already uses for Bash.
FILE_TOOLS = ("edit", "write", "multiedit", "notebookedit")
_PATH_KEYS = ("file_path", "notebook_path", "path")

# Best-effort imports of shared scrub/strip helpers (same dir). A missing
# helper must never break capture.
try:
    from silent_fail import scrub_secrets
except ImportError:  # pragma: no cover
    def scrub_secrets(text: str) -> str:  # type: ignore[no-redef]
        return text
try:
    from privacy_filter import strip_private
except ImportError:  # pragma: no cover
    def strip_private(text: str) -> str:  # type: ignore[no-redef]
        return text


# --- Per-session state -------------------------------------------------------

def _state_dir() -> Path:
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    return base / "todo-state"


def _state_path(session_id: str) -> Path:
    safe = "".join(c if c.isalnum() or c in "-_" else "_" for c in session_id)[:64]
    return _state_dir() / f"{safe}.json"


def _load(path: Path) -> dict:
    try:
        data = json.loads(path.read_text())
        if not isinstance(data, dict):
            return {}
        if time.time() - float(data.get("updated", 0)) > _STATE_TTL_S:
            return {}
        todos = data.get("todos", {})
        files = data.get("files", [])
        return {
            "todos": todos if isinstance(todos, dict) else {},
            "files": files if isinstance(files, list) else [],
        }
    except (OSError, json.JSONDecodeError, ValueError):
        return {}


def _save(path: Path, state: dict) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps({
            "updated": time.time(),
            "todos": dict(list(state.get("todos", {}).items())[-_TODOS_MAX:]),
            "files": list(state.get("files", []))[-_FILES_MAX:],
        }))
        tmp.replace(path)
    except OSError:
        pass


# --- File-event log ----------------------------------------------------------

def record_file_event(session_id: str, tool_name: str, tool_input) -> None:
    """Append a file-touch event to the session's todo state.

    Only fires for file-modifying tools, and only once the session already
    HAS todo state (i.e. TodoWrite ran at least once) — sessions that never
    use todos never pay for a state file. Never raises.
    """
    if not session_id or not tool_name:
        return
    try:
        if tool_name.lower() not in FILE_TOOLS:
            return
        if not isinstance(tool_input, dict):
            return
        file_path = ""
        for k in _PATH_KEYS:
            v = tool_input.get(k)
            if isinstance(v, str) and v.strip():
                file_path = v.strip()
                break
        if not file_path:
            return
        path = _state_path(session_id)
        if not path.exists():
            return  # no todo state yet — nothing to attribute files to
        state = _load(path)
        if not state:
            return  # expired / corrupt — don't resurrect
        state["files"] = list(state.get("files", []))
        state["files"].append({"path": file_path, "ts": time.time()})
        _save(path, state)
    except Exception:  # noqa: BLE001 — hook-adjacent: never raise
        return


# --- TodoWrite diff -----------------------------------------------------------

def _parse_todos(tool_input) -> Optional[list[dict]]:
    """Extract the TodoWrite items list; ``None`` when the shape is wrong."""
    if not isinstance(tool_input, dict):
        return None
    todos = tool_input.get("todos")
    if not isinstance(todos, list):
        return None
    out = []
    for item in todos:
        if not isinstance(item, dict):
            continue
        content = str(item.get("content", "") or "").strip()
        status = str(item.get("status", "") or "").strip().lower()
        if content and status:
            out.append({"content": content, "status": status})
    return out


def observe_todowrite(session_id: str, tool_input) -> Optional[dict]:
    """Diff one TodoWrite call against prior session state; capture completions.

    Returns ``None`` when nothing completed (state is still updated), else::

        {"completed": [{"content": ..., "duration_s": <float|None>,
                        "files": [...], "learning": <slug|None>}, ...]}

    Completion detection requires a PRIOR recorded non-completed status for
    the item — an unseen item arriving already-completed has no baseline to
    diff against and is skipped (also makes repeated completed lists
    idempotent: after the first transition the stored status is
    ``completed`` and later TodoWrites no-op). Silent-fail: never raises.
    """
    if not session_id:
        return None
    try:
        new_todos = _parse_todos(tool_input)
        if new_todos is None:
            return None
        path = _state_path(session_id)
        state = _load(path)
        prior = state.get("todos", {}) if state else {}
        files = state.get("files", []) if state else []
        now = time.time()

        completed: list[dict] = []
        next_todos: dict[str, dict] = {}
        for item in new_todos:
            key = item["content"][:_CONTENT_KEY_CAP]
            old = prior.get(key) if isinstance(prior.get(key), dict) else None
            entry = {
                "status": item["status"],
                "first_seen": float(old.get("first_seen", now)) if old else now,
                "in_progress_since": old.get("in_progress_since") if old else None,
            }
            if item["status"] == "in_progress" and (
                old is None or old.get("status") != "in_progress"
            ):
                entry["in_progress_since"] = now
            if (
                item["status"] == "completed"
                and old is not None
                and old.get("status") != "completed"
            ):
                # Window starts when the item went in_progress; fall back to
                # when it was first seen (some flows skip in_progress).
                since = entry["in_progress_since"] or entry["first_seen"]
                duration_s: Optional[float] = None
                try:
                    if entry["in_progress_since"]:
                        duration_s = max(0.0, now - float(entry["in_progress_since"]))
                except (TypeError, ValueError):
                    duration_s = None
                touched: list[str] = []
                for ev in files:
                    try:
                        if float(ev.get("ts", 0)) >= float(since) and ev.get("path"):
                            p = str(ev["path"])
                            if p not in touched:
                                touched.append(p)
                    except (TypeError, ValueError, AttributeError):
                        continue
                completed.append({
                    "content": item["content"],
                    "duration_s": duration_s,
                    "files": touched,
                    "learning": write_todo_learning(
                        session_id, item["content"], duration_s, touched
                    ),
                })
            next_todos[key] = entry

        state = {"todos": next_todos, "files": files}
        _save(path, state)
        return {"completed": completed} if completed else None
    except Exception:  # noqa: BLE001 — hook-adjacent: never raise
        return None


# --- Learning emission ---------------------------------------------------------

def _learnings_dir() -> Path:
    """Where learnings get written. Honors REFLECT_LEARNINGS_DIR override;
    defaults to ~/.learnings/documents/ (same as the recall hooks)."""
    custom = os.environ.get("REFLECT_LEARNINGS_DIR")
    if custom:
        return Path(custom).expanduser()
    return Path.home() / ".learnings" / "documents"


def write_todo_learning(
    session_id: str,
    content: str,
    duration_s: Optional[float],
    files: list[str],
) -> Optional[str]:
    """Write the todo-completion candidate learning; return its slug.

    Confidence is ``medium``: the completion is user/agent-marked (explicit
    "done"), but the *how* — files + duration — is inferred from tool
    events, not verified the way a test-runner fix is. Category is
    ``Process`` ("how I accomplished X"). Never raises.
    """
    try:
        ld = _learnings_dir()
        ld.mkdir(parents=True, exist_ok=True)
        ts_ms = int(time.time() * 1000)
        slug = f"lrn-todo-done-{ts_ms}-{session_id[:8]}"
        path = ld / f"{slug}.md"
        n = 2
        while path.exists():
            path = ld / f"{slug}-{n}.md"
            n += 1
        clean = scrub_secrets(strip_private(str(content))[:_CONTENT_BODY_CAP])
        title = clean.splitlines()[0][:120] if clean else "(untitled)"
        duration_line = (
            f"duration_s: {int(duration_s)}\n" if duration_s is not None else ""
        )
        duration_text = (
            f"~{int(duration_s)}s in progress" if duration_s is not None
            else "untracked (item never marked in_progress)"
        )
        if files:
            files_text = "\n".join(
                f"- `{scrub_secrets(str(f))[:300]}`" for f in files[:30]
            )
        else:
            files_text = "_(no file events recorded during this todo)_"
        body = (
            f"---\n"
            f"id: {path.stem}\n"
            f"confidence: medium\n"
            f"category: Process\n"
            f"source: todo-completion\n"
            f"session_id: {session_id}\n"
            f"{duration_line}"
            f"captured_at: {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n"
            f"---\n\n"
            f"# Todo completed: {title}\n\n"
            f"**Task**: {clean}\n\n"
            f"**Duration**: {duration_text}\n\n"
            f"**Files touched while in progress**:\n{files_text}\n\n"
            f"_Auto-captured by the PostToolUse TodoWrite watcher. Confidence "
            f"is `medium` because the completion was explicitly marked but the "
            f"execution detail is inferred from file events._\n"
        )
        path.write_text(body, encoding="utf-8")
        return path.stem
    except Exception:  # noqa: BLE001
        return None


# --- Cleanup -------------------------------------------------------------------

def cleanup_session(session_id: str) -> None:
    """Remove one session's todo state (and its tmp sibling). Never raises."""
    if not session_id:
        return
    try:
        path = _state_path(session_id)
        for p in (path, path.with_suffix(".json.tmp")):
            try:
                p.unlink()
            except FileNotFoundError:
                pass
    except OSError:
        pass


def cleanup_stale(max_age_s: float = _STATE_TTL_S) -> int:
    """Sweep TTL-expired todo-state files; return how many were removed.

    Wired into the Stop hook (the plugin has no SessionEnd event): the live
    session's state file is fresh and survives; only abandoned sessions'
    files get reaped. Never raises.
    """
    removed = 0
    try:
        d = _state_dir()
        if not d.is_dir():
            return 0
        now = time.time()
        for p in list(d.glob("*.json")) + list(d.glob("*.json.tmp")):
            try:
                if now - p.stat().st_mtime > max_age_s:
                    p.unlink()
                    removed += 1
            except OSError:
                continue
    except Exception:  # noqa: BLE001
        return removed
    return removed
