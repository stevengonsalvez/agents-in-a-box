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

O2 (rides REFLECT_TIERED_INJECT): the per-project conventions doc is a
Tier-1 ambient pointer — when a fresh CONVENTIONS.md exists for this
project, ONE line (summary + path) is prepended alongside the slots block.
Never the doc body, and never when the doc is stale (the R14-shaped
``compute_conventions_is_stale`` check) — a wrong pointer is worse than
no pointer. Like slots, it never suppresses the lower tiers.

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

# Cross-harness stdin readers (snake_case claude/codex, camelCase copilot).
# Same import-or-inline-fallback convention as silent_fail above. In the
# *deployed* copilot layout this hook lands at
# ``~/.copilot/skills/recall/hooks/`` and ``scripts/`` resolves to a
# non-existent path (a pre-existing quirk shared with silent_fail), so the
# import no-ops there and the inline copy below takes over.
try:
    from hook_input import get_cwd  # noqa: E402
except ImportError:
    def get_cwd(data, default=""):  # type: ignore[no-redef]
        return data["cwd"] if isinstance(data, dict) and "cwd" in data else default


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
# O2: opt-in flag for materializing a CONVENTIONS.md symlink in the project
# root (writing into user repos is intrusive — the injected path already
# makes the doc readable without it).
CONVENTIONS_SYMLINK_FLAG = "REFLECT_CONVENTIONS_SYMLINK"


# --- R11: forced-grounding short-circuit ---------------------------------
#
# Ported from Hindsight (agent.py:305 _all_mental_models_are_usable_and_fresh
# + agent.py:993-1003). On a warm/familiar project the tier-1 (skills) hit is
# usually fresh AND high-confidence — in that case the broad lower-tier recall
# is pure noise and token cost. So if the top skills hit clears both the
# freshness and the rerank-score gate, we short-circuit: SessionStart does ONE
# skill lookup and is done, never spawning the lower-tier recall.py subprocess.
#
# The two knobs are deliberately separate so the proof can flip one at a time:
#   * freshness:  the skill hit must be explicitly fresh (mirrors Hindsight's
#                 `is_stale is not False` — unknown/missing freshness is unsafe).
#   * score:      the skill's rerank score must be STRICTLY above the threshold.
SHORT_CIRCUIT_SCORE_THRESHOLD = 0.8  # rerank-score gate; warm hits clear it
SHORT_CIRCUIT_MAX_AGE_DAYS = 30  # a skill older than this is "stale" for boot


def freshness_check(skill: dict, max_age_days: int = SHORT_CIRCUIT_MAX_AGE_DAYS) -> bool:
    """Is this tier-1 skill hit explicitly fresh and usable?

    Mirrors Hindsight's ``_all_mental_models_are_usable_and_fresh``: a hit is
    fresh only when freshness is *explicitly* asserted (unknown/missing => not
    fresh) and the body is non-empty. ``skill`` is a small dict the skills-tier
    probe produces:

        {"content": str, "age_days": float | None, "is_stale": bool | None}

    Decision (all must hold):
      * content is present and non-blank (an empty skill is never usable);
      * the hit is not explicitly stale (``is_stale is True`` => not fresh);
      * if an age is known, it is within ``max_age_days`` (older => stale).
        A missing age is treated as unknown freshness => NOT fresh, so a hit
        with no provenance can never short-circuit.
    """
    if not str(skill.get("content") or "").strip():
        return False
    if skill.get("is_stale") is True:
        return False
    age = skill.get("age_days")
    if age is None:
        return False  # unknown provenance is not "explicitly fresh"
    try:
        return float(age) <= max_age_days
    except (TypeError, ValueError):
        return False


