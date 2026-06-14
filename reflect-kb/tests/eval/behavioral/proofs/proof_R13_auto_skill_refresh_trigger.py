# ABOUTME: Behavioral proof for R13 — auto-skill-refresh trigger on belief revision.
# ABOUTME: Drives the REAL reflect_cascade.execute_revision_actions + reflect_db skills
# ABOUTME: store + skill_index.match_skills: an UPDATE/DELETE landing on a learning whose
# ABOUTME: tokens/category overlap a skill's tags marks that skill is_stale (excluded from
# ABOUTME: inject) and queues ONE dedup'd skill_refresh task; an mtime-changing refresh clears it.
"""R13 auto-skill-refresh trigger proof.

Port R13 (bead agents-in-a-box-kdo.39) is a STORAGE/SIGNAL port, not a
file-engine recall port. Its behaviour lives entirely in
``plugins/reflect/scripts/`` (``reflect_cascade.py``, ``reflect_db.py``,
``skill_index.py``) — so this proof drives those real modules against an
on-disk ``reflect_db`` sqlite store directly. No LLM, no torch model, no vector
engine is involved: the seeds plus the literal revision-action objects fully
determine every asserted outcome. (In production the drain's LLM only *chooses*
which CREATE/UPDATE/DELETE action to emit; here we hand the executor the actions
verbatim, so the assertions test the executor's deterministic state transitions
and the deterministic stdlib token-overlap match, never an LLM decision.)

The TRUE invariant (corrected against the real diff at db2fb687 —
``feat(reflect): auto-flag and refresh skills when backing learnings change``):

When ``execute_revision_actions`` applies an UPDATE or DELETE to a learning, it
back-reacts on the promoted-skills index via ``trigger_skill_refreshes``:

  STALENESS CONDITION (the trigger predicate, deterministic, stdlib-only):
    a skill is "backed by" the revised learning when one of its tags either
    EQUALS the learning's category (case-insensitive) OR has ALL its content
    tokens present in the learning's title. ``skills_backing_learning`` computes
    this with ``_content_tokens`` (lowercased, stopwords dropped).

  WHEN THE CONDITION HOLDS (refresh fires):
    * every backing skill is flagged ``is_stale = 1`` in reflect.db
      (``mark_skills_stale``), and
    * exactly one ``skill_refresh`` task per skill is appended to
      ``pending_reflections.jsonl`` (``enqueue_skill_refresh``), and
    * the summary reports ``skills_marked_stale`` / ``refreshes_queued`` counts.

  WHEN THE CONDITION DOES NOT HOLD (refresh skipped — the control):
    * a skill with NO tag overlap and NO category match is left ``is_stale = 0``
      and NO ``skill_refresh`` task is queued for it.

  DOWNSTREAM OBSERVABLE (why staleness matters):
    ``skill_index.match_skills`` excludes ``is_stale`` rows — so a skill that
    matched a query BEFORE the revision stops matching AFTER it (it can no
    longer win the R11 inject tier with possibly-drifted guidance), while a
    non-stale control skill that matches the same query keeps matching.

  CLEARING (the refresh completing):
    re-``upsert_skill`` with a NEW mtime (the SKILL.md was actually
    regenerated/edited) clears ``is_stale``; re-upserting the SAME mtime (a
    blind rebuild pass) PRESERVES the flag — so a stale skill stays stale until
    its content really changes.

  DEDUP:
    a second revision touching the same skill does NOT queue a second
    ``skill_refresh`` task (``_skill_refresh_already_queued``).

Why no LLM: the staleness predicate is pure stdlib token overlap; the queue
file is a literal JSONL append; ``match_skills`` is deterministic token scoring;
the clearing rule is a CASE on mtime equality. The seeds (learning title +
category, skill tags + mtime) and the literal action objects fully determine the
``is_stale`` column, the queued tasks, the match results, and the clear/preserve
outcome. Nothing asserted here was decided by a model.

Falsifiability: if the trigger never fired, Arm 1's in-scope skill would stay
``is_stale = 0`` and queue nothing. If the predicate ignored tag/category scope,
Arm 1's out-of-scope control skill would ALSO be flagged. If ``match_skills``
did not exclude stale rows, Arm 2's post-revision match would still return the
flagged skill. If ``upsert_skill`` cleared on every upsert, Arm 3's same-mtime
re-upsert would clear the flag. If dedup were absent, Arm 4 would queue two
tasks. Each arm seeds its OWN fresh DB (no cross-arm state sharing).

PORT: R13
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the S5 storage proof does so this runs from either checkout layout.
_CONFTEST_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _CONFTEST_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _CONFTEST_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402
import skill_index  # noqa: E402


@pytest.fixture
def db(tmp_path, monkeypatch):
    """Fresh isolated on-disk reflect DB wired as the MODULE-DEFAULT connection.

    reflect_cascade's executor, the R13 trigger, and skill_index.match_skills
    all call reflect_db helpers WITHOUT a conn= argument (production shape), so
    they resolve via reflect_db.get_conn. Pointing get_conn at this sandbox
    makes the real modules drive THIS db, not the developer's ~/.reflect.

    REFLECT_STATE_DIR is redirected too: enqueue_skill_refresh writes the
    skill_refresh task to ``<state>/pending_reflections.jsonl``, so the queue
    lands inside the per-test tmp tree, never ~/.reflect.
    """
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path / "state"))
    yield connection
    reflect_db.close_all()


def _skill_row(conn, path: str):
    return conn.execute("SELECT * FROM skills WHERE path = ?", (path,)).fetchone()


def _seed_skill(conn, tmp_path: Path, *, name: str, tags: list[str],
                summary: str = "", mtime: float = 1000.0) -> str:
    """Index one skill row directly (the natural key is its path)."""
    path = str(tmp_path / "skills" / name / "SKILL.md")
    reflect_db.upsert_skill(
        name, path, tags=tags, summary=summary or f"The {name} skill.",
        mtime=mtime, conn=conn,
    )
    return path


def _queue_tasks(tmp_path: Path) -> list[dict]:
    qfile = tmp_path / "state" / "pending_reflections.jsonl"
    if not qfile.exists():
        return []
    return [
        json.loads(line)
        for line in qfile.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def _refresh_tasks_for(tmp_path: Path, skill_path: str) -> list[dict]:
    return [
        t for t in _queue_tasks(tmp_path)
        if t.get("trigger") == "skill_refresh"
        and t.get("transcript_path") == skill_path
    ]


# ── Arm 1: trigger fires for in-scope skill, SKIPS the out-of-scope control ──

def test_R13_update_marks_in_scope_skill_stale_and_skips_control(db, tmp_path):
    """The decisive knob: condition-holds fires, condition-absent skips.

    One revised learning, TWO seeded skills:
      * IN-SCOPE   — a tag whose tokens are all in the learning's title.
      * OUT-OF-SCOPE — unrelated tags, no category match.
    The trigger must flag + queue the first and leave the second untouched.
    """
    conn = db

    # A learning whose TITLE tokens back the in-scope skill's "testflight" tag,
    # and whose CATEGORY is "tools".
    lid = reflect_db.add_learning(
        title="TestFlight builds need an AD_ID declaration before upload",
        category="tools",
        confidence="high",
        scope="project",
        source_memory_ids=["transcript-A"],
        conn=conn,
    )

    in_scope = _seed_skill(
        conn, tmp_path, name="publish", tags=["testflight", "fastlane"],
    )
    out_of_scope = _seed_skill(
        conn, tmp_path, name="dbmigrate", tags=["postgres", "alembic"],
    )

    # Both skills start NOT stale (control on the seed state).
    assert _skill_row(conn, in_scope)["is_stale"] == 0
    assert _skill_row(conn, out_of_scope)["is_stale"] == 0

    # ---- ingest an UPDATE of the learning (a NEW source proves it). ----
    summary = reflect_cascade.execute_revision_actions(
        [{
            "action": "UPDATE",
            "target_id": lid,
            "reason": "session restated the TestFlight AD_ID rule",
        }],
        source_memory_id="transcript-B",
    )

    assert summary["updated"] == 1 and summary["errors"] == [], (
        f"the UPDATE must merge as evidence; got {summary}"
    )

    # THE TRIGGER FIRED for the in-scope skill: flagged + counted + queued.
    assert summary["skills_marked_stale"] == 1, (
        "exactly the one in-scope skill must be flagged stale; "
        f"got {summary['skills_marked_stale']}"
    )
    assert summary["refreshes_queued"] == 1, (
        f"exactly one skill_refresh task must be queued; got {summary['refreshes_queued']}"
    )
    assert _skill_row(conn, in_scope)["is_stale"] == 1, (
        "the skill whose tag tokens back the revised learning MUST be flagged "
        "is_stale — this is the auto-refresh trigger firing"
    )

    # THE CONTROL DID NOT FIRE: out-of-scope skill is untouched.
    assert _skill_row(conn, out_of_scope)["is_stale"] == 0, (
        "a skill with no tag/category overlap must NOT be flagged — the trigger "
        "is scoped to backing skills, not every skill in the index"
    )

    # The queue carries exactly one refresh task, and it targets the in-scope
    # skill's SKILL.md path (the field the drain keys all its mechanics on).
    in_tasks = _refresh_tasks_for(tmp_path, in_scope)
    out_tasks = _refresh_tasks_for(tmp_path, out_of_scope)
    assert len(in_tasks) == 1, (
        f"one skill_refresh task for the in-scope skill; got {in_tasks}"
    )
    assert out_tasks == [], (
        f"no skill_refresh task for the out-of-scope control; got {out_tasks}"
    )
    assert in_tasks[0]["learning_id"] == lid, (
        "the queued task must carry the revised learning's id as provenance"
    )


# ── Arm 2: a stale skill drops out of the inject matcher; control still matches ─

def test_R13_stale_skill_excluded_from_match_control_still_matches(db, tmp_path):
    """Downstream observable: staleness removes a skill from match_skills.

    Same query matches BOTH skills before the revision. After the revision
    flags one stale, that one stops matching while the control survives —
    proving the exclusion is staleness-driven, not a query-wide drop.
    """
    conn = db

    lid = reflect_db.add_learning(
        title="A deploy rollback must drain connections before the swap",
        category="ops",
        confidence="high",
        scope="project",
        source_memory_ids=["t-A"],
        conn=conn,
    )

    # Both skills match the query "deploy rollback". The backed skill matches
    # via its "rollback" tag (whose token IS in the learning title, so it is in
    # the learning's scope). The control matches the query via its NAME
    # ("rollback-helper") but its TAGS ("canary", "bluegreen") have NO token in
    # the learning title and no category match — so the trigger must skip it.
    backed = _seed_skill(
        conn, tmp_path, name="deployer", tags=["rollback", "kubernetes"],
    )
    control = _seed_skill(
        conn, tmp_path, name="rollback helper", tags=["canary", "bluegreen"],
    )

    # BEFORE the revision both are matchable for a deploy/rollback query.
    pre = {r["path"] for r in skill_index.match_skills("deploy rollback", conn=conn)}
    assert backed in pre and control in pre, (
        f"both skills should match the query before any revision; got {pre}"
    )

    # ---- revise the learning the first skill is built on. ----
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid, "reason": "restated rollback drain rule"}],
        source_memory_id="t-B",
    )
    assert summary["updated"] == 1
    assert _skill_row(conn, backed)["is_stale"] == 1, "backed skill must be flagged"
    assert _skill_row(conn, control)["is_stale"] == 0, "control must stay fresh"

    # AFTER: the stale skill is excluded from matching; the control survives.
    post = {r["path"] for r in skill_index.match_skills("deploy rollback", conn=conn)}
    assert backed not in post, (
        "a stale skill MUST NOT win the inject tier — match_skills excludes "
        f"is_stale rows so drifted guidance is never injected; got {post}"
    )
    assert control in post, (
        "the non-stale control matching the same query must still match — the "
        f"exclusion is staleness-driven, not a query-wide drop; got {post}"
    )


# ── Arm 3: DELETE triggers too; mtime-changing refresh clears, same-mtime keeps ─

def test_R13_delete_triggers_then_refresh_clears_only_on_mtime_change(db, tmp_path):
    """DELETE (retire) fires the trigger; clearing is gated on a real refresh.

    A blind re-index (same mtime) must NOT clear staleness — only an
    mtime-changing regeneration (the SKILL.md was actually rewritten) does.
    """
    conn = db

    lid = reflect_db.add_learning(
        title="Always disable the legacy cache header in prod",
        category="config",
        confidence="medium",
        scope="project",
        conn=conn,
    )
    skill = _seed_skill(
        conn, tmp_path, name="cache-config", tags=["cache", "header"], mtime=2000.0,
    )
    assert _skill_row(conn, skill)["is_stale"] == 0

    # ---- DELETE (retire) the backing learning. ----
    summary = reflect_cascade.execute_revision_actions(
        [{
            "action": "DELETE",
            "target_id": lid,
            "reason": "superseded: prod now ENABLES the cache header",
        }],
    )
    assert summary["deleted"] == 1 and summary["errors"] == [], (
        f"DELETE must retire exactly one learning; got {summary}"
    )
    # DELETE fires the trigger exactly like UPDATE does.
    assert summary["skills_marked_stale"] == 1 and summary["refreshes_queued"] == 1, (
        f"retiring a backing learning must flag + queue its skill; got {summary}"
    )
    assert _skill_row(conn, skill)["is_stale"] == 1, (
        "a DELETE/retire on a backing learning must flag the skill stale"
    )

    # CLEARING CONTROL — blind re-upsert with the SAME mtime preserves staleness
    # (a full rebuild_index pass must not clear a flag set by belief revision).
    reflect_db.upsert_skill(
        "cache-config", skill, tags=["cache", "header"],
        summary="The cache-config skill.", mtime=2000.0, conn=conn,
    )
    assert _skill_row(conn, skill)["is_stale"] == 1, (
        "re-upserting the SAME mtime (a blind reindex) must PRESERVE the flag — "
        "the skill content did not actually change"
    )

    # CLEARING — re-upsert with a NEW mtime (the SKILL.md was regenerated)
    # clears the flag: the refresh completed.
    reflect_db.upsert_skill(
        "cache-config", skill, tags=["cache", "header"],
        summary="The cache-config skill (regenerated).", mtime=2001.0, conn=conn,
    )
    assert _skill_row(conn, skill)["is_stale"] == 0, (
        "an mtime-changing upsert (real regeneration) MUST clear is_stale — the "
        "skill caught up with the revised corpus"
    )


# ── Arm 4: queue dedup — a second revision does not enqueue a second task ─────

def test_R13_refresh_task_is_deduplicated_per_skill_path(db, tmp_path):
    """At most one pending skill_refresh task per skill path.

    Two separate revisions both back the same skill. The first queues a task;
    the second must NOT queue a duplicate (the drain keys retry/poison
    mechanics on transcript_path, so a duplicate would double-process).
    """
    conn = db

    skill = _seed_skill(
        conn, tmp_path, name="linter", tags=["ruff", "lint"],
    )

    lid1 = reflect_db.add_learning(
        title="Run ruff before committing python",
        category="tooling",
        source_memory_ids=["s1"],
        conn=conn,
    )
    lid2 = reflect_db.add_learning(
        title="Configure ruff line-length to 100",
        category="tooling",
        source_memory_ids=["s2"],
        conn=conn,
    )

    s1 = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid1, "reason": "restated ruff rule"}],
        source_memory_id="s1-new",
    )
    assert s1["updated"] == 1 and s1["refreshes_queued"] == 1, (
        f"first revision must queue the refresh; got {s1}"
    )

    s2 = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid2, "reason": "restated ruff config"}],
        source_memory_id="s2-new",
    )
    # The skill is ALREADY stale and ALREADY queued: the second revision must
    # flag nothing new (mark_skills_stale only counts 0->1 transitions) and
    # queue nothing new (dedup on the skill's path).
    assert s2["updated"] == 1, f"second revision must still apply; got {s2}"
    assert s2["refreshes_queued"] == 0, (
        "a second revision on the same skill must NOT queue a duplicate "
        f"skill_refresh task; got {s2['refreshes_queued']}"
    )

    tasks = _refresh_tasks_for(tmp_path, skill)
    assert len(tasks) == 1, (
        f"exactly one pending skill_refresh task for the skill path; got {tasks}"
    )
