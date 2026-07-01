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
from datetime import UTC, datetime
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
    the same injection-safe transport ainb-fleet's broadcast/sequence use. A
    ``--`` terminator precedes the payload so a message starting with ``-`` is
    treated as literal text, not as a tmux flag (which would fail the send).
    """
    try:
        lit = subprocess.run(
            ["tmux", "send-keys", "-t", tmux_session, "-l", "--", text],
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


def _row_timestamp(obj: dict) -> float | None:
    """Parse a row's top-level ISO-8601 ``timestamp`` to epoch seconds, or ``None``.

    Claude writes each JSONL row with a ``timestamp`` like
    ``"2025-06-14T12:34:56.789Z"``. We compare it against the wall-clock send time
    so backlog rows (a resume/compaction rotation rolls prior history — including
    pre-send ``end_turn`` rows — into a new file) are never mistaken for the reply.
    """
    ts = obj.get("timestamp")
    if not isinstance(ts, str) or not ts:
        return None
    # ``fromisoformat`` accepts the ``+00:00`` offset but not a trailing ``Z``.
    normalized = ts[:-1] + "+00:00" if ts.endswith("Z") else ts
    try:
        dt = datetime.fromisoformat(normalized)
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=UTC)
    return dt.timestamp()


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


def _scan_new_rows_for_turn_end(
    path: Path, start_offset: int, min_timestamp: float | None = None
) -> tuple[int, str | None]:
    """Scan rows from ``start_offset``; return ``(new_offset, reply_text|None)``.

    ``reply_text`` is set only when an assistant row with
    ``stop_reason == "end_turn"`` is found in the new region. The last such
    turn wins (the most recent complete answer).

    When ``min_timestamp`` (epoch seconds) is given, only rows whose JSONL
    ``timestamp`` is strictly AFTER it are accepted. This rejects pre-send
    backlog: a resume/compaction rotation rolls prior history — including
    pre-send ``end_turn`` rows — into a new file, and offset-reset scanning
    would otherwise return that stale answer as the reply. A row without a
    parseable timestamp is also rejected once a guard is active, since it
    cannot be proven to post-date the send.

    Only COMPLETE (newline-terminated) lines are consumed. A trailing partial
    write (no terminating ``\n`` yet) is left for the next poll: the returned
    offset never advances past the last newline, so a reply that lands in a
    not-yet-flushed line is re-read once the line is completed instead of being
    skipped and lost.
    """
    try:
        with path.open("rb") as fh:
            fh.seek(start_offset)
            data = fh.read()
    except OSError:
        return start_offset, None

    # Advance only past complete lines: everything up to and including the last
    # newline. A trailing partial line stays unread until it is terminated.
    last_nl = data.rfind(b"\n")
    if last_nl == -1:
        # No complete line yet — nothing to consume, hold the offset.
        return start_offset, None
    complete = data[: last_nl + 1]
    new_offset = start_offset + last_nl + 1

    reply: str | None = None
    for line in complete.splitlines():
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
            if min_timestamp is not None:
                row_ts = _row_timestamp(obj)
                if row_ts is None or row_ts <= min_timestamp:
                    # Backlog (or unprovable) row — predates the send, skip it.
                    continue
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
    send_time: float | None = None,
) -> str | None:
    """Poll the transcript for the next end-of-turn assistant reply.

    ``transcript`` may be ``None`` at send time (no transcript yet); we re-resolve
    it on each poll so a freshly-created file is picked up. Claude can also rotate
    to a NEW ``*.jsonl`` mid-turn (resume / compaction), so on every poll we
    re-resolve the latest transcript: if it differs from the one we are reading,
    we switch to it and reset the offset to 0 so the end-of-turn row in the new
    file is not missed.

    ``send_time`` (epoch seconds, captured by the caller right before the send) is
    the backlog guard: only rows whose JSONL ``timestamp`` post-dates it count as
    the reply. Without it, an offset-reset on rotation would surface a rolled-up
    PRE-send ``end_turn`` (carried into the new file by resume/compaction) as the
    answer. Returns the reply text or ``None`` on timeout.
    """
    deadline = time.monotonic() + timeout
    offset = start_offset
    path = transcript
    while time.monotonic() < deadline:
        latest = latest_transcript_for_cwd(cwd)
        if latest is not None and latest != path:
            # First resolution after a None send-time path, or a rotation to a
            # newer transcript — read the new file from the top.
            path = latest
            offset = 0
        if path is not None:
            offset, reply = _scan_new_rows_for_turn_end(path, offset, send_time)
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
    # Wall-clock watermark: the reply must be a turn that ends AFTER this instant.
    send_time = time.time()
    if not send_keys(session.tmux_session, text):
        return None
    return wait_for_reply(session.cwd, offset, transcript, timeout, send_time=send_time)