def should_short_circuit(
    skill: dict | None,
    *,
    threshold: float = SHORT_CIRCUIT_SCORE_THRESHOLD,
    max_age_days: int = SHORT_CIRCUIT_MAX_AGE_DAYS,
) -> bool:
    """Tier-1 short-circuit decision: fresh AND rerank-score above threshold.

    Returns True iff the skills-tier hit exists, passes ``freshness_check``, and
    its rerank score is STRICTLY greater than ``threshold``. The score gate is
    the knob: the same fresh skill flips short-circuit on/off as its score
    crosses ``threshold``. When True the caller emits the skills-only payload
    and never runs the lower-tier recall.
    """
    if not skill:
        return False
    if not freshness_check(skill, max_age_days=max_age_days):
        return False
    try:
        score = float(skill.get("score"))
    except (TypeError, ValueError):
        return False
    return score > threshold


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


def sg2_commit_tags(limit: int = 3, n: int = 5) -> list[str]:
    """SG2 enrichment: top tokens from the post-commit-captured ``commits.jsonl``.

    The SG2 git-event capture (hooks/post_commit.sh -> reflect_db.record_commit)
    records every commit's subject + branch keyed to the session. Reading the
    newest few SHA-linked subjects here enriches the SessionStart recall query
    with the *captured* commit context — which can be richer than raw
    ``git log`` (it carries merge-conflict and revert signal the plain log
    drops). Additive: returns [] on any failure so the existing git-derived
    query is unchanged. State path follows reflect_db's convention
    (REFLECT_STATE_DIR override, else ~/.reflect).
    """
    try:
        state = Path(
            os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect"))
        )
        commits_file = state / "commits.jsonl"
        if not commits_file.is_file():
            return []
        lines = [
            ln for ln in commits_file.read_text(encoding="utf-8").splitlines()
            if ln.strip()
        ]
        tokens: dict[str, int] = {}
        for ln in lines[-n:]:
            try:
                rec = json.loads(ln)
            except (json.JSONDecodeError, TypeError):
                continue
            subject = str(rec.get("message", "")).splitlines()[:1]
            for line in subject:
                for tok in re.findall(r"[a-zA-Z][a-zA-Z0-9_-]{2,}", line):
                    low = tok.lower()
                    if low in STOPWORDS:
                        continue
                    tokens[low] = tokens.get(low, 0) + 1
        ranked = sorted(tokens.items(), key=lambda kv: (-kv[1], kv[0]))
        return [t for t, _ in ranked[:limit]]
    except Exception:
        return []


def build_query(cwd: Path) -> tuple[str, list[str]]:
    """
    D3: query = project_name + branch + top-3 commit-derived tags.
    Returns (query_string, tag_list_for_rerank).

    SG2: enriched with tokens from the post-commit-captured commits.jsonl
    (SHA<->session linkage) when present — additive, never subtractive.
    """
    parts = [project_name(cwd)]
    branch = current_branch(cwd)
    if branch:
        # Normalise: "feat/foo-bar" → "foo bar"
        parts.append(re.sub(r"[/_-]+", " ", branch))
    tags = recent_commit_tags(cwd)
    # SG2 additive enrichment: fold in tags from captured commits (deduped
    # below). Order-preserving so the plain git-log tags still lead.
    for sg2_tag in sg2_commit_tags():
        if sg2_tag not in tags:
            tags.append(sg2_tag)
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


# --- M8: token-economics footer -------------------------------------------

# Matches the per-row economics recall.py renders next to each learning's
# type glyph: "D:<discovery> → R:<read> (-<pct>%)". The hook owns the
# session-level footer; recall.py owns the per-row numbers — parsing our
# own row format keeps this hook stdlib-only (no yaml import, no second
# recall subprocess).
ECONOMICS_ROW_RE = re.compile(r"D:(\d+) → R:(\d+)")


