# ABOUTME: Behavioral proof for R20 — the installed-skills index (reflect.db skills table).
# ABOUTME: The REAL builder (skill_index.rebuild_index) indexes the on-disk skill set and the
# ABOUTME: REAL lookup (skill_index.match_skills) returns the expected skill, never an absent one.
"""R20 skills-index query proof.

Invariant (the heart of R20): a queryable index of installed skills is built
in ``reflect.db`` from the actual ``SKILL.md`` files on disk — one row per
skill carrying ``name / path / tags / summary / mtime`` — and a token query
against that index returns the skill that owns the matching trigger/tag while
NEVER returning a skill that is not present in the index. This replaces the
old "scan + frontmatter-parse every SKILL.md on every query" path with a
single sqlite lookup.

This port has NO LLM and NO embedding engine anywhere in its surface: the
builder is a stdlib filesystem scan + frontmatter parse, the store is sqlite,
and ``match_skills`` is deterministic token-overlap ranking
(``score = 2·|q ∩ strong| + 1·|q ∩ weak|``). So the proof drives the REAL
``skill_index`` + ``reflect_db`` modules directly against a hermetic tmp DB
and a hermetic tmp skills directory built from a real skill set — the seeds
plus the documented ranking fully determine every assertion. There is nothing
for an LLM to decide.

Three arms, each with its OWN fresh DB + skills dir (no cross-arm state):

  PRESENCE   — rebuild the index from a two-skill on-disk set, then query the
               trigger of one skill. That skill's row comes back; the index
               actually holds both skills (built from the real file set).

  ABSENCE    — against the SAME built index, a query naming a topic/skill that
               was never installed returns the empty list. A skill absent from
               the index is never fabricated into a result.

  PRUNE      — the decisive control proving the match is driven by the BUILT
               index, not by static query/text overlap: a query matches a skill
               while it is installed, then — after that SKILL.md is removed from
               disk and the index is rebuilt (which prunes the row) — the SAME
               query returns nothing. If ``match_skills`` were matching the
               query against anything other than the freshly-built on-disk set,
               the pruned skill would still surface. It does not.

PORT: R20
"""
from __future__ import annotations

import shutil
import sys
from pathlib import Path

import pytest

# Resolve the REAL reflect scripts (skill_index + reflect_db) the same way the
# behavioral conftest resolves recall.py: EVAL_ROOT is reflect-kb/tests/eval, so
# parents[2] is the repo root where plugins/ lives alongside reflect-kb/, and
# parents[1].parent covers a standalone reflect-kb checkout with the plugin as a
# sibling dir.
_EVAL_ROOT = Path(__file__).resolve().parents[2]  # reflect-kb/tests/eval
_SCRIPT_CANDIDATES = [
    _EVAL_ROOT.parents[2] / "plugins" / "reflect" / "scripts",
    _EVAL_ROOT.parents[1].parent / "plugins" / "reflect" / "scripts",
]
_SCRIPTS = next((p for p in _SCRIPT_CANDIDATES if (p / "skill_index.py").exists()), None)
if _SCRIPTS is None:
    raise RuntimeError(
        f"skill_index.py not found; tried: {[str(p) for p in _SCRIPT_CANDIDATES]}"
    )
sys.path.insert(0, str(_SCRIPTS))

import reflect_db  # noqa: E402
import skill_index  # noqa: E402


def _write_skill(
    base: Path,
    dirname: str,
    *,
    name: str,
    description: str,
    triggers: list[str] | None = None,
    tags: list[str] | None = None,
) -> Path:
    """Write a realistic SKILL.md (block-scalar description + list triggers/tags).

    Mirrors the on-disk shape the real builder parses; the fixture handles the
    frontmatter, so dates/structure are literal and the parse is deterministic.
    """
    skill_dir = base / dirname
    skill_dir.mkdir(parents=True)
    lines = ["---", f"name: {name}", "description: |", f"  {description}", 'version: "1.0.0"']
    if tags:
        lines.append("tags:")
        lines.extend(f"  - {t}" for t in tags)
    if triggers:
        lines.append("triggers:")
        lines.extend(f"  - {t}" for t in triggers)
    lines.extend(["---", "", f"# {dirname}", "body"])
    path = skill_dir / "SKILL.md"
    path.write_text("\n".join(lines), encoding="utf-8")
    return path


