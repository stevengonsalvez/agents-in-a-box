# ABOUTME: Regression tests for port S9 — volatile signals out of frontmatter.
# ABOUTME: Pins the ByteRover runtime-signals sidecar shape: ALL ranking
# ABOUTME: signals (importance, maturity, recall/feedback counters) live in
# ABOUTME: reflect.db's learning_signals table; note markdown is immutable
# ABOUTME: after write — per-query bumps never touch files — and pre-existing
# ABOUTME: telemetry on the learnings table migrates into the sidecar.
"""Port S9: volatile ranking signals live ONLY in reflect.db.

Acceptance bullets pinned here:
  1. per-query bumps don't touch markdown files (add_recall_event writes the
     learning_signals sidecar row; the note file's bytes and mtime are
     untouched; neither the template nor create_knowledge_note ever emit a
     volatile field into frontmatter)
  2. existing data migrated (a legacy DB whose learnings rows already carry
     recall/helpful/ignored/stale counters gets those values copied into
     learning_signals on init_db; the backfill is idempotent and never
     resets accumulated signals)
"""

from __future__ import annotations

import re
import sqlite3
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
TEMPLATE = PLUGIN_ROOT / "assets" / "learning_template.md"
sys.path.insert(0, str(SCRIPTS))

import output_generator  # noqa: E402
import reflect_db  # noqa: E402


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB per test, wired as the module default connection."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


@pytest.fixture()
def project(tmp_path, monkeypatch):
    """Isolated non-git project dir so notes land under tmp_path."""
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    return tmp_path


def _frontmatter_keys(text: str) -> set[str]:
    """Top-level frontmatter keys (comments and nested keys excluded)."""
    assert text.startswith("---")
    end = text.find("\n---", 3)
    header = text[3:end]
    keys = set()
    for line in header.splitlines():
        m = re.match(r"^([A-Za-z_][A-Za-z0-9_]*):", line)
        if m:
            keys.add(m.group(1))
    return keys


def _signals_row(conn, learning_id: str):
    return conn.execute(
        "SELECT * FROM learning_signals WHERE learning_id = ?",
        (learning_id,),
    ).fetchone()


# =========================================================================
# Acceptance 1 — per-query bumps don't touch markdown files
# =========================================================================

def test_template_frontmatter_carries_no_volatile_fields():
    keys = _frontmatter_keys(TEMPLATE.read_text())
    leaked = keys & reflect_db.VOLATILE_SIGNAL_FIELDS
    assert not leaked, f"volatile signal fields leaked into template: {leaked}"


def test_template_documents_the_sidecar_contract():
    text = TEMPLATE.read_text()
    assert "learning_signals" in text  # points writers at the DB sidecar


def test_create_knowledge_note_emits_no_volatile_fields(project):
    path, _ = output_generator.create_knowledge_note(
        title="S9 sidecar note", category="testing", tags=["s9"],
        symptoms=["s"], root_cause="rc", key_insight="ki",
        problem="p", solution="s", confidence="high",
    )
    keys = _frontmatter_keys(path.read_text())
    leaked = keys & reflect_db.VOLATILE_SIGNAL_FIELDS
    assert not leaked, f"volatile signal fields leaked into note: {leaked}"


def test_recall_bump_leaves_note_file_untouched(conn, tmp_path):
    note = tmp_path / "note.md"
    note.write_text("---\ntitle: immutable\n---\n\nbody\n")
    before_bytes = note.read_bytes()
    before_mtime = note.stat().st_mtime_ns

    lid = reflect_db.add_learning(
        "s9 immutable note", artifact_path=str(note), conn=conn,
    )
    for feedback in ("", "helpful", "ignored", "stale"):
        reflect_db.add_recall_event(lid, "query", feedback=feedback, conn=conn)

    assert note.read_bytes() == before_bytes
    assert note.stat().st_mtime_ns == before_mtime