def economics_footer(block: str) -> str:
    """M8: one-line token-economics roll-up for the injected recall block.

    'memory: N learnings, ~X tok injected, est ~Y tok saved' — X is the
    re-read cost actually paid this session, Y the discovery cost the
    learnings spare the agent from re-deriving (claude-mem renderFooter
    shape). Returns "" when the block carries no economics rows (economics
    disabled via RECALL_ECONOMICS=0, empty inject, skills-tier win) so the
    footer never appears without numbers backing it.
    """
    rows = ECONOMICS_ROW_RE.findall(block or "")
    if not rows:
        return ""
    discovery = sum(int(d) for d, _ in rows)
    read = sum(int(r) for _, r in rows)
    saved = discovery - read
    return (
        f"memory: {len(rows)} learnings, ~{read} tok injected, "
        f"est ~{saved} tok saved"
    )


# --- O2: conventions tier (Tier-1 ambient — pre-synthesized doc pointer) --

def conventions_symlink_enabled() -> bool:
    """Opt-in flag for the project-root CONVENTIONS.md symlink (read at
    call time so settings.json env stanzas and tests can flip it)."""
    return os.environ.get(CONVENTIONS_SYMLINK_FLAG, "").strip().lower() in (
        "1", "true", "yes", "on",
    )


def conventions_tier_context(cwd: Path) -> str:
    """Tier 1 ambient of the inject hierarchy (O2): the per-project
    conventions doc, surfaced as a 1-line summary + path — NEVER the doc
    body. Reading the doc is the agent's choice and costs zero boot tokens
    (it is a pre-synthesized regular file under the reflect state dir).

    Rides the R10 tiered-inject flag. A stale doc (R14-shaped check: any
    in-scope observation changed after last_refreshed_at, or the stored
    trigger flag is set) injects NOTHING — a wrong pointer is worse than
    no pointer. Like the slots tier, the block never suppresses lower
    tiers: it is PREPENDED to whatever the rest of the hierarchy produces.

    Silent-fail: any error (missing module, locked DB, broken config)
    returns "" so SessionStart degrades to the conventions-less behaviour.
    """
    if not tiered_inject_enabled():
        return ""
    try:
        # Lazy imports: only pay the sqlite cost when the flag is on.
        # scripts/ is already on sys.path (silent_fail import above).
        import conventions_generator
        import reflect_db

        conn = reflect_db.get_conn()
        project_id = reflect_db.derive_slot_project_id(cwd)
        line = conventions_generator.session_inject_line(project_id, conn=conn)
        if line and conventions_symlink_enabled():
            try:
                conventions_generator.symlink_into_project(
                    project_id, cwd, conn=conn,
                )
            except Exception:
                pass  # the injected path still works without the symlink
        return line
    except Exception:
        return ""


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


def _rerank_score(rec: dict) -> float:
    """Map a recall.py JSON result row to a 0..1 rerank score.

    recall.py's JSON shape (render_json) carries ``confidence`` as a tier
    string; we map it back to the same weights recall.py's rerank uses so the
    short-circuit gate reasons over a comparable scale without re-running the
    embedding model. HIGH -> 1.0, MEDIUM -> 0.7, LOW -> 0.4.
    """
    weights = {"HIGH": 1.0, "MEDIUM": 0.7, "LOW": 0.4}
    return weights.get(str(rec.get("confidence", "")).upper(), 0.5)


def _age_days(archived_at: str | None) -> float | None:
    """Days since the skill's archive timestamp; None if unknown/unparseable."""
    if not archived_at:
        return None
    from datetime import datetime

    try:
        ts = datetime.fromisoformat(str(archived_at).rstrip("Z"))
    except (ValueError, TypeError):
        return None
    try:
        return max(0.0, (datetime.now() - ts).days)
    except (TypeError, ValueError):
        return None