@pytest.fixture
def fresh_db_and_skills(tmp_path: Path):
    """A hermetic tmp reflect.db + empty tmp skills dir, isolated per arm.

    Each arm gets its OWN DB + skills directory (never touches ~/.reflect or
    ~/.claude/skills) so no cross-arm state can contaminate the index. The
    connection is closed on teardown to drop the process-global cache.
    """
    db_file = tmp_path / "reflect.db"
    conn = reflect_db.init_db(db_file)
    skills = tmp_path / "skills"
    skills.mkdir()
    try:
        yield conn, skills
    finally:
        reflect_db.close_all()


# Two real skills, written to disk, that the builder will index.
def _seed_two_skills(skills: Path) -> None:
    _write_skill(
        skills,
        "postgres-deadlock",
        name="reflect:deadlock-doctor",
        description="Diagnose a Postgres deadlock from pg_locks lock-wait cycles.",
        triggers=["postgres deadlock", "lock-wait cycle"],
    )
    _write_skill(
        skills,
        "tmux-monitor",
        name="tmux-monitor",
        description="Watch tmux sessions for agent activity.",
        tags=["tmux", "monitoring"],
    )


def test_R20_index_built_from_disk_returns_present_skill(fresh_db_and_skills):
    """PRESENCE: the real builder indexes the on-disk skill set, and a query on
    a present skill's trigger returns that skill's row."""
    conn, skills = fresh_db_and_skills
    _seed_two_skills(skills)

    summary = skill_index.rebuild_index(skills, conn=conn)
    assert summary["indexed"] == 2, f"expected both on-disk skills indexed, got {summary}"

    # The index is genuinely built from the real file set: both rows present,
    # each carrying its path (the natural key) pointing at the real SKILL.md.
    rows = {r["name"]: r for r in reflect_db.get_skills(conn=conn)}
    assert set(rows) == {"reflect:deadlock-doctor", "tmux-monitor"}, (
        f"index must hold exactly the two on-disk skills, got {sorted(rows)}"
    )
    assert rows["reflect:deadlock-doctor"]["path"] == str(
        skills / "postgres-deadlock" / "SKILL.md"
    )

    # A query naming the deadlock-doctor's trigger resolves to exactly that
    # skill via the real lookup — the deterministic token-overlap ranking.
    matches = skill_index.match_skills(
        "how to diagnose a postgres deadlock from a lock-wait cycle", conn=conn
    )
    assert [m["name"] for m in matches] == ["reflect:deadlock-doctor"], (
        "the query naming the deadlock skill's trigger must return exactly that "
        f"skill from the built index; got {[m['name'] for m in matches]}"
    )
    assert matches[0]["score"] > 0


def test_R20_absent_skill_is_not_returned(fresh_db_and_skills):
    """ABSENCE: against the same built index, a query for a topic/skill that was
    never installed returns nothing — the index does not fabricate a match."""
    conn, skills = fresh_db_and_skills
    _seed_two_skills(skills)
    skill_index.rebuild_index(skills, conn=conn)

    # An off-corpus topic that no indexed skill names: zero matches.
    assert skill_index.match_skills(
        "rotate kubernetes ingress tls certificates", conn=conn
    ) == [], "a query for an un-indexed topic must return no skills"

    # A query naming a skill that is simply not in the index: zero matches.
    assert skill_index.match_skills(
        "frobnicate the quux widget pipeline", conn=conn
    ) == [], "a skill absent from the index must never be returned"


def test_R20_prune_removes_skill_from_query_results(fresh_db_and_skills):
    """PRUNE (decisive control): the match tracks the BUILT index, not static
    query/text overlap. A query matches a skill while it is installed; after the
    SKILL.md is removed from disk and the index rebuilt (pruning the row), the
    SAME query returns nothing."""
    conn, skills = fresh_db_and_skills
    _seed_two_skills(skills)
    skill_index.rebuild_index(skills, conn=conn)

    query = "postgres deadlock help"
    before = [m["name"] for m in skill_index.match_skills(query, conn=conn)]
    assert before == ["reflect:deadlock-doctor"], (
        f"while installed, the query must match the deadlock skill; got {before}"
    )

    # Uninstall the skill on disk, then rebuild from the new (smaller) file set.
    shutil.rmtree(skills / "postgres-deadlock")
    summary = skill_index.rebuild_index(skills, conn=conn)
    assert summary["removed"] == 1, f"rebuild must prune the uninstalled skill, got {summary}"
    assert {r["name"] for r in reflect_db.get_skills(conn=conn)} == {"tmux-monitor"}

    # SAME query, now matches nothing: the result was driven by the on-disk
    # index, not by the (unchanged) query text.
    after = [m["name"] for m in skill_index.match_skills(query, conn=conn)]
    assert after == [], (
        "after the skill is uninstalled and the index rebuilt, the same query "
        f"must return no skills (matches track the built index); got {after}"
    )
