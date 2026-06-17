# ABOUTME: Regression tests for port R14 — per-skill computed staleness flag.
# ABOUTME: Pins: is_stale recomputes on read (true iff any in-scope learning
# ABOUTME: was created/mutated after the skill's last_refreshed_at, OR the
# ABOUTME: stored R13 flag is set) and the computation stays cheap (<10ms
# ABOUTME: for 50 skills over hundreds of learnings).
"""Port R14: per-skill staleness flag (hindsight
``compute_mental_model_is_stale`` shape, computed on read).

Acceptance criteria pinned here:
  1. staleness recomputes correctly on test fixtures
  2. cheap (<10ms) for a project with 50 skills

Plus the design invariants:
  - creation of a NEW in-scope learning counts as an update
  - every mutation path (proof append, status change) counts via the
    learning_history timestamp — no R13 trigger required
  - out-of-scope / no-tag skills never flip from the computed half
  - tag == category (case-insensitive) is in scope; multi-word tags
    must match whole in the title
  - the stored R13 flag is respected (computed OR stored)
  - regenerating the skill (new mtime upsert → fresh last_refreshed_at)
    clears the computed flag
  - tolerant timestamp parsing: bad data never flips a skill
  - get_skills(compute_stale=True) annotates rows without persisting
"""

from __future__ import annotations

import sqlite3
import sys
import time
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parent / "scripts"
sys.path.insert(0, str(SCRIPTS))

import reflect_db  # noqa: E402

OLD = "2026-01-01T00:00:00+00:00"
REFRESH = "2026-02-01T00:00:00+00:00"
NEW = "2026-03-01T00:00:00+00:00"


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB per test; never touches ~/.reflect."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


def _seed_skill(conn, name: str = "publish", *, tags=None,
                refreshed_at: str = REFRESH) -> str:
    """Index one skill row with a pinned last_refreshed_at. Returns the path."""
    path = f"/skills/{name}/SKILL.md"
    reflect_db.upsert_skill(
        name, path,
        tags=tags if tags is not None else ["testflight", "fastlane"],
        summary=f"{name} things.", mtime=1.0, conn=conn,
    )
    _set_refreshed_at(conn, path, refreshed_at)
    return path


def _set_refreshed_at(conn, path: str, ts: str) -> None:
    with conn:
        conn.execute(
            "UPDATE skills SET last_refreshed_at = ? WHERE path = ?", (ts, path),
        )


def _add_learning(conn, title: str, *, category: str = "Unknown",
                  created_at: str = NEW) -> str:
    """A learning whose created_at is pinned (deterministic ordering)."""
    lid = reflect_db.add_learning(title, category=category, conn=conn)
    with conn:
        conn.execute(
            "UPDATE learnings SET created_at = ? WHERE id = ?", (created_at, lid),
        )
        # add_learning's S6/SG1 side-channel rows carry wall-clock stamps;
        # re-pin them so only the timestamps this test controls matter.
        conn.execute(
            "UPDATE learning_history SET created_at = ? WHERE learning_id = ?",
            (created_at, lid),
        )
    return lid


def _set_history_at(conn, lid: str, ts: str) -> None:
    with conn:
        conn.execute(
            "UPDATE learning_history SET created_at = ? WHERE learning_id = ?",
            (ts, lid),
        )


# ── acceptance 1: staleness recomputes correctly on fixtures ────────────────

def test_fresh_skill_no_learnings_not_stale(conn):
    path = _seed_skill(conn)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False