def skills_tier_probe(query: str, tags: list[str], recall: Path) -> dict | None:
    """R11: the single tier-1 (skills) lookup.

    Runs ONE focused, HIGH-confidence recall (limit 1, JSON) — the "skills"
    tier — and folds the top hit into the small dict ``freshness_check`` /
    ``should_short_circuit`` consume. Returns None on any miss (no hit, recall
    unavailable, bad JSON) so the caller falls through to the lower tiers.

    This is deliberately the *only* probe that may run before the short-circuit
    decision: on warm projects it is the entire SessionStart recall cost.
    """
    if not recall or not UV_BIN:
        return None
    try:
        r = subprocess.run(
            [
                UV_BIN, "run", "--quiet", str(recall),
                query,
                "--limit", "1",
                "--confidence", "HIGH",
                "--format", "json",
                "--max-chars", str(SESSION_START_MAX_CHARS),
                "--tags", ",".join(tags),
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        return None
    if r.returncode != 0 or not r.stdout:
        return None
    try:
        envelope = json.loads(r.stdout)
    except json.JSONDecodeError:
        return None
    results = envelope.get("results") if isinstance(envelope, dict) else None
    if not results:
        return None
    top = results[0]
    return {
        "id": top.get("id"),
        "content": top.get("key_insight") or top.get("title") or top.get("how_to_apply") or "",
        "score": _rerank_score(top),
        "age_days": _age_days(top.get("archived_at")),
        "is_stale": None,
    }


def render_skills_only(skill: dict, query: str) -> str:
    """The skills-only inject payload emitted when we short-circuit.

    Distinct, tagged block so a consumer (and the proof) can tell a
    short-circuited boot from a full lower-tier recall.
    """
    sid = skill.get("id") or "?"
    body = str(skill.get("content") or "").strip()
    return (
        f"## Prior skill relevant to `{query[:80]}`\n"
        f"- **[{sid}]** {body}\n"
    )


def run_lower_tier_recall(query: str, tags: list[str], recall: Path) -> str:
    """The original (lower-tier) recall path: the broad fused GraphRAG+QMD pull.

    Factored out of ``_main_body`` so the short-circuit orchestrator can choose
    NOT to call it — and so the proof can spy on whether it ran. Carries the
    shipped Phase-A enrichments the inline call had: A6 branch-shard pin (via
    the ``RECALL_BRANCH`` the caller sets in the environment), R4/M8 token
    budget (``--max-tokens``), the OOD floor (``--min-overlap``), and the
    synthetic-query suppressors SG6 (``--no-gap-log``) and A4 (``--no-followup``).
    Returns the inject string ("" on any failure — D9 silent-fail).
    """
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
                # SG6: boot-time queries are synthetic — don't log them as gaps.
                "--no-gap-log",
                # A4: nor count a genuine first ask after boot as a followup.
                "--no-followup",
            ],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
            env=dict(os.environ),  # A6: inherit RECALL_BRANCH set by the caller
        )
    except (subprocess.TimeoutExpired, OSError):
        return ""
    if r.returncode != 0:
        return ""
    return (r.stdout or "").strip()


def session_start_recall_payload(query: str, tags: list[str], recall: Path) -> str:
    """Orchestrate R11 short-circuit then (only if needed) the lower tier.

    1. Probe the tier-1 skills hit (ONE lookup).
    2. If it is fresh AND high-score, emit the skills-only payload and STOP —
       the lower-tier recall never runs (zero extra noise/token/latency).
    3. Otherwise fall through to the original lower-tier recall.
    """
    skill = skills_tier_probe(query, tags, recall)
    if should_short_circuit(skill):
        return render_skills_only(skill, query)
    return run_lower_tier_recall(query, tags, recall)


