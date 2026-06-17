# ABOUTME: Behavioral proof for R10 — tiered hierarchical inject at SessionStart: the curated
# ABOUTME: skills tier outranks (and suppresses) the raw-learnings tier, gated by REFLECT_TIERED_INJECT.
"""R10 tiered (hierarchical) inject proof.

Invariant (the heart of R10 — ``skill_tier_context`` + the tier ordering in
``_main_body`` of ``session_start_recall.py``): SessionStart retrieval is a
HIERARCHY. The curated skills tier is consulted FIRST; when it has a STRONG
hit (``skill_index.match_skills`` score >= ``REFLECT_SKILL_TIER_MIN_SCORE``,
default 2.0 — at least one name/tag token match) the hook injects the skill
name + one-line summary and ``emit()``s, RETURNING before the raw-learnings
recall ever runs. The lower (learnings) tier runs ONLY when the skills tier is
empty, weak, or stale. The whole tier is behind the opt-in ``REFLECT_TIERED_INJECT``
flag. This ports Hindsight's "mental-models before observations" forced
retrieval order so polished knowledge always precedes — and here outright
suppresses — raw notes covering the same ground.

This is the hook/signal surface, not the recall.py ``behavioral_kb`` fixture
(that fixture is recall.py-only and never runs the SessionStart hook), so the
proof drives the REAL hook exactly the way the harness does — as a subprocess
reading hook JSON on stdin — wired to a fully hermetic world: a tmp HOME, tmp
``REFLECT_DB_PATH`` / ``REFLECT_STATE_DIR`` / ``REFLECT_SKILLS_DIR``, and a fake
``uv`` on PATH standing in for the recall.py learnings tier. The fake uv prints
a single marker line (``LRNFAKE1``); whether that marker appears in the injected
context is the honest, LLM-free observable of WHICH tier answered:

  - skill block present + marker ABSENT  => the skills (higher) tier won and
    suppressed the learnings (lower) tier — the hierarchy fired.
  - skill block absent + marker PRESENT  => the learnings (lower) tier ran —
    the hierarchy did not suppress it.

Nothing here is decided by an LLM: the skill-index tokenizer + score formula
(``2.0 * name/tag-hits + 1.0 * summary-hits``), the documented min-score gate,
the ``is_stale`` prune rule, and the ``REFLECT_TIERED_INJECT`` flag fully
determine each arm. The project dir is named ``webapp-testing`` so the hook's
``build_query`` (project_name = cwd basename, no git remote in the sandbox)
emits the single token ``webapp-testing`` — an exact name-token hit (score 2.0).

Four arms, each in its OWN fresh hermetic world (no cross-arm DB/skills/state
sharing), decide the invariant:

  A. STRONG HIT, knob ON -> skills tier WINS and SUPPRESSES learnings. The
     skill name + summary are injected and the learnings marker is absent: the
     higher tier precedes the lower and skips it entirely.

  B. CONTROL, knob OFF (same skill, same world shape) -> tiering disabled, so
     the skills tier short-circuits to "" and the learnings tier runs: the
     skill block is ABSENT and the learnings marker is PRESENT. The ONLY thing
     that changed from arm A is the documented ``REFLECT_TIERED_INJECT`` flag —
     proving the PORT (the tier), not incidental skill presence, drove arm A's
     ordering/suppression.

  C. WEAK HIT (documented min-score gate), knob ON, ``REFLECT_SKILL_TIER_MIN_SCORE``
     raised above the hit's 2.0 score -> the skills tier finds the skill but
     it falls below the strong-hit threshold, so the hierarchy falls THROUGH to
     learnings: skill block absent, learnings marker present. Proves the tier
     assignment is driven by the documented score rule, not "any skill wins".

  D. STALE SKILL (documented ``is_stale`` prune), knob ON, the indexed skill
     flagged stale via the real ``reflect_db.mark_skills_stale`` -> a stale top
     tier can never win; ``match_skills`` excludes it, so the hierarchy falls
     through to learnings. Proves the prune rule the tier documents.

PORT: R10
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

# Resolve the REAL hook + real scripts dir the same way the deployed layout
# places them (this proof lives at reflect-kb/tests/eval/behavioral/proofs/).
_HERE = Path(__file__).resolve().parent
_REPO_ROOT = _HERE.parents[4]  # proofs/ -> behavioral -> eval -> tests -> reflect-kb -> repo
_PLUGIN = _REPO_ROOT / "plugins" / "reflect"
HOOK = _PLUGIN / "skills" / "recall" / "hooks" / "session_start_recall.py"
SCRIPTS = _PLUGIN / "scripts"
if not HOOK.exists():
    raise RuntimeError(f"session_start_recall.py not found at {HOOK}")

# The fake-uv learnings-tier marker. Distinct from any skill token so its
# presence/absence cleanly distinguishes which tier answered.
LRN_MARKER = "LRNFAKE1"

# A strong name-token hit: project dir basename == skill name == single token
# `webapp-testing` (skill_index's tokenizer does NOT split on hyphens), so the
# query token set is exactly {webapp-testing} and match_skills scores 2.0
# (one name-token hit * 2.0) == the default strong threshold.
SKILL_NAME = "webapp-testing"
SKILL_SUMMARY = "Drive Playwright browser tests for webapps."


def _build_world(tmp: Path) -> dict[str, Path]:
    """Construct a fresh hermetic world: tmp HOME, isolated reflect.db /
    state / skills dir, a project dir named to produce a strong skill hit,
    and a fake-uv bin standing in for the learnings tier."""
    home = tmp / "home"
    skills = tmp / "skills"
    skill_dir = skills / SKILL_NAME
    uvbin = tmp / "uvbin"
    state = tmp / "state"
    proj = tmp / SKILL_NAME  # cwd basename -> query token == skill name
    for d in (home, skill_dir, uvbin, proj):
        d.mkdir(parents=True)

    # Minimal SKILL.md the R20 frontmatter parser understands. name + tags
    # drive the strong-hit score; summary is what the tier injects.
    (skill_dir / "SKILL.md").write_text(
        "---\n"
        f"name: {SKILL_NAME}\n"
        "description: |\n"
        f"  {SKILL_SUMMARY}\n"
        "tags:\n"
        "  - playwright\n"
        "---\n"
        f"# {SKILL_NAME}\n"
        "body\n",
        encoding="utf-8",
    )

    # Fake uv: the hook shells `uv run --quiet recall.py ...` for the learnings
    # tier. This stand-in makes "the lower tier produced results" observable
    # without a knowledge base — it prints one canned learning line.
    uv = uvbin / "uv"
    uv.write_text(f"#!/bin/sh\necho '- prior learning marker {LRN_MARKER} [lrn-fake-1]'\n", encoding="utf-8")
    uv.chmod(0o755)

    return {
        "home": home,
        "db": tmp / "reflect.db",
        "state": state,
        "skills": skills,
        "skill_md": skill_dir / "SKILL.md",
        "uvbin": uvbin,
        "proj": proj,
    }


def _env(world: dict[str, Path], *, tiered: bool, extra: dict[str, str] | None = None) -> dict[str, str]:
    """A deliberately MINIMAL env (not a copy of os.environ) so no real ~/.reflect,
    real uv, or real git can leak in. PATH carries the fake uv plus /usr/bin:/bin
    (git lives there on the runner; with no repo in the sandbox it returns nothing
    and project_name falls back to the cwd basename — the strong-hit token)."""
    env = {
        "PATH": f"{world['uvbin']}:/usr/bin:/bin",
        "HOME": str(world["home"]),
        "REFLECT_DB_PATH": str(world["db"]),
        "REFLECT_STATE_DIR": str(world["state"]),
        "REFLECT_SKILLS_DIR": str(world["skills"]),
        "CLAUDE_PROJECT_DIR": str(world["proj"]),
    }
    if tiered:
        env["REFLECT_TIERED_INJECT"] = "1"
    if extra:
        env.update(extra)
    return env


def _run_hook(env: dict[str, str]) -> str:
    """Run the REAL hook as a subprocess (the harness's invocation shape),
    using THIS interpreter (the 3.11 eval venv python — the hook needs 3.11+
    for the reflect_db import chain). Assert the silent-fail contract (exit 0,
    well-formed SessionStart JSON) and return the injected additionalContext."""
    result = subprocess.run(
        [sys.executable, str(HOOK)],
        input="{}",
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )
    assert result.returncode == 0, (
        f"hook must always exit 0 (D9 silent-fail contract); got {result.returncode}\n"
        f"stdout={result.stdout!r}\nstderr={result.stderr[-1500:]!r}"
    )
    parsed = json.loads(result.stdout)
    out = parsed["hookSpecificOutput"]
    assert out["hookEventName"] == "SessionStart"
    return out["additionalContext"]


def _index_skill(world: dict[str, Path], *, mark_stale: bool = False) -> None:
    """Drive the REAL skill_index + reflect_db to populate the hermetic skills
    table (the same modules the hook lazily imports), optionally flagging the
    skill stale via the real ``mark_skills_stale``. Runs in a child process so
    the on-disk sqlite db is shared with the hook subprocess but the import of
    the production modules stays out of the test interpreter."""
    code = (
        "import sys; sys.path.insert(0, %r)\n"
        "import reflect_db, skill_index\n"
        "conn = reflect_db.get_conn()\n"
        "skill_index.refresh_if_stale(conn=conn)\n"
    ) % str(SCRIPTS)
    if mark_stale:
        code += "reflect_db.mark_skills_stale([%r], conn=conn)\n" % str(world["skill_md"])
    r = subprocess.run(
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        env=_env(world, tiered=False),  # env only carries DB/skills paths here
        timeout=60,
    )
    assert r.returncode == 0, f"skill indexing failed:\nstderr={r.stderr[-1500:]!r}"


# --- Arm A: strong hit, knob ON -> skills tier WINS, suppresses learnings ----

def test_R10_strong_skill_hit_wins_and_suppresses_learnings(tmp_path):
    world = _build_world(tmp_path)
    _index_skill(world)

    ctx = _run_hook(_env(world, tiered=True))

    assert SKILL_NAME in ctx, f"skill name missing — skills tier did not fire: {ctx!r}"
    assert SKILL_SUMMARY in ctx, f"skill one-line summary missing from inject: {ctx!r}"
    assert LRN_MARKER not in ctx, (
        "the raw-learnings (lower) tier leaked into a skills-tier win — the "
        f"hierarchy did NOT suppress the lower tier: {ctx!r}"
    )
    # The injected block is the compact router (name + summary), not the body,
    # and the curated-prefers-raw header marks it as the top tier.
    assert "curated" in ctx, f"skills-tier header missing: {ctx!r}"


# --- Arm B: control, knob OFF -> learnings tier runs (ordering CHANGES) -------

def test_R10_knob_off_falls_through_to_learnings(tmp_path):
    world = _build_world(tmp_path)
    _index_skill(world)

    ctx = _run_hook(_env(world, tiered=False))

    assert LRN_MARKER in ctx, (
        "with REFLECT_TIERED_INJECT off, the learnings (lower) tier must run; "
        f"its marker is missing: {ctx!r}"
    )
    assert SKILL_NAME not in ctx, (
        "the skills tier must NOT inject when the opt-in flag is off — only the "
        f"flag changed from arm A, so the flag (the PORT) drove arm A's suppression: {ctx!r}"
    )


# --- Arm C: weak hit (documented min-score gate) -> falls through ------------

def test_R10_weak_hit_below_threshold_falls_through(tmp_path):
    world = _build_world(tmp_path)
    _index_skill(world)

    # The hit scores exactly 2.0; raise the documented strong-hit threshold
    # above it so it no longer qualifies as a strong hit.
    ctx = _run_hook(
        _env(world, tiered=True, extra={"REFLECT_SKILL_TIER_MIN_SCORE": "2.5"})
    )

    assert SKILL_NAME not in ctx, (
        "a hit below REFLECT_SKILL_TIER_MIN_SCORE must NOT win the skills tier — "
        f"the documented score gate decides tier assignment: {ctx!r}"
    )
    assert LRN_MARKER in ctx, (
        "a weak (below-threshold) skills tier must fall through to the learnings "
        f"tier; its marker is missing: {ctx!r}"
    )


# --- Arm D: stale skill (documented is_stale prune) -> falls through ---------

def test_R10_stale_skill_is_pruned_and_falls_through(tmp_path):
    world = _build_world(tmp_path)
    _index_skill(world, mark_stale=True)

    ctx = _run_hook(_env(world, tiered=True))

    assert SKILL_NAME not in ctx, (
        "a stale skill (is_stale=1) must be pruned by match_skills and can never "
        f"win the inject tier — a wrong/outdated skill is worse than none: {ctx!r}"
    )
    assert LRN_MARKER in ctx, (
        "a stale top tier must fall through to the learnings tier; its marker is "
        f"missing: {ctx!r}"
    )
