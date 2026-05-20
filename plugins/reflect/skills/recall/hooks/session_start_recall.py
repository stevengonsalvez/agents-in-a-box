#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
SessionStart Recall Hook (Phase 2 of reflect retrieval).

Fires on SessionStart. Builds a query from the current project context
(cwd, git branch, recent commits) and injects the top-3 learnings
(any confidence; reranked) into the agent's context via additionalContext.

Usage in settings.json:
{
  "hooks": {
    "SessionStart": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "uv run {{HOME_TOOL_DIR}}/skills/recall/hooks/session_start_recall.py"
      }]
    }]
  }
}

Exit behavior (D9): always exit 0 with possibly-empty hookSpecificOutput.
Never blocks, never errors out.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import traceback
from pathlib import Path
from typing import NoReturn


# --- Silent-fail event sink ----------------------------------------------
#
# Hooks MUST NOT raise into the user's session — a recall failure (graphrag
# down, broken cwd, missing dep) is not the user's problem. The shared
# helper in ``plugins/reflect/scripts/silent_fail.py`` handles the
# breadcrumb writer + credential scrubber + forensics log; we just import
# it. sys.path manipulation needed because uv-script mode doesn't see
# sibling packages by default.

_HOOK_NAME = "session_start_recall"
_PLUGIN_ROOT = Path(__file__).resolve().parents[3]  # skills/recall/hooks/<this> → plugins/reflect/
sys.path.insert(0, str(_PLUGIN_ROOT / "scripts"))
try:
    from silent_fail import write_last_event, forensics_log  # noqa: E402
except ImportError:
    # Defensive fallback: if the shared helper is missing (broken install)
    # we still must silent-fail. Define no-ops so the wrapper at the bottom
    # of this file can't itself blow up.
    def write_last_event(**kwargs):  # type: ignore[no-redef]
        pass
    def forensics_log(*args, **kwargs):  # type: ignore[no-redef]
        pass


# D2: conservative caps for auto-inject
SESSION_START_LIMIT = 3
SESSION_START_CONFIDENCE = "ANY"  # relaxed; rely on reranking
SESSION_START_MAX_CHARS = 1500  # tighter than explicit /reflect:recall


# --- Context extraction --------------------------------------------------

STOPWORDS = {
    "fix", "feat", "chore", "docs", "test", "refactor", "build", "ci", "perf",
    "the", "a", "an", "of", "to", "for", "on", "in", "at", "and", "or",
    "add", "remove", "update", "change", "merge", "pull", "request", "pr",
}


def git_capture(args: list[str], cwd: Path) -> str:
    try:
        r = subprocess.run(
            ["git", *args],
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if r.returncode == 0:
            return r.stdout.strip()
    except (subprocess.TimeoutExpired, OSError):
        # OSError subsumes FileNotFoundError / PermissionError — never let the
        # hook crash the session start just because git is missing or blocked.
        pass
    return ""


def project_name(cwd: Path) -> str:
    """Remote origin basename → fall back to cwd basename."""
    url = git_capture(["remote", "get-url", "origin"], cwd)
    if url:
        base = url.rstrip("/").rsplit("/", 1)[-1]
        return re.sub(r"\.git$", "", base)
    return cwd.name


def current_branch(cwd: Path) -> str:
    b = git_capture(["branch", "--show-current"], cwd)
    if b in ("main", "master", ""):
        return ""
    return b


def recent_commit_tags(cwd: Path, n: int = 5, limit: int = 3) -> list[str]:
    """Last N commit subjects → top-K alphanumeric tokens excluding stopwords."""
    log = git_capture(["log", f"-{n}", "--format=%s"], cwd)
    if not log:
        return []
    tokens: dict[str, int] = {}
    for line in log.splitlines():
        for tok in re.findall(r"[a-zA-Z][a-zA-Z0-9_-]{2,}", line):
            low = tok.lower()
            if low in STOPWORDS:
                continue
            tokens[low] = tokens.get(low, 0) + 1
    # Sort by frequency, stable
    ranked = sorted(tokens.items(), key=lambda kv: (-kv[1], kv[0]))
    return [t for t, _ in ranked[:limit]]


def build_query(cwd: Path) -> tuple[str, list[str]]:
    """
    D3: query = project_name + branch + top-3 commit-derived tags.
    Returns (query_string, tag_list_for_rerank).
    """
    parts = [project_name(cwd)]
    branch = current_branch(cwd)
    if branch:
        # Normalise: "feat/foo-bar" → "foo bar"
        parts.append(re.sub(r"[/_-]+", " ", branch))
    tags = recent_commit_tags(cwd)
    parts.extend(tags)
    # Dedup, preserving order
    seen: set[str] = set()
    dedup: list[str] = []
    for p in parts:
        for word in p.split():
            w = word.lower()
            if w and w not in seen:
                seen.add(w)
                dedup.append(word)
    return " ".join(dedup), tags


# --- Hook main -----------------------------------------------------------

def find_recall_script() -> Path | None:
    """recall.py may live in scripts/ of this plugin in deployed form."""
    here = Path(__file__).resolve().parent
    candidates = [
        here.parent / "scripts" / "recall.py",
        # fallback: colocated
        here / "recall.py",
    ]
    for c in candidates:
        if c.exists():
            return c
    return None


def emit(additional_context: str) -> NoReturn:
    """Always exit 0 with valid JSON.

    Typed NoReturn so callers (and linters) know execution stops here —
    no need for a `return` after `emit(...)` at the call site.
    """
    print(
        json.dumps(
            {
                "hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": additional_context,
                }
            }
        )
    )
    sys.exit(0)


