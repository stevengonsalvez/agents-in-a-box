# ABOUTME: The bridge's transport to ainb sessions.
#
# Three responsibilities, all reusing ainb's EXISTING, verified mechanisms:
#   1. discover()        — `ainb list --format json` -> running TargetSessions.
#   2. send(session, t)  — tmux send-keys (`-l` literal + Enter), matching
#                          ainb-fleet's broadcast transport (broker has a known
#                          silent-delivery gap, so tmux is the reliable path).
#   3. capture_reply()   — read the session's JSONL transcript: snapshot the
#                          byte offset, send, then wait for the next assistant
#                          row with stop_reason == "end_turn" and return its
#                          concatenated text blocks.
#
# RESPONSE-CAPTURE CONTRACT (decision, documented in README):
#   ainb has no `session send --wait` primitive like agent-deck. The reliable
#   equivalent is the JSONL transcript that ainb-fleet's `sequence` verb already
#   watches (`wait_for_turn_end`). We mirror that contract in Python: the reply
#   is the text of the last assistant turn whose stop_reason is "end_turn",
#   captured AFTER the send offset. This is the semantic match for agent-deck's
#   TEMPLATE contract (`session output --json -> content`), adapted to ainb.

from __future__ import annotations

import json
import os
import re
import subprocess
import time
from pathlib import Path

from .routing import TargetSession, is_conductor_name


def _ainb_bin() -> str:
    return os.environ.get("AINB_BIN", "ainb")


def cwd_to_project_slug(cwd: str) -> str:
    """Replicate ainb's cwd -> project-dir slug.

    Every char that is not ``[A-Za-z0-9-]`` collapses to ``-`` (matches the Rust
    `cwd_to_project_slug` in `fleet/read/jsonl_tail.rs`).
    """
    return "".join(c if (c.isalnum() and c.isascii()) or c == "-" else "-" for c in cwd)


def transcript_dir_for_cwd(cwd: str) -> Path:
    return Path.home() / ".claude" / "projects" / cwd_to_project_slug(cwd)


def latest_transcript_for_cwd(cwd: str) -> Path | None:
    """Newest ``*.jsonl`` under the cwd's Claude project dir, or ``None``."""
    base = transcript_dir_for_cwd(cwd)
    if not base.is_dir():
        return None
    newest: tuple[float, Path] | None = None
    for p in base.glob("*.jsonl"):
        try:
            mtime = p.stat().st_mtime
        except OSError:
            continue
        if newest is None or mtime > newest[0]:
            newest = (mtime, p)
    return newest[1] if newest else None


