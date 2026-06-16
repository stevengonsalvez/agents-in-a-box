# ABOUTME: Regression tests for port SG1 — cross-turn contradiction detection.
# ABOUTME: Pins the negation-stripped Jaccard detector, the add_learning
# ABOUTME: post-write hook (older learning loses is_latest), the audit trail
# ABOUTME: (sqlite event + events.jsonl mirror + S6 history snapshot), the
# ABOUTME: schema migration/backfill, and the reflect-status surfacing.
"""Port SG1: new learning writes demote recent contradicted learnings.

Acceptance criteria pinned here:
  1. second save of 'never use foo' against an existing 'use foo'
     learning flips the older one's is_latest
  2. contradiction event written to events.jsonl (beside the DB —
     ~/.reflect/events.jsonl in production) AND the sqlite events table
  3. status skill shows contradiction count
"""

from __future__ import annotations

import json
import os
import sqlite3
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
STATUS_SKILL = PLUGIN_ROOT / "skills" / "reflect-status" / "SKILL.md"
sys.path.insert(0, str(SCRIPTS))

import contradiction_detector  # noqa: E402
import reflect_db  # noqa: E402


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB per test, wired as the module default connection."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


def _row(conn, lid):
    return reflect_db.get_learning(lid, conn=conn)


def _events_jsonl(tmp_path: Path) -> list[dict]:
    path = tmp_path / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


# ── detector: negation polarity + negation-stripped Jaccard ─────────────────

def test_detects_pure_negation_flip():
    assert contradiction_detector.detect_contradiction("use foo", "never use foo") == 1.0


def test_detects_dont_contraction():
    assert contradiction_detector.detect_contradiction(
        "don't commit straight to main", "commit straight to main"
    ) == 1.0


def test_detects_not_marker():
    assert contradiction_detector.detect_contradiction(
        "do not deploy on fridays", "deploy on fridays"
    ) == 1.0


def test_same_polarity_is_not_a_contradiction():
    # Both negated → restatement, not contradiction.
    assert contradiction_detector.detect_contradiction(
        "never use foo", "don't use foo"
    ) is None
    # Both positive → duplicate territory (C1's job), not contradiction.
    assert contradiction_detector.detect_contradiction("use foo", "use foo") is None


def test_low_overlap_is_not_a_contradiction():
    assert contradiction_detector.detect_contradiction(
        "never use foo", "use bar baz instead of qux"
    ) is None


def test_threshold_is_strictly_greater_than():
    # Identical stripped sets score exactly 1.0 > 0.9; a threshold of 1.0
    # must therefore reject them (agentmemory's `sim > threshold` shape).
    assert contradiction_detector.detect_contradiction(
        "use foo", "never use foo", threshold=1.0
    ) is None


def test_vacuous_text_is_never_a_contradiction():
    assert contradiction_detector.detect_contradiction("never", "") is None
    assert contradiction_detector.detect_contradiction("not the and of", "use foo") is None


def test_concepts_exclude_negation_and_stopwords():
    concepts = contradiction_detector.extract_concepts("Never use foo in the build")
    assert "never" not in concepts
    assert "the" not in concepts
    assert {"use", "foo", "build"} <= concepts
    # Negated and positive forms of the same rule share concept buckets.
    assert concepts == contradiction_detector.extract_concepts("use foo in the build")


def test_has_negation_variants():
    assert contradiction_detector.has_negation("never use foo")
    assert contradiction_detector.has_negation("don't use foo")
    assert contradiction_detector.has_negation("DO NOT use foo")
    assert contradiction_detector.has_negation("you shouldn't use foo")
    assert not contradiction_detector.has_negation("use foo")
    # Bare "no" / "avoid" deliberately not markers (false-positive control).
    assert not contradiction_detector.has_negation("there is no place like home")


# ── acceptance 1: second save flips the older learning's is_latest ──────────

def test_negated_save_flips_older_is_latest(conn):
    old = reflect_db.add_learning("use foo", conn=conn)
    assert _row(conn, old)["is_latest"] == 1

    new = reflect_db.add_learning("never use foo", conn=conn)

    old_row = _row(conn, old)
    assert old_row["is_latest"] == 0
    assert old_row["superseded_by_learning_id"] == new
    # The new learning stays latest and untouched.
    new_row = _row(conn, new)
    assert new_row["is_latest"] == 1
    assert new_row["superseded_by_learning_id"] is None