def emit(additional_context: str) -> NoReturn:
    """Always exit 0 with valid JSON.

    Typed NoReturn so callers (and linters) know execution stops here —
    no need for a `return` after `emit(...)` at the call site.

    Output envelope is harness-gated on the ``REFLECT_HARNESS`` env var
    (set by the per-harness adapter on the hook command), NOT on the stdin
    shape — the env is available regardless of whether the cross-harness
    input helper imported successfully, so the decision is robust even in
    the deployed copilot layout where the helper import no-ops.

      * Claude / Codex (default): the canonical
        ``{"hookSpecificOutput": {"hookEventName": ..., "additionalContext": ...}}``
        envelope. Byte-identical to before — unchanged for those harnesses.

      * Copilot (``REFLECT_HARNESS=copilot``): the documented-fallback
        plain ``{"additionalContext": ...}`` shape.

    TODO(copilot-envelope): the exact sessionStart additionalContext
    envelope Copilot expects is docs-silent and could NOT be confirmed
    against the live binary (org-policy-blocked during this work). The
    plain ``{"additionalContext": ...}`` form here is the best-documented
    guess; confirm against ``copilot`` once policy is lifted and adjust if
    it injects nothing. The Claude envelope is also emitted-compatible (an
    unknown extra key is harmless if Copilot ignores it), so this can be
    revisited without breaking claude/codex.
    """
    if os.environ.get("REFLECT_HARNESS") == "copilot":
        print(json.dumps({"additionalContext": additional_context}))
    else:
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
    # Hooks receive JSON on stdin. Claude sets ``CLAUDE_PROJECT_DIR`` so we
    # historically ignored stdin for cwd derivation, but Copilot does NOT
    # set that env — it sends ``cwd`` on stdin (camelCase harness, but the
    # ``cwd`` key itself is shared across all three). Parse it tolerantly
    # and use it only as a fallback so claude/codex behaviour is unchanged.
    raw = ""
    try:
        raw = sys.stdin.read()
    except Exception:
        pass
    try:
        data = json.loads(raw) if raw else {}
    except (json.JSONDecodeError, ValueError):
        data = {}

    env_dir = os.environ.get("CLAUDE_PROJECT_DIR")
    if env_dir:
        cwd = Path(env_dir).resolve()
    else:
        stdin_cwd = get_cwd(data) if isinstance(data, dict) else ""
        cwd = Path(stdin_cwd or os.getcwd()).resolve()

    # Skip for $HOME — no project context there
    if cwd == Path.home():
        emit("")

    # A1: slots are Tier-0 — agent-curated working memory injects BEFORE
    # any recall result. O2: the conventions doc pointer is Tier-1 ambient
    # (1 line: summary + path; nothing when stale). Both are prepended to
    # every emit path below — ambient context never suppresses retrieval.
    ambient_block = join_blocks(
        slot_tier_context(cwd), conventions_tier_context(cwd),
    )

    query, tags = build_query(cwd)
    if not query:
        emit(ambient_block)

    # R10: tiered inject — skills (curated) are the top retrieval tier. A
    # strong skill hit wins outright; the learnings recall below only runs
    # when the skills tier is empty/stale (or the flag is off).
    skill_block = skill_tier_context(query)
    if skill_block:
        emit(join_blocks(ambient_block, skill_block))

    recall = find_recall_script()
    if not recall or not UV_BIN:
        emit(ambient_block)

    # D9: SessionStart must feel instant. Each recall subprocess is 10s-capped
    # (see run_lower_tier_recall / skills_tier_probe) — prefer empty context
    # over a stalled boot. R11: a warm project short-circuits after the single
    # tier-1 skills lookup and never pays for the lower-tier recall.
    #
    # A6: pin the branch shard for THIS worktree before any recall runs, so both
    # the R11 skills probe and the lower-tier recall read the current branch
    # sub-shard (~/.learnings/shards/<project>/branches/<branch>/) and never
    # serve another worktree's learnings. run_lower_tier_recall inherits this
    # via the environment; current_branch() collapses main/master to "".
    os.environ["RECALL_BRANCH"] = current_branch(cwd)
    recall_block = session_start_recall_payload(query, tags, recall)
    # M8: append the one-line token-economics footer (sums the per-row D:/R:
    # numbers recall.py rendered; "" for a short-circuited skills-only block).
    emit(join_blocks(ambient_block, recall_block, economics_footer(recall_block)))


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