def discover() -> list[TargetSession]:
    """Discover running sessions via `ainb list --format json`."""
    try:
        out = subprocess.run(
            [_ainb_bin(), "list", "--format", "json"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return []
    if out.returncode != 0 or not out.stdout.strip():
        return []
    try:
        rows = json.loads(out.stdout)
    except json.JSONDecodeError:
        return []
    if not isinstance(rows, list):
        return []

    sessions: list[TargetSession] = []
    for r in rows:
        if not isinstance(r, dict) or not r.get("is_running", False):
            continue
        name = str(r.get("workspace_name", "")).strip()
        tmux = str(r.get("tmux_session_name", "")).strip()
        cwd = str(r.get("worktree_path", "")).strip()
        sid = str(r.get("session_id", "")).strip()
        if not name or not tmux:
            continue
        sessions.append(
            TargetSession(
                name=name,
                tmux_session=tmux,
                cwd=cwd,
                session_id=sid,
                is_conductor=is_conductor_name(name),
            )
        )
    return sessions


def tmux_session_exists(tmux_session: str) -> bool:
    try:
        out = subprocess.run(
            ["tmux", "has-session", "-t", tmux_session],
            capture_output=True,
            timeout=10,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return out.returncode == 0


def send_keys(tmux_session: str, text: str) -> bool:
    """Send ``text`` to a tmux session via literal send-keys + Enter.

    Uses ``-l`` (literal) so the payload is never interpreted as key names —
    the same injection-safe transport ainb-fleet's broadcast/sequence use.
    """
    try:
        lit = subprocess.run(
            ["tmux", "send-keys", "-t", tmux_session, "-l", text],
            capture_output=True,
            timeout=15,
            check=False,
        )
        if lit.returncode != 0:
            return False
        enter = subprocess.run(
            ["tmux", "send-keys", "-t", tmux_session, "Enter"],
            capture_output=True,
            timeout=15,
            check=False,
        )
        return enter.returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


# --- JSONL reply extraction -------------------------------------------------

_WS_RE = re.compile(r"\s+")


def _assistant_text_from_row(obj: dict) -> tuple[str | None, str | None]:
    """Return ``(stop_reason, text)`` for an assistant row, else ``(None, None)``.

    Concatenates every ``{"type":"text"}`` block in ``message.content``. Matches
    the extraction the Rust ``last_narrative_snapshot`` / ``last_assistant_info``
    perform.
    """
    if obj.get("type") != "assistant":
        return None, None
    message = obj.get("message")
    if not isinstance(message, dict):
        return None, None
    stop_reason = message.get("stop_reason")
    content = message.get("content")
    parts: list[str] = []
    if isinstance(content, list):
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text":
                t = block.get("text")
                if isinstance(t, str):
                    parts.append(t)
    elif isinstance(content, str):
        parts.append(content)
    text = "\n".join(p for p in parts if p).strip()
    return (stop_reason if isinstance(stop_reason, str) else None), (text or None)


def _scan_new_rows_for_turn_end(path: Path, start_offset: int) -> tuple[int, str | None]:
    """Scan rows from ``start_offset``; return ``(new_offset, reply_text|None)``.

    ``reply_text`` is set only when an assistant row with
    ``stop_reason == "end_turn"`` is found in the new region. The last such
    turn wins (the most recent complete answer).
    """
    try:
        with path.open("rb") as fh:
            fh.seek(start_offset)
            data = fh.read()
            new_offset = fh.tell()
    except OSError:
        return start_offset, None

    reply: str | None = None
    for line in data.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except (json.JSONDecodeError, UnicodeDecodeError):
            continue
        if not isinstance(obj, dict):
            continue
        stop_reason, text = _assistant_text_from_row(obj)
        if stop_reason == "end_turn" and text:
            reply = text
    return new_offset, reply


def current_offset(path: Path | None) -> int:
    """Byte length of ``path`` right now (the send watermark)."""
    if path is None:
        return 0
    try:
        return path.stat().st_size
    except OSError:
        return 0


def wait_for_reply(
    cwd: str,
    start_offset: int,
    transcript: Path | None,
    timeout: float,
    poll_interval: float = 0.5,
) -> str | None:
    """Poll the transcript for the next end-of-turn assistant reply.

    ``transcript`` may be ``None`` at send time (no transcript yet); we re-resolve
    it on each poll so a freshly-created file is picked up. Returns the reply
    text or ``None`` on timeout.
    """
    deadline = time.monotonic() + timeout
    offset = start_offset
    path = transcript
    while time.monotonic() < deadline:
        if path is None:
            path = latest_transcript_for_cwd(cwd)
            if path is not None:
                offset = 0  # brand-new transcript — read from the top
        if path is not None:
            offset, reply = _scan_new_rows_for_turn_end(path, offset)
            if reply is not None:
                return reply
        time.sleep(poll_interval)
    return None


def send_and_capture(session: TargetSession, text: str, timeout: float) -> str | None:
    """Send ``text`` to ``session`` and capture its next end-of-turn reply.

    Returns ``None`` if the send failed or no reply arrived before ``timeout``.
    """
    transcript = latest_transcript_for_cwd(session.cwd)
    offset = current_offset(transcript)
    if not send_keys(session.tmux_session, text):
        return None
    return wait_for_reply(session.cwd, offset, transcript, timeout)