def test_positive_save_flips_older_negated_rule(conn):
    # Direction-agnostic: latest write wins regardless of which side negates.
    old = reflect_db.add_learning("never use foo", conn=conn)
    new = reflect_db.add_learning("use foo", conn=conn)
    assert _row(conn, old)["is_latest"] == 0
    assert _row(conn, old)["superseded_by_learning_id"] == new


def test_unrelated_and_low_overlap_writes_do_not_flip(conn):
    a = reflect_db.add_learning("use foo", conn=conn)
    reflect_db.add_learning("never use bar", conn=conn)                 # different rule
    reflect_db.add_learning("never use foo in production builds", conn=conn)  # < 0.9 overlap
    assert _row(conn, a)["is_latest"] == 1


def test_same_polarity_duplicate_does_not_flip(conn):
    a = reflect_db.add_learning("never use foo", conn=conn)
    reflect_db.add_learning("never use foo", conn=conn)
    assert _row(conn, a)["is_latest"] == 1


def test_scope_isolation(conn):
    a = reflect_db.add_learning("use foo", scope="global", conn=conn)
    reflect_db.add_learning("never use foo", scope="project", conn=conn)
    assert _row(conn, a)["is_latest"] == 1  # out-of-scope: untouched


def test_retired_learning_is_not_a_candidate(conn):
    a = reflect_db.add_learning("use foo", conn=conn)
    reflect_db.update_learning_status(a, "reverted", revert_reason="stale", conn=conn)
    reflect_db.add_learning("never use foo", conn=conn)
    assert _row(conn, a)["is_latest"] == 1  # already retired; no double demotion


def test_demoted_learning_never_resurfaces_as_candidate(conn):
    a = reflect_db.add_learning("use foo", conn=conn)
    b = reflect_db.add_learning("never use foo", conn=conn)
    assert _row(conn, a)["is_latest"] == 0
    # A third flip targets the CURRENT latest (b), not the long-demoted a.
    c = reflect_db.add_learning("use foo", conn=conn)
    assert _row(conn, b)["is_latest"] == 0
    assert _row(conn, b)["superseded_by_learning_id"] == c
    assert _row(conn, a)["superseded_by_learning_id"] == b  # unchanged


def test_history_snapshot_recorded_on_demotion(conn):
    a = reflect_db.add_learning("use foo", conn=conn)
    new = reflect_db.add_learning("never use foo", conn=conn)
    history = reflect_db.get_learning_history(a, conn=conn)
    assert [h["change_type"] for h in history] == ["contradiction"]
    snap = json.loads(history[0]["snapshot_json"])
    assert snap["is_latest"] == 1  # pre-demotion form archived (S6)
    assert new in history[0]["reason"]


def test_detector_failure_never_breaks_the_write(conn, monkeypatch):
    """Post-write hook is silent-fail shaped: the learning still lands."""
    monkeypatch.setattr(
        reflect_db, "_load_contradiction_detector",
        lambda: (_ for _ in ()).throw(RuntimeError("detector exploded")),
    )
    lid = reflect_db.add_learning("never use foo", conn=conn)
    assert _row(conn, lid) is not None


# ── acceptance 2: contradiction event in sqlite + events.jsonl ──────────────

def test_contradiction_event_written_to_sqlite(conn):
    old = reflect_db.add_learning("use foo", conn=conn)
    new = reflect_db.add_learning("never use foo", conn=conn)
    events = reflect_db.get_events_by_type("contradiction_detected", conn=conn)
    assert len(events) == 1
    assert events[0]["learning_id"] == old
    details = json.loads(events[0]["details_json"])
    assert details["older_id"] == old
    assert details["newer_id"] == new
    assert details["similarity"] == 1.0
    assert details["older_title"] == "use foo"
    assert details["newer_title"] == "never use foo"


