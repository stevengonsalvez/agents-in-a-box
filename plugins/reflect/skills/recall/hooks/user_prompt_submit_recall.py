#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
UserPromptSubmit Recall Hook (Phase 3 of reflect retrieval).

Fires on every UserPromptSubmit. Uses the user's actual prompt as the
GraphRAG query — much sharper than SessionStart's cwd/branch heuristic —
and injects new (not-already-injected) top-N learnings as
``additionalContext``.

Companion to ``session_start_recall.py``:

  * SessionStart fires BEFORE the user has typed anything; query has to
    be inferred from cwd, branch, recent commits. Coarse but immediate.
  * UserPromptSubmit has the actual prompt to query against; sharp hits.
    Per-session dedupe (``~/.reflect/session-injected/<sid>.json``)
    prevents re-injecting the same learning on every prompt.

This hook ALSO handles the second half of the PostToolUse mini-learning
capture: if the PostToolUse hook armed a watcher
(``~/.reflect/armed/<sid>.json``) and the current prompt looks like a
correction, write a low-confidence learning directly to disk WITHOUT
calling /reflect, then clear the armed state.

Usage in hooks config (Claude plugin.json or Codex hooks.json):
{
  "hooks": {
    "UserPromptSubmit": [{
      "matcher": "",
      "hooks": [{
        "type": "command",
        "command": "uv run <HOME_TOOL_DIR>/skills/recall/hooks/user_prompt_submit_recall.py"
      }]
    }]
  }
}

Exit behavior: always exits 0 with possibly-empty hookSpecificOutput.
On any uncaught exception, falls through to empty inject + breadcrumb.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
import time
import traceback
from pathlib import Path
from typing import NoReturn


# Shared silent-fail mechanics.
_HOOK_NAME = "user_prompt_submit_recall"
_PLUGIN_ROOT = Path(__file__).resolve().parents[3]  # skills/recall/hooks/<this> → plugins/reflect/
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

# Cross-harness stdin readers (snake_case claude/codex, camelCase copilot).
# Same import-or-inline-fallback convention as silent_fail; the import
# no-ops in the deployed copilot layout (recall hooks land under
# ~/.copilot/skills/recall/hooks/ where scripts/ doesn't resolve), so the
# inline copy below carries the camelCase tolerance there.
try:
    from hook_input import get_session_id  # noqa: E402
except ImportError:
    def get_session_id(data, default=""):  # type: ignore[no-redef]
        for k in ("session_id", "sessionId"):
            if k in data:
                return data[k]
        return default


# --- Tunables ------------------------------------------------------------

# Per-prompt recall is tighter than SessionStart baseline. SessionStart
# injects 3 broad learnings; we inject up to 3 prompt-sharp ones but
# dedupe against the session-injected set.
USER_PROMPT_LIMIT = 3
USER_PROMPT_CONFIDENCE = "ANY"
USER_PROMPT_MAX_CHARS = 1500

# Minimum prompt length to bother querying — anything shorter is too
# noisy to give useful hits and would inject random learnings on every
# "hi" / "ok" / "next".
MIN_PROMPT_CHARS = 12


# --- Paths (resolved per-call to honor REFLECT_STATE_DIR at runtime) -----

def state_dir() -> Path:
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def session_injected_path(session_id: str) -> Path:
    return state_dir() / "session-injected" / f"{session_id}.json"


def armed_path(session_id: str) -> Path:
    return state_dir() / "armed" / f"{session_id}.json"


def learnings_dir() -> Path:
    """Where mini-learnings get written. Honors REFLECT_LEARNINGS_DIR
    override; defaults to ~/.learnings/documents/."""
    custom = os.environ.get("REFLECT_LEARNINGS_DIR")
    if custom:
        return Path(custom).expanduser()
    return Path.home() / ".learnings" / "documents"


# --- Mini-learning correction detection ---------------------------------

# Patterns that suggest the user is correcting a failed approach. Tuned
# to be conservative — false positives produce noise learnings, so we
# require an explicit corrective verb. ``\S+`` (not ``\w+``) so flag-like
# tokens such as ``--insecure`` are matched naturally.
_CORRECTION_PATTERNS = [
    re.compile(r"(?i)\b(?:try|use|do)\s+\S+(?:\s+\S+)*?\s+(?:instead|rather)\b"),
    re.compile(r"(?i)\b(?:no|don't|do not),?\s+(?:use|try|do)\b"),
    re.compile(r"(?i)\b(?:should have|shouldn't have|must use|need to use)\b"),
    re.compile(r"(?i)\binstead\s+of\s+\S+,?\s+(?:use|try|do)\b"),
]


def looks_like_correction(prompt: str) -> bool:
    return any(p.search(prompt) for p in _CORRECTION_PATTERNS)