def test_recall_bump_lands_in_sidecar_table(conn):
    lid = reflect_db.add_learning("s9 bumped", conn=conn)
    reflect_db.add_recall_event(lid, "q1", conn=conn)
    reflect_db.add_recall_event(lid, "q2", feedback="helpful", conn=conn)
    reflect_db.add_recall_event(lid, "q3", feedback="ignored", conn=conn)
    reflect_db.add_recall_event(lid, "q4", feedback="stale", conn=conn)

    row = _signals_row(conn, lid)
    assert row["recall_count"] == 4
    assert row["helpful_count"] == 1
    assert row["ignored_count"] == 1
    assert row["stale_count"] == 1
    assert row["last_recalled_at"]  # set on every bump


def test_legacy_learnings_columns_stay_in_sync(conn):
    """The pre-S9 counters on the learnings table remain a mirror so existing
    readers keep working — both homes are inside reflect.db, never markdown."""
    lid = reflect_db.add_learning("s9 mirror", conn=conn)
    reflect_db.add_recall_event(lid, "q", feedback="helpful", conn=conn)
    learning = reflect_db.get_learning(lid, conn=conn)
    signals = reflect_db.get_learning_signals(lid, conn=conn)
    assert learning["recall_count"] == signals["recall_count"] == 1
    assert learning["helpful_count"] == signals["helpful_count"] == 1


# =========================================================================
# Acceptance 2 — existing data migrated
# =========================================================================

def test_legacy_db_counters_migrate_into_sidecar(tmp_path):
    """A pre-S9 DB (counters on learnings, no learning_signals table) gets
    every row's telemetry copied into the sidecar on the next init_db."""
    db_file = tmp_path / "legacy.db"
    legacy = sqlite3.connect(db_file)
    legacy.executescript(
        """
        CREATE TABLE learnings (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            category TEXT NOT NULL DEFAULT 'Unknown',
            confidence TEXT NOT NULL DEFAULT 'LOW',
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'approved', 'rejected',
                                  'indexed', 'reverted')),
            source_tool TEXT NOT NULL DEFAULT '',
            source_path TEXT NOT NULL DEFAULT '',
            content_hash TEXT NOT NULL DEFAULT '',
            commit_hash TEXT,
            created_at TEXT NOT NULL,
            approved_at TEXT,
            indexed_at TEXT,
            reverted_at TEXT,
            revert_reason TEXT,
            last_recalled_at TEXT,
            recall_count INTEGER NOT NULL DEFAULT 0,
            helpful_count INTEGER NOT NULL DEFAULT 0,
            ignored_count INTEGER NOT NULL DEFAULT 0,
            stale_count INTEGER NOT NULL DEFAULT 0
        );
        """
    )
    legacy.execute(
        "INSERT INTO learnings (id, title, created_at, last_recalled_at, "
        "recall_count, helpful_count, ignored_count, stale_count) "
        "VALUES ('lrn-hot', 'hot learning', '2026-01-01T00:00:00+00:00', "
        "'2026-02-01T00:00:00+00:00', 7, 3, 2, 1)",
    )
    legacy.execute(
        "INSERT INTO learnings (id, title, created_at) "
        "VALUES ('lrn-cold', 'cold learning', '2026-01-02T00:00:00+00:00')",
    )
    legacy.commit()
    legacy.close()

    conn = reflect_db.init_db(db_file)
    try:
        hot = reflect_db.get_learning_signals("lrn-hot", conn=conn)
        cold = reflect_db.get_learning_signals("lrn-cold", conn=conn)
        rows = conn.execute("SELECT COUNT(*) FROM learning_signals").fetchone()
    finally:
        reflect_db.close_all()

    assert rows[0] == 2
    assert hot["recall_count"] == 7
    assert hot["helpful_count"] == 3
    assert hot["ignored_count"] == 2
    assert hot["stale_count"] == 1
    assert hot["last_recalled_at"] == "2026-02-01T00:00:00+00:00"
    # New ranking fields start at the ByteRover defaults.
    assert hot["importance"] == pytest.approx(50.0)
    assert hot["maturity"] == "draft"
    assert cold["recall_count"] == 0
    assert cold["importance"] == pytest.approx(50.0)
    assert cold["maturity"] == "draft"


