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

R10 (opt-in via REFLECT_TIERED_INJECT): retrieval is tiered — the skills
index (R20, curated) is consulted FIRST, and a strong skill hit injects
just the skill name + one-line summary, skipping the raw-learnings recall
entirely. Lower tiers run only when the skills tier is empty or stale.

A1 (opt-in via REFLECT_SLOTS): memory slots are Tier-0 — the agent-curated
scratchpads (persona, pending_items, project_context, ...) inject BEFORE
any skill hit or recall result, and unlike the skills tier they never
suppress the lower tiers: the slots block is PREPENDED to whatever the
rest of the hierarchy produces.

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
SESSION_START_MAX_CHARS = 1500
# R7: OOD gate — suppress injection when even the best hit barely mentions the
# query's terms (most sessions have NO relevant prior art; junk costs context).
SESSION_START_MIN_OVERLAP = float(os.environ.get("REFLECT_RECALL_MIN_OVERLAP", "0.2"))
# R4: optional token budget for the injected block (0 = keep max-chars only).
SESSION_START_MAX_TOKENS = int(os.environ.get("REFLECT_RECALL_MAX_TOKENS", "0"))  # tighter than explicit /reflect:recall
# R10: tiered inject — skills (curated) > learnings (raw). Opt-in rollout
# flag: the skills tier only runs when REFLECT_TIERED_INJECT is truthy.
TIERED_INJECT_FLAG = "REFLECT_TIERED_INJECT"
SKILL_TIER_LIMIT = 2
# match_skills scores name/tag token hits at 2.0 and summary hits at 1.0;
# default threshold = at least one strong (name/tag) hit.
SKILL_TIER_DEFAULT_MIN_SCORE = 2.0
# A1: memory slots are Tier-0 of the inject hierarchy. Opt-in rollout flag
# (mirrors agentmemory's SLOTS=on gate); char budget for the slots block.
SLOTS_FLAG = "REFLECT_SLOTS"
SLOT_TIER_MAX_CHARS = 4000


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


# --- A1: slots tier (Tier-0 — the agent's working memory) ----------------

def slots_enabled() -> bool:
    """Opt-in rollout flag for the slots tier (read at call time so
    settings.json env stanzas and tests can flip it)."""
    return os.environ.get(SLOTS_FLAG, "").strip().lower() in (
        "1", "true", "yes", "on",
    )


def slot_tier_context(cwd: Path) -> str:
    """Tier 0 of the inject hierarchy: pinned editable memory slots.

    Seeds the 8 default slots for this project (idempotent), then renders
    every non-empty slot as a compact markdown block. Unlike the skills
    tier this block never wins outright — it is PREPENDED to whatever the
    lower tiers produce, because slots are working memory, not retrieval.

    Silent-fail: any error (missing module, locked DB, broken config)
    returns "" so SessionStart degrades to the slot-less behaviour.
    """
    if not slots_enabled():
        return ""
    try:
        # Lazy import: only pay the sqlite cost when the flag is on.
        # scripts/ is already on sys.path (silent_fail import above).
        import reflect_db

        conn = reflect_db.get_conn()
        project_id = reflect_db.derive_slot_project_id(cwd)
        reflect_db.ensure_default_slots(project_id, conn=conn)
        return reflect_db.render_slots_context(
            project_id=project_id, max_chars=SLOT_TIER_MAX_CHARS, conn=conn,
        )
    except Exception:
        return ""


def join_blocks(*blocks: str) -> str:
    """Join non-empty context blocks with a blank line."""
    return "\n\n".join(b for b in blocks if b)


# --- R10: skills tier (curated beats raw) --------------------------------

def tiered_inject_enabled() -> bool:
    """Opt-in rollout flag. Read at call time so settings.json env stanzas
    (and tests) can flip it without re-importing the hook."""
    return os.environ.get(TIERED_INJECT_FLAG, "").strip().lower() in (
        "1", "true", "yes", "on",
    )


def skill_tier_min_score() -> float:
    """'Strong hit' threshold for the skills tier (env-tunable)."""
    try:
        return float(os.environ["REFLECT_SKILL_TIER_MIN_SCORE"])
    except (KeyError, ValueError):
        return SKILL_TIER_DEFAULT_MIN_SCORE


def skill_tier_context(query: str) -> str:
    """Tier 1 of the R10 hierarchy: skills (curated) outrank raw learnings.

    Matches *query* against the R20 skills index (the ``skills`` table in
    reflect.db). A strong hit returns a compact block — skill name + one-
    line summary per hit — and the caller skips the recall.py learnings
    pass entirely: a polished skill always wins over a raw note covering
    the same ground. ``refresh_if_stale()`` runs first (stat()-only for
    unchanged skills) so a deleted or stale skill can never win the tier;
    an empty/stale top tier falls through to the learnings inject below.

    Silent-fail: any error (missing module, locked/unwritable DB, broken
    config) returns "" so the hook degrades to the flat learnings path.
    """
    if not tiered_inject_enabled():
        return ""
    try:
        # Lazy imports: only pay the sqlite/index cost when the flag is on.
        # scripts/ is already on sys.path (silent_fail import above).
        import reflect_db
        import skill_index

        conn = reflect_db.get_conn()
        skill_index.refresh_if_stale(conn=conn)
        min_score = skill_tier_min_score()
        hits = [
            hit
            for hit in skill_index.match_skills(
                query, limit=SKILL_TIER_LIMIT, conn=conn
            )
            if hit["score"] >= min_score
        ]
        if not hits:
            return ""
        lines = [
            "## Skills for this context (curated — prefer over raw learnings)"
        ]
        for hit in hits:
            summary = hit.get("summary") or "(no summary)"
            lines.append(f"- **{hit['name']}** — {summary}")
        return "\n".join(lines)[:SESSION_START_MAX_CHARS]
    except Exception:
        return ""


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

    # A1: slots are Tier-0 — agent-curated working memory injects BEFORE
    # any recall result, and is prepended to every emit path below.
    slots_block = slot_tier_context(cwd)

    query, tags = build_query(cwd)
    if not query:
        emit(slots_block)

    # R10: tiered inject — skills (curated) are the top retrieval tier. A
    # strong skill hit wins outright; the learnings recall below only runs
    # when the skills tier is empty/stale (or the flag is off).
    skill_block = skill_tier_context(query)
    if skill_block:
        emit(join_blocks(slots_block, skill_block))

    recall = find_recall_script()
    if not recall or not UV_BIN:
        emit(slots_block)

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
                "--min-overlap", str(SESSION_START_MIN_OVERLAP),
                "--max-tokens", str(SESSION_START_MAX_TOKENS),
                "--tags", ",".join(tags),
                # SG6: SessionStart queries are synthetic (cwd/branch/commit
                # tokens, not a genuine ask) and come up empty on most
                # sessions — recording them as knowledge gaps would surface
                # the project name as a fake gap every session.
                "--no-gap-log",
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        emit(slots_block)

    if r.returncode != 0:
        emit(slots_block)

    emit(join_blocks(slots_block, (r.stdout or "").strip()))


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