# --- Dedupe state --------------------------------------------------------

def load_session_injected(session_id: str) -> set[str]:
    """Per-session set of learning IDs already injected. Returns empty
    set on missing/corrupt file (best-effort, never raises)."""
    if not session_id:
        return set()
    p = session_injected_path(session_id)
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
        ids = data.get("injected", [])
        return set(ids) if isinstance(ids, list) else set()
    except (FileNotFoundError, OSError, json.JSONDecodeError, ValueError):
        return set()


def save_session_injected(session_id: str, injected: set[str]) -> None:
    """Atomic write of the dedupe set. Best-effort; never raises."""
    if not session_id:
        return
    try:
        p = session_injected_path(session_id)
        p.parent.mkdir(parents=True, exist_ok=True)
        payload = {"injected": sorted(injected), "ts": time.time()}
        tmp = p.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(payload), encoding="utf-8")
        tmp.replace(p)
    except Exception:
        pass


# --- recall.py invocation (mirrors session_start_recall.py) -------------

UV_BIN = shutil.which("uv")


def find_recall_script() -> Path | None:
    here = Path(__file__).resolve().parent
    candidates = [
        here.parent / "scripts" / "recall.py",
        here / "recall.py",
    ]
    for c in candidates:
        if c.exists():
            return c
    return None


