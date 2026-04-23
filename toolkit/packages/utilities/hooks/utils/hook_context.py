#!/usr/bin/env python3
"""
Shared hook helpers for building speech-safe labels and parsing todo state
from Claude Code transcripts.
"""

import json
import os
import re
import subprocess
from pathlib import Path
from typing import Iterator, Optional


def run_git_command(args: list[str], cwd: str) -> str:
    """Run a git command and return stdout, empty on failure."""
    try:
        result = subprocess.run(
            ["git", *args],
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=0.5,
        )
        return result.stdout.strip() if result.returncode == 0 else ""
    except Exception:
        return ""


def normalize_for_speech(value: str) -> str:
    """Convert file/path-like names into speech-friendly text."""
    cleaned = re.sub(r"[/_.-]+", " ", value or "")
    cleaned = re.sub(r"\s+", " ", cleaned).strip()
    return cleaned


def build_session_label(cwd: str) -> str:
    """Build a short label that identifies the current worktree/session."""
    if not cwd:
        cwd = os.getcwd()

    cwd_path = Path(cwd)
    fallback = normalize_for_speech(cwd_path.name) or "current session"

    git_root = run_git_command(["rev-parse", "--show-toplevel"], cwd)
    if not git_root:
        return fallback

    project_name = normalize_for_speech(Path(git_root).name)
    branch = normalize_for_speech(run_git_command(["branch", "--show-current"], cwd))

    if branch and branch not in {"head", "main", "master"}:
        return f"{project_name}, {branch}"
    if project_name:
        return project_name
    return fallback


def iter_transcript_lines_reverse(
    transcript_path: str,
    chunk_size: int = 65536,
) -> Iterator[str]:
    """Yield JSONL transcript lines from the end of the file backward."""
    if not transcript_path or not Path(transcript_path).exists():
        return

    try:
        with open(transcript_path, "rb") as handle:
            handle.seek(0, 2)
            position = handle.tell()
            remainder = ""

            while position > 0:
                read_size = min(chunk_size, position)
                position -= read_size
                handle.seek(position)
                chunk = handle.read(read_size).decode("utf-8", errors="ignore")
                data = chunk + remainder
                lines = data.splitlines()

                # When position > 0, the first element may be a partial line.
                if position > 0:
                    remainder = lines[0] if lines else data
                    lines = lines[1:] if len(lines) > 1 else []
                else:
                    remainder = ""

                for line in reversed(lines):
                    if line.strip():
                        yield line

            if remainder.strip():
                yield remainder
    except Exception:
        return


def extract_todo_snapshot_from_value(value: object) -> Optional[dict]:
    """Extract a normalized todo snapshot from a parsed JSON value."""
    if not isinstance(value, dict):
        return None

    todos = value.get("todos")
    if not isinstance(todos, list):
        return None

    items: list[dict] = []
    pending = 0
    in_progress = 0
    done = 0

    for todo in todos:
        if not isinstance(todo, dict):
            continue

        text = (
            todo.get("text")
            or todo.get("task")
            or todo.get("content")
            or ""
        )
        status = str(todo.get("status", "pending"))

        if status in {"done", "completed"}:
            done += 1
        elif status in {"in_progress", "active"}:
            in_progress += 1
        else:
            pending += 1

        items.append({"text": str(text), "status": status})

    return {
        "title": value.get("title"),
        "items": items,
        "pending": pending,
        "in_progress": in_progress,
        "done": done,
    }


def extract_latest_todo_snapshot(transcript_path: str) -> Optional[dict]:
    """Find the latest TodoWrite snapshot in a Claude transcript."""
    for line in iter_transcript_lines_reverse(transcript_path):
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue

        candidates = []
        if isinstance(entry, dict):
            message = entry.get("message")
            if isinstance(message, dict):
                candidates.append(message)
            candidates.append(entry)

        for candidate in candidates:
            content = candidate.get("content")
            if not isinstance(content, list):
                continue

            for item in reversed(content):
                if not isinstance(item, dict):
                    continue

                if item.get("type") == "tool_use" and item.get("name") == "TodoWrite":
                    snapshot = extract_todo_snapshot_from_value(item.get("input"))
                    if snapshot:
                        return snapshot

                if item.get("type") == "tool_result":
                    raw_content = item.get("content")
                    if isinstance(raw_content, str):
                        try:
                            parsed = json.loads(raw_content)
                        except json.JSONDecodeError:
                            continue
                        snapshot = extract_todo_snapshot_from_value(parsed)
                        if snapshot:
                            return snapshot

    return None


def summarize_todos(snapshot: Optional[dict]) -> Optional[str]:
    """Convert a normalized todo snapshot into a compact spoken summary."""
    if not snapshot:
        return None

    pending = int(snapshot.get("pending", 0))
    in_progress = int(snapshot.get("in_progress", 0))

    parts = []
    if pending:
        parts.append(f"{pending} pending")
    if in_progress:
        parts.append(f"{in_progress} in progress")

    if not parts:
        return None

    return ", ".join(parts)