# Resolve `uv` once at module load. SessionStart hooks often run with a
# trimmed PATH (launchd, IDE subprocesses), so a late lookup can fail even
# when `uv` is installed. None → fall through to empty emit.
UV_BIN = shutil.which("uv")


def _main_body() -> NoReturn:
    """The real work. Wrapped by ``main()`` in a top-level catch so any
    uncaught exception silent-fails to an empty inject + last-event log."""
    # Hooks receive JSON on stdin but we don't need it for cwd derivation
    try:
        _ = sys.stdin.read()
    except Exception:
        pass

    cwd = Path(os.environ.get("CLAUDE_PROJECT_DIR", os.getcwd())).resolve()

    # Skip for $HOME — no project context there
    if cwd == Path.home():
        emit("")

    query, tags = build_query(cwd)
    if not query:
        emit("")

    recall = find_recall_script()
    if not recall or not UV_BIN:
        emit("")

    # D9: SessionStart must feel instant. 10s cap — if recall is slower
    # than that, prefer empty context over a stalled session boot. The
    # recall cache makes repeat sessions fast; the first call absorbs
    # the miss silently.
    try:
        r = subprocess.run(
            [
                UV_BIN, "run", "--quiet", str(recall),
                query,
                "--limit", str(SESSION_START_LIMIT),
                "--confidence", SESSION_START_CONFIDENCE,
                "--format", "markdown",
                "--max-chars", str(SESSION_START_MAX_CHARS),
                "--tags", ",".join(tags),
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        emit("")

    if r.returncode != 0:
        emit("")

    emit((r.stdout or "").strip())


def main() -> NoReturn:
    """Top-level entry. Any uncaught exception falls through to an empty
    inject + a breadcrumb on ~/.reflect/last-event.json so the status line
    can show ⚠ without anything reaching the user's session."""
    try:
        _main_body()
    except SystemExit:
        # ``emit()`` and the inner code use sys.exit(0) for clean exits —
        # let those through unchanged.
        raise
    except BaseException as exc:  # noqa: BLE001 — deliberately broadest catch
        detail = str(exc) or traceback.format_exc(limit=2)
        write_last_event(
            hook_name=_HOOK_NAME,
            event="error",
            kind=type(exc).__name__,
            detail=detail,
        )
        forensics_log(_HOOK_NAME, f"{type(exc).__name__}: {detail}")
        # MUST exit 0 with valid JSON. Don't even let json.dumps raise —
        # use a literal so this last branch can never throw.
        try:
            sys.stdout.write(
                '{"hookSpecificOutput":{"hookEventName":"SessionStart",'
                '"additionalContext":""}}\n'
            )
            sys.stdout.flush()
        except Exception:
            pass
        sys.exit(0)


if __name__ == "__main__":
    main()