def query_recall(query: str) -> tuple[str, list[str]]:
    """Run the recall script with the prompt as query.

    Returns ``(markdown_output, learning_ids)``. On any failure, returns
    ``("", [])`` — silent.

    The recall script emits markdown with learning IDs in ``[lrn-...]``
    style brackets; we extract those for the dedupe set. If we ever
    can't parse the IDs we still inject the markdown — better to
    re-inject a learning than skip recall entirely.
    """
    recall = find_recall_script()
    if not recall or not UV_BIN:
        return "", []
    try:
        r = subprocess.run(
            [
                UV_BIN, "run", "--quiet", str(recall),
                query,
                "--limit", str(USER_PROMPT_LIMIT * 3),  # over-fetch; dedupe filters
                "--confidence", USER_PROMPT_CONFIDENCE,
                "--format", "markdown",
                "--max-chars", str(USER_PROMPT_MAX_CHARS * 2),
                "--tags", "",
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        return "", []
    if r.returncode != 0:
        return "", []
    output = (r.stdout or "").strip()
    ids = re.findall(r"\[lrn-[a-z0-9\-]+\]", output)
    return output, ids


def filter_to_new(markdown: str, already_injected: set[str]) -> tuple[str, list[str]]:
    """Strip out blocks corresponding to already-injected learning IDs.

    The recall script emits markdown as a flat list of bullets (one per
    learning). We split on top-level ``"- "`` lines and keep blocks whose
    ``[lrn-...]`` ID is NOT in ``already_injected``. Returns
    ``(filtered_markdown, new_ids)``.
    """
    if not markdown:
        return "", []
    blocks: list[list[str]] = []
    current: list[str] = []
    for line in markdown.split("\n"):
        # New bullet starts a new block (top-level "- " line).
        if line.startswith("- ") and current:
            blocks.append(current)
            current = [line]
        else:
            current.append(line)
    if current:
        blocks.append(current)

    kept_blocks: list[str] = []
    kept_ids: list[str] = []
    for block in blocks:
        block_text = "\n".join(block)
        ids_in_block = re.findall(r"\[(lrn-[a-z0-9\-]+)\]", block_text)
        if any(i in already_injected for i in ids_in_block):
            continue  # already injected this session — skip
        kept_blocks.append(block_text)
        kept_ids.extend(ids_in_block)
        if len(kept_blocks) >= USER_PROMPT_LIMIT:
            break
    return "\n".join(kept_blocks), kept_ids[:USER_PROMPT_LIMIT]


# --- Mini-learning capture (Phase 2 of PostToolUse arming) ---------------

def maybe_capture_minilearning(session_id: str, prompt: str) -> bool:
    """If PostToolUse armed a watcher for this session and the current
    prompt looks like a correction, write a low-confidence learning to
    disk and clear the armed state.

    Returns ``True`` iff a learning was written. Best-effort; swallows
    all errors (silent-fail).
    """
    if not session_id:
        return False
    armed = armed_path(session_id)
    if not armed.exists():
        return False
    try:
        armed_data = json.loads(armed.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        # Clear the broken armed file so it doesn't linger.
        try:
            armed.unlink()
        except OSError:
            pass
        return False

    if not looks_like_correction(prompt):
        # Not a correction — leave the armed state in place for a few
        # minutes in case the NEXT prompt is the correction. We add a
        # max-age check below.
        try:
            armed_age = time.time() - float(armed_data.get("ts", 0))
            if armed_age > 600:  # 10 minutes
                armed.unlink()
        except (OSError, ValueError):
            pass
        return False

    # Write the mini-learning.
    try:
        ld = learnings_dir()
        ld.mkdir(parents=True, exist_ok=True)
        ts = int(time.time())
        slug = f"lrn-mini-{ts}-{session_id[:8]}"
        path = ld / f"{slug}.md"
        tool = armed_data.get("tool", "unknown")
        tool_input = scrub_secrets(str(armed_data.get("tool_input", ""))[:200])
        tool_response = scrub_secrets(str(armed_data.get("tool_response", ""))[:200])
        body = (
            f"---\n"
            f"id: {slug}\n"
            f"confidence: low\n"
            f"source: posttooluse-minilearning\n"
            f"session_id: {session_id}\n"
            f"captured_at: {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n"
            f"---\n\n"
            f"# Mini-learning: {tool} correction\n\n"
            f"**Failed tool call**: `{tool}`\n\n"
            f"Input (truncated): `{tool_input}`\n\n"
            f"Response (truncated): `{tool_response}`\n\n"
            f"**User correction**: {scrub_secrets(prompt[:500])}\n\n"
            f"_Auto-captured by the PostToolUse + UserPromptSubmit watcher. "
            f"Confidence is `low` — review before relying on it._\n"
        )
        path.write_text(body, encoding="utf-8")
        forensics_log(_HOOK_NAME, f"mini-learning captured: {slug}")
    except Exception:
        return False

    # Clear armed state (single shot).
    try:
        armed.unlink()
    except OSError:
        pass
    return True


# --- Output --------------------------------------------------------------

def emit(additional_context: str) -> NoReturn:
    """Always exit 0 with valid JSON for the UserPromptSubmit event.

    Output envelope is harness-gated on ``REFLECT_HARNESS`` (set by the
    adapter on the hook command), mirroring ``session_start_recall.emit``.

    NOTE: on Copilot the ``userPromptSubmitted`` hook's *output is ignored*
    by the CLI, so neither envelope can actually surface recall to the
    model on that harness — this hook still fires there purely for its
    capture/dedupe side-effects (the mini-learning watcher, the
    session-injected dedupe set). We emit the plain copilot shape anyway
    so the contract is consistent and a future Copilot version that starts
    honouring the output would Just Work.

    TODO(copilot-envelope): same docs-silent caveat as
    ``session_start_recall.emit`` — confirm against the live binary once
    policy is lifted.
    """
    if os.environ.get("REFLECT_HARNESS") == "copilot":
        print(json.dumps({"additionalContext": additional_context}))
    else:
        print(
            json.dumps(
                {
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                        "additionalContext": additional_context,
                    }
                }
            )
        )
    sys.exit(0)


def _main_body() -> NoReturn:
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
    # ``prompt`` is the claude/codex key. Copilot's userPromptSubmitted
    # payload is camelCase elsewhere, so tolerate ``userPrompt`` too —
    # presence-first, snake_case wins when both are absent is moot here.
    prompt = ""
    for _k in ("prompt", "userPrompt"):
        if _k in data:
            prompt = str(data[_k] or "").strip()
            break

    # If we can capture a mini-learning, do it BEFORE recall — the
    # captured learning won't be in the index yet but the act of
    # writing it is the side-effect we care about.
    if session_id and prompt:
        maybe_capture_minilearning(session_id, prompt)

    if len(prompt) < MIN_PROMPT_CHARS:
        emit("")

    # Query recall with the prompt itself.
    markdown, _ = query_recall(prompt)
    if not markdown:
        emit("")

    already = load_session_injected(session_id)
    filtered, new_ids = filter_to_new(markdown, already)
    if not filtered:
        emit("")

    # Persist the new IDs so future prompts in this session don't
    # re-inject the same learnings.
    if session_id and new_ids:
        save_session_injected(session_id, already | set(new_ids))

    # Truncate to the per-prompt char budget (filter_to_new uses block
    # boundaries; this is a hard upper bound).
    if len(filtered) > USER_PROMPT_MAX_CHARS:
        filtered = filtered[:USER_PROMPT_MAX_CHARS].rstrip() + " …"

    emit(filtered)


def main() -> NoReturn:
    """Top-level entry. Silent-fail wrapper — any uncaught exception
    becomes an empty inject + a breadcrumb on ~/.reflect/last-event.json."""
    try:
        _main_body()
    except SystemExit:
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
        try:
            sys.stdout.write(
                '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit",'
                '"additionalContext":""}}\n'
            )
            sys.stdout.flush()
        except Exception:
            pass
        sys.exit(0)


if __name__ == "__main__":
    main()