def test_backfill_is_idempotent_and_preserves_accumulated_signals(tmp_path):
    """Re-opening an already-migrated DB must never reset sidecar state back
    to defaults or to the learnings-table mirror values."""
    db_file = tmp_path / "migrated.db"
    conn = reflect_db.init_db(db_file)
    lid = reflect_db.add_learning("s9 sticky", conn=conn)
    reflect_db.add_recall_event(lid, "q", feedback="helpful", conn=conn)
    reflect_db.set_learning_signals(
        lid, importance=80, maturity="validated", conn=conn,
    )
    reflect_db.close_all()

    conn = reflect_db.init_db(db_file)
    try:
        signals = reflect_db.get_learning_signals(lid, conn=conn)
        count = conn.execute(
            "SELECT COUNT(*) FROM learning_signals WHERE learning_id = ?",
            (lid,),
        ).fetchone()[0]
    finally:
        reflect_db.close_all()

    assert count == 1
    assert signals["importance"] == pytest.approx(80.0)
    assert signals["maturity"] == "validated"
    assert signals["recall_count"] == 1
    assert signals["helpful_count"] == 1


def test_new_learning_is_seeded_with_default_sidecar_row(conn):
    lid = reflect_db.add_learning("s9 fresh", conn=conn)
    row = _signals_row(conn, lid)
    assert row is not None
    assert row["importance"] == pytest.approx(50.0)
    assert row["maturity"] == "draft"
    assert row["recall_count"] == 0
    assert row["last_recalled_at"] is None


def test_missing_sidecar_row_reads_as_defaults(conn):
    """No row yet → ByteRover createDefaultRuntimeSignals semantics: readers
    get the defaults, never a miss."""
    signals = reflect_db.get_learning_signals("lrn-never-seen", conn=conn)
    assert signals["importance"] == pytest.approx(50.0)
    assert signals["maturity"] == "draft"
    assert signals["recall_count"] == 0
    assert signals["helpful_count"] == 0
    assert signals["ignored_count"] == 0
    assert signals["stale_count"] == 0
    assert signals["last_recalled_at"] is None


# =========================================================================
# Sidecar write API — clamping / validation / no-op semantics
# =========================================================================

def test_set_learning_signals_clamps_importance(conn):
    lid = reflect_db.add_learning("s9 clamp", conn=conn)
    assert reflect_db.set_learning_signals(
        lid, importance=150, conn=conn,
    )["importance"] == pytest.approx(100.0)
    assert reflect_db.set_learning_signals(
        lid, importance=-5, conn=conn,
    )["importance"] == pytest.approx(0.0)
    # Unparseable → keeps the current value, never a crash.
    assert reflect_db.set_learning_signals(
        lid, importance="junk", conn=conn,
    )["importance"] == pytest.approx(0.0)


def test_set_learning_signals_validates_maturity(conn):
    lid = reflect_db.add_learning("s9 tiers", conn=conn)
    assert reflect_db.set_learning_signals(
        lid, maturity="VALIDATED", conn=conn,
    )["maturity"] == "validated"
    # Unknown tier is ignored — the CHECK-constrained column keeps its value.
    assert reflect_db.set_learning_signals(
        lid, maturity="bogus", conn=conn,
    )["maturity"] == "validated"
    assert reflect_db.set_learning_signals(
        lid, maturity="core", conn=conn,
    )["maturity"] == "core"


def test_set_learning_signals_missing_learning_is_a_noop(conn):
    assert reflect_db.set_learning_signals(
        "lrn-ghost", importance=90, conn=conn,
    ) is None
    assert _signals_row(conn, "lrn-ghost") is None


def test_strip_volatile_signal_fields_drops_only_volatile_keys():
    frontmatter = {
        "title": "keep me",
        "tags": ["a"],
        "confidence": "HIGH",
        "confidence_num": 0.9,
        "importance": 80,
        "maturity": "core",
        "recall_count": 12,
        "helpful_count": 4,
        "ignored_count": 1,
        "stale_count": 2,
        "last_recalled_at": "2026-06-01T00:00:00+00:00",
    }
    cleaned = reflect_db.strip_volatile_signal_fields(frontmatter)
    assert set(cleaned) == {"title", "tags", "confidence", "confidence_num"}
    # Original dict untouched (pure function).
    assert "importance" in frontmatter


def test_strip_volatile_signal_fields_tolerates_empty_input():
    assert reflect_db.strip_volatile_signal_fields({}) == {}
    assert reflect_db.strip_volatile_signal_fields(None) == {}


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