def test_new_in_scope_learning_after_refresh_is_stale(conn):
    """Creation counts as an update — a new in-scope learning stales the skill."""
    path = _seed_skill(conn, tags=["testflight"])
    _add_learning(conn, "TestFlight builds need AD_ID declaration",
                  created_at=NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is True


def test_in_scope_learning_older_than_refresh_not_stale(conn):
    path = _seed_skill(conn, tags=["testflight"])
    _add_learning(conn, "TestFlight builds need AD_ID declaration",
                  created_at=OLD)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False


def test_mutation_after_refresh_flips_stale_without_r13_trigger(conn):
    """An old learning mutated after the refresh flips the computed flag via
    its learning_history timestamp — no event-driven trigger involved."""
    path = _seed_skill(conn, tags=["testflight"])
    lid = _add_learning(conn, "TestFlight builds need AD_ID declaration",
                        created_at=OLD)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False

    reflect_db.add_learning_proof(lid, "transcript-2", conn=conn)
    _set_history_at(conn, lid, NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is True
    # The stored R13 flag was never touched — this is read-time only.
    row = conn.execute(
        "SELECT is_stale FROM skills WHERE path = ?", (path,),
    ).fetchone()
    assert row["is_stale"] == 0


def test_status_change_counts_as_update(conn):
    """Retiring an in-scope learning (status change) also stales the skill."""
    path = _seed_skill(conn, tags=["testflight"])
    lid = _add_learning(conn, "TestFlight builds need AD_ID declaration",
                        created_at=OLD)
    reflect_db.update_learning_status(lid, "reverted",
                                      revert_reason="contradicted", conn=conn)
    _set_history_at(conn, lid, NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is True


def test_out_of_scope_learning_does_not_flip(conn):
    path = _seed_skill(conn, tags=["kubernetes", "helm"])
    _add_learning(conn, "Never use var in TypeScript",
                  category="Code Style", created_at=NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False


def test_category_tag_match_is_in_scope(conn):
    """A skill tag equal to the learning's category (case-insensitive)."""
    path = _seed_skill(conn, "sec", tags=["security"])
    _add_learning(conn, "Always rotate leaked credentials immediately",
                  category="Security", created_at=NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is True


def test_multiword_tag_must_match_whole(conn):
    """'belief revision' must not fire on a title that only says 'revision'."""
    path = _seed_skill(conn, "rev", tags=["belief revision"])
    _add_learning(conn, "Schema revision requires a migration", created_at=NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False

    _add_learning(conn, "Belief revision must snapshot history first",
                  created_at=NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is True


def test_skill_without_tags_has_empty_scope(conn):
    path = _seed_skill(conn, "untagged", tags=[])
    _add_learning(conn, "Untagged matches nothing ever", created_at=NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False


def test_stored_r13_flag_wins_even_without_learnings(conn):
    """Computed staleness is stored-flag OR newer-in-scope-learning."""
    path = _seed_skill(conn)
    reflect_db.mark_skills_stale([path], conn=conn)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is True


def test_refresh_clears_computed_staleness(conn):
    """Regenerating the skill (fresh last_refreshed_at) un-stales it."""
    path = _seed_skill(conn, tags=["testflight"])
    _add_learning(conn, "TestFlight builds need AD_ID declaration",
                  created_at=NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is True

    # New mtime upsert = SKILL.md regenerated; last_refreshed_at moves
    # past the learning's updated_at.
    reflect_db.upsert_skill("publish", path, tags=["testflight"],
                            summary="publish things.", mtime=2.0, conn=conn)
    _set_refreshed_at(conn, path, "2026-04-01T00:00:00+00:00")
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False


def test_unparseable_refreshed_at_falls_back_to_stored_flag(conn):
    """Bad last_refreshed_at: never computed-stale, stored flag still counts."""
    path = _seed_skill(conn, tags=["testflight"], refreshed_at="not-a-date")
    _add_learning(conn, "TestFlight builds need AD_ID declaration",
                  created_at=NEW)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False

    reflect_db.mark_skills_stale([path], conn=conn)
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is True


def test_unparseable_learning_timestamp_never_flips(conn):
    path = _seed_skill(conn, tags=["testflight"])
    lid = _add_learning(conn, "TestFlight builds need AD_ID declaration",
                        created_at=NEW)
    with conn:
        conn.execute(
            "UPDATE learnings SET created_at = 'garbage' WHERE id = ?", (lid,),
        )
        conn.execute(
            "UPDATE learning_history SET created_at = 'garbage' "
            "WHERE learning_id = ?", (lid,),
        )
    assert reflect_db.compute_skill_is_stale(path, conn=conn) is False


def test_compute_skill_is_stale_unknown_path_is_none(conn):
    assert reflect_db.compute_skill_is_stale("/nope/SKILL.md", conn=conn) is None
    assert reflect_db.compute_skill_is_stale("", conn=conn) is None


def test_batch_compute_covers_every_skill(conn):
    stale_path = _seed_skill(conn, "stale-one", tags=["testflight"])
    fresh_path = _seed_skill(conn, "fresh-one", tags=["kubernetes"])
    flagged_path = _seed_skill(conn, "flagged-one", tags=["unrelated"])
    reflect_db.mark_skills_stale([flagged_path], conn=conn)
    _add_learning(conn, "TestFlight builds need AD_ID declaration",
                  created_at=NEW)

    computed = reflect_db.compute_skills_staleness(conn=conn)
    assert computed == {stale_path: True, fresh_path: False, flagged_path: True}


def test_get_skills_compute_stale_annotates_without_persisting(conn):
    stale_path = _seed_skill(conn, "stale-one", tags=["testflight"])
    fresh_path = _seed_skill(conn, "fresh-one", tags=["kubernetes"])
    _add_learning(conn, "TestFlight builds need AD_ID declaration",
                  created_at=NEW)

    by_path = {
        r["path"]: r for r in reflect_db.get_skills(compute_stale=True, conn=conn)
    }
    assert by_path[stale_path]["is_stale"] == 1
    assert by_path[fresh_path]["is_stale"] == 0

    # Default read still returns the stored column (no persistence).
    stored = {r["path"]: r["is_stale"] for r in reflect_db.get_skills(conn=conn)}
    assert stored == {stale_path: 0, fresh_path: 0}


def test_scope_predicate_direct(conn):
    assert reflect_db.skill_scope_matches(
        ["testflight"], "TestFlight builds need AD_ID declaration", "Tools",
    )
    assert reflect_db.skill_scope_matches(
        ["security"], "anything at all", "Security",
    )
    assert not reflect_db.skill_scope_matches(
        ["belief revision"], "Schema revision requires a migration", "",
    )
    assert not reflect_db.skill_scope_matches([], "anything", "Anything")
    assert not reflect_db.skill_scope_matches(["", "  "], "anything", "")


# ── acceptance 2: cheap (<10ms) for a project with 50 skills ────────────────

def test_compute_under_10ms_for_50_skills(conn):
    """50 skills + 300 learnings (all newer than every refresh — the worst
    case: nothing is pruned by the oldest-floor cut). Best-of-5 to dampen
    scheduler noise; the bound is the acceptance criterion itself."""
    for i in range(50):
        _seed_skill(conn, f"skill-{i:02d}",
                    tags=[f"topic{i % 17}", "shared-tag", f"area {i % 7} ops"])
    with conn:
        for j in range(300):
            lid = f"perf{j:012d}"
            conn.execute(
                "INSERT INTO learnings (id, title, category, created_at) "
                "VALUES (?, ?, ?, ?)",
                (
                    lid,
                    f"Learning about topic{j % 23} and area {j % 7} ops "
                    f"with extra words number {j}",
                    f"Category{j % 11}",
                    NEW,
                ),
            )

    best = min(
        _timed(lambda: reflect_db.compute_skills_staleness(conn=conn))
        for _ in range(5)
    )
    assert best < 0.010, f"compute_skills_staleness took {best * 1000:.2f}ms"


def _timed(fn) -> float:
    start = time.perf_counter()
    fn()
    return time.perf_counter() - start


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