def test_contradiction_event_mirrored_to_events_jsonl(conn, tmp_path):
    old = reflect_db.add_learning("use foo", conn=conn)
    new = reflect_db.add_learning("never use foo", conn=conn)
    records = _events_jsonl(tmp_path)
    assert len(records) == 1
    rec = records[0]
    assert rec["type"] == "contradiction_detected"
    assert rec["older_id"] == old and rec["newer_id"] == new
    assert rec["similarity"] == 1.0
    assert rec["created_at"]


def test_no_contradiction_no_jsonl(conn, tmp_path):
    reflect_db.add_learning("use foo", conn=conn)
    reflect_db.add_learning("use bar", conn=conn)
    assert _events_jsonl(tmp_path) == []


def test_get_contradiction_count(conn):
    assert reflect_db.get_contradiction_count(conn=conn) == 0
    reflect_db.add_learning("use foo", conn=conn)
    reflect_db.add_learning("never use foo", conn=conn)
    assert reflect_db.get_contradiction_count(conn=conn) == 1


# ── schema migration + concept-index backfill ───────────────────────────────

def test_migration_adds_is_latest_to_old_db(tmp_path):
    """A pre-SG1 DB gains is_latest (default 1) and a backfilled concept_index."""
    db_file = tmp_path / "old.db"
    raw = sqlite3.connect(str(db_file))
    raw.execute(
        """CREATE TABLE learnings (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               category TEXT NOT NULL DEFAULT 'Unknown',
               confidence TEXT NOT NULL DEFAULT 'LOW',
               status TEXT NOT NULL DEFAULT 'pending',
               source_tool TEXT NOT NULL DEFAULT '',
               source_path TEXT NOT NULL DEFAULT '',
               content_hash TEXT NOT NULL DEFAULT '',
               created_at TEXT NOT NULL,
               approved_at TEXT,
               indexed_at TEXT
           )"""
    )
    raw.execute(
        "INSERT INTO learnings (id, title, created_at) "
        "VALUES ('legacy1', 'use foo', '2026-01-01T00:00:00+00:00')"
    )
    raw.commit()
    raw.close()

    connection = reflect_db.init_db(db_file)
    try:
        row = reflect_db.get_learning("legacy1", conn=connection)
        assert row["is_latest"] == 1
        concepts = {
            r[0]
            for r in connection.execute(
                "SELECT concept FROM concept_index WHERE learning_id = 'legacy1'"
            ).fetchall()
        }
        assert {"use", "foo"} <= concepts
        # Backfilled legacy rows are live contradiction candidates.
        new = reflect_db.add_learning("never use foo", scope="project", conn=connection)
        assert reflect_db.get_learning("legacy1", conn=connection)["is_latest"] == 0
        assert (
            reflect_db.get_learning("legacy1", conn=connection)[
                "superseded_by_learning_id"
            ]
            == new
        )
    finally:
        reflect_db.close_all()


def test_concept_index_rows_written_per_learning(conn):
    lid = reflect_db.add_learning("never use var in TypeScript", conn=conn)
    concepts = {
        r[0]
        for r in conn.execute(
            "SELECT concept FROM concept_index WHERE learning_id = ?", (lid,)
        ).fetchall()
    }
    assert "never" not in concepts
    assert {"use", "var", "typescript"} <= concepts


# ── acceptance 3: status skill shows contradiction count ────────────────────

def test_cli_contradictions_shows_count(tmp_path):
    db_file = tmp_path / "cli.db"
    connection = reflect_db.init_db(db_file)
    reflect_db.add_learning("use foo", conn=connection)
    reflect_db.add_learning("never use foo", conn=connection)
    reflect_db.close_all()

    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(db_file)
    result = subprocess.run(
        [sys.executable, str(SCRIPTS / "reflect_db.py"), "contradictions"],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    assert "contradictions detected: 1" in result.stdout
    assert "'use foo'" in result.stdout
    assert "'never use foo'" in result.stdout


def test_status_skill_documents_contradictions():
    skill = STATUS_SKILL.read_text()
    assert "Contradictions" in skill
    assert "contradiction_detected" in skill
    assert "events.jsonl" in skill
    assert "reflect_db.py contradictions" in skill
    assert "is_latest" in skill


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
