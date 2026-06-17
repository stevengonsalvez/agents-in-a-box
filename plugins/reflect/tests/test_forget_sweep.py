# ABOUTME: Regression tests for port A3 — per-row TTL (`forget_after` ISO
# ABOUTME: timestamp). Pins the learnings column + migration, the sweep
# ABOUTME: (expired rows archived non-destructively: status -> archived,
# ABOUTME: is_latest 0, S6 snapshot, learning_forgotten event, note file
# ABOUTME: moved to .forgotten/), absent-TTL permanence, the sweep CLI,
# ABOUTME: and the drain plumbing (cascade CREATE forget_after passthrough).
"""Port A3: learnings may carry a forget_after TTL; an hourly sweep archives
expired rows (agentmemory Memory.forgetAfter + mem::auto-forget shape).

Acceptance criteria pinned here:
  1. learning with forget_after=now-1d gets archived on next sweep
  2. absent forget_after = permanent
  3. drain optionally proposes forget_after for clearly-scoped corrections
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
SWEEP_SCRIPT = SCRIPTS / "reflect_forget_sweep.py"
PLIST = PLUGIN_ROOT / "launchd" / "com.reflect.forget.plist"
SKILL = PLUGIN_ROOT / "skills" / "reflect" / "SKILL.md"
TEMPLATE = PLUGIN_ROOT / "assets" / "learning_template.md"
sys.path.insert(0, str(SCRIPTS))

import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402
import reflect_forget_sweep  # noqa: E402


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB per test, wired as the module default connection."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


def _iso(delta: timedelta) -> str:
    return (datetime.now(timezone.utc) + delta).isoformat()


def _row(conn, lid):
    return reflect_db.get_learning(lid, conn=conn)


# ── acceptance 1: forget_after=now-1d archived on next sweep ─────────────────

def test_expired_learning_archived_on_sweep(conn):
    lid = reflect_db.add_learning(
        "avoid payments-service, incident in progress",
        forget_after=_iso(timedelta(days=-1)),
        conn=conn,
    )
    expired = reflect_db.sweep_expired_learnings(conn=conn)
    assert [r["id"] for r in expired] == [lid]
    row = _row(conn, lid)
    assert row["status"] == "archived"
    assert row["is_latest"] == 0


def test_sweep_writes_audit_event_and_history_snapshot(conn):
    lid = reflect_db.add_learning(
        "sprint-scoped rule", forget_after=_iso(timedelta(days=-1)), conn=conn,
    )
    reflect_db.sweep_expired_learnings(conn=conn)

    events = reflect_db.get_events(reflect_db.FORGET_EVENT_TYPE, conn=conn)
    assert len(events) == 1
    assert events[0]["learning_id"] == lid
    details = json.loads(events[0]["details_json"])
    assert details["title"] == "sprint-scoped rule"

    history = reflect_db.get_learning_history(lid, conn=conn)
    sweep_snaps = [h for h in history if h["change_type"] == "forget_sweep"]
    assert len(sweep_snaps) == 1
    # S6: the snapshot preserves the PRE-archive form.
    snapshot = json.loads(sweep_snaps[0]["snapshot_json"])
    assert snapshot["status"] != "archived"


def test_sweep_is_idempotent(conn):
    reflect_db.add_learning(
        "expired", forget_after=_iso(timedelta(days=-1)), conn=conn,
    )
    assert len(reflect_db.sweep_expired_learnings(conn=conn)) == 1
    # Already archived — never expires twice.
    assert reflect_db.sweep_expired_learnings(conn=conn) == []
    assert len(reflect_db.get_events(reflect_db.FORGET_EVENT_TYPE, conn=conn)) == 1


def test_dry_run_reports_without_mutating(conn):
    lid = reflect_db.add_learning(
        "expired", forget_after=_iso(timedelta(days=-1)), conn=conn,
    )
    expired = reflect_db.sweep_expired_learnings(dry_run=True, conn=conn)
    assert [r["id"] for r in expired] == [lid]
    assert _row(conn, lid)["status"] != "archived"
    assert reflect_db.get_events(reflect_db.FORGET_EVENT_TYPE, conn=conn) == []


def test_zulu_suffix_ttl_parses_and_expires(conn):
    past_z = (
        datetime.now(timezone.utc) - timedelta(days=1)
    ).strftime("%Y-%m-%dT%H:%M:%SZ")
    lid = reflect_db.add_learning("zulu ttl", forget_after=past_z, conn=conn)
    expired = reflect_db.sweep_expired_learnings(conn=conn)
    assert [r["id"] for r in expired] == [lid]


def test_archived_row_excluded_from_contradiction_candidates(conn):
    old = reflect_db.add_learning(
        "use foo service", forget_after=_iso(timedelta(days=-1)), conn=conn,
    )
    reflect_db.sweep_expired_learnings(conn=conn)
    # A contradicting write must not demote (or even consider) archived rows.
    resolved = reflect_db.detect_and_resolve_contradictions(
        "new-id", "never use foo service", conn=conn,
    )
    assert resolved == []
    assert _row(conn, old)["superseded_by_learning_id"] is None


# ── acceptance 2: absent forget_after = permanent ────────────────────────────

def test_absent_forget_after_is_permanent(conn):
    lid = reflect_db.add_learning("durable rule", conn=conn)
    assert _row(conn, lid)["forget_after"] is None
    assert reflect_db.sweep_expired_learnings(conn=conn) == []
    row = _row(conn, lid)
    assert row["status"] == "pending"
    assert row["is_latest"] == 1


def test_future_ttl_not_swept_yet(conn):
    lid = reflect_db.add_learning(
        "valid this quarter", forget_after=_iso(timedelta(days=30)), conn=conn,
    )
    assert reflect_db.sweep_expired_learnings(conn=conn) == []
    assert _row(conn, lid)["status"] == "pending"
    # ... but it expires once the clock passes the TTL.
    expired = reflect_db.sweep_expired_learnings(
        now=_iso(timedelta(days=31)), conn=conn,
    )
    assert [r["id"] for r in expired] == [lid]


def test_unparseable_ttl_treated_as_permanent(conn):
    lid = reflect_db.add_learning(
        "bad ttl", forget_after="next sprint sometime", conn=conn,
    )
    assert reflect_db.sweep_expired_learnings(conn=conn) == []
    assert _row(conn, lid)["status"] == "pending"


def test_empty_string_ttl_stored_as_null(conn):
    lid = reflect_db.add_learning("empty ttl", forget_after="", conn=conn)
    assert _row(conn, lid)["forget_after"] is None


# ── schema migration: pre-A3 DBs gain the column + archived status ──────────

def test_migration_adds_forget_after_to_existing_db(tmp_path):
    db_file = tmp_path / "old.db"
    import sqlite3

    raw = sqlite3.connect(str(db_file))
    # Pre-A3 shape: no forget_after column, status CHECK without 'archived'.
    pre_a3_ddl = reflect_db._LEARNINGS_DDL.replace(
        "    forget_after            TEXT,\n", ""
    ).replace(", 'archived'", "")
    assert "forget_after" not in pre_a3_ddl and "'archived'" not in pre_a3_ddl
    raw.executescript(pre_a3_ddl)
    raw.execute(
        "INSERT INTO learnings (id, title, created_at) VALUES ('l1', 'old row', '2026-01-01')"
    )
    raw.commit()
    raw.close()

    conn = reflect_db.init_db(db_file)
    try:
        cols = {r[1] for r in conn.execute("PRAGMA table_info(learnings)")}
        assert "forget_after" in cols
        row = conn.execute("SELECT * FROM learnings WHERE id='l1'").fetchone()
        assert row["forget_after"] is None  # migrated rows stay permanent
        # New CHECK accepts the archived status.
        with conn:
            conn.execute("UPDATE learnings SET status='archived' WHERE id='l1'")
    finally:
        reflect_db.close_all()


# ── sweep script: file archival + CLI ────────────────────────────────────────

def test_run_sweep_moves_artifact_into_forgotten_dir(conn, tmp_path, monkeypatch):
    note = tmp_path / "docs" / "note.md"
    sidecar = tmp_path / "docs" / "note.entities.yaml"
    note.parent.mkdir(parents=True)
    note.write_text("# scoped note\n")
    sidecar.write_text("entities: []\n")
    lid = reflect_db.add_learning(
        "scoped note",
        forget_after=_iso(timedelta(days=-1)),
        artifact_path=str(note),
        sidecar_path=str(sidecar),
        conn=conn,
    )
    summary = reflect_forget_sweep.run_sweep()
    assert summary["archived"] == 1
    assert summary["learnings"][0]["id"] == lid
    assert not note.exists() and not sidecar.exists()
    forgotten = tmp_path / "docs" / reflect_forget_sweep.FORGOTTEN_DIR_NAME
    assert (forgotten / "note.md").exists()
    assert (forgotten / "note.entities.yaml").exists()


def test_run_sweep_survives_missing_artifact_file(conn):
    reflect_db.add_learning(
        "no file on disk",
        forget_after=_iso(timedelta(days=-1)),
        artifact_path="/nonexistent/path/note.md",
        conn=conn,
    )
    summary = reflect_forget_sweep.run_sweep()
    assert summary["archived"] == 1
    assert summary["learnings"][0]["files_archived"] == []


def test_cli_sweep_archives_expired_learning(tmp_path):
    db_file = tmp_path / "cli.db"
    connection = reflect_db.init_db(db_file)
    lid = reflect_db.add_learning(
        "cli expired", forget_after=_iso(timedelta(days=-1)), conn=connection,
    )
    keep = reflect_db.add_learning("cli permanent", conn=connection)
    reflect_db.close_all()

    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(db_file)
    result = subprocess.run(
        [sys.executable, str(SWEEP_SCRIPT)],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    summary = json.loads(result.stdout)
    assert summary["archived"] == 1
    assert summary["learnings"][0]["id"] == lid

    connection = reflect_db.init_db(db_file)
    try:
        assert reflect_db.get_learning(lid, conn=connection)["status"] == "archived"
        assert reflect_db.get_learning(keep, conn=connection)["status"] == "pending"
    finally:
        reflect_db.close_all()


def test_cli_dry_run_mutates_nothing(tmp_path):
    db_file = tmp_path / "cli.db"
    connection = reflect_db.init_db(db_file)
    lid = reflect_db.add_learning(
        "cli expired", forget_after=_iso(timedelta(days=-1)), conn=connection,
    )
    reflect_db.close_all()

    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(db_file)
    result = subprocess.run(
        [sys.executable, str(SWEEP_SCRIPT), "--dry-run"],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    summary = json.loads(result.stdout)
    assert summary["dry_run"] is True and summary["expired"] == 1

    connection = reflect_db.init_db(db_file)
    try:
        assert reflect_db.get_learning(lid, conn=connection)["status"] == "pending"
    finally:
        reflect_db.close_all()


# ── acceptance 3: drain optionally proposes forget_after ────────────────────

def test_cascade_create_passes_forget_after_through(conn, monkeypatch):
    monkeypatch.setattr(reflect_cascade, "find_semantic_twin", lambda *a, **k: None)
    ttl = _iso(timedelta(days=2))
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "avoid X service, it's down",
          "reason": "scoped to incident", "forget_after": ttl}],
        source_memory_id="t1",
    )
    assert summary["created"] == 1 and summary["errors"] == []
    row = conn.execute(
        "SELECT * FROM learnings WHERE title = ?", ("avoid X service, it's down",),
    ).fetchone()
    assert row["forget_after"] == ttl


def test_cascade_create_without_forget_after_is_permanent(conn, monkeypatch):
    monkeypatch.setattr(reflect_cascade, "find_semantic_twin", lambda *a, **k: None)
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "durable drain rule", "reason": "new"}],
        source_memory_id="t1",
    )
    assert summary["created"] == 1
    row = conn.execute(
        "SELECT * FROM learnings WHERE title = ?", ("durable drain rule",),
    ).fetchone()
    assert row["forget_after"] is None


# ── plumbing pins: skill doc, template, and launchd timer carry the port ────

def test_skill_doc_offers_forget_after_in_create_contract():
    text = SKILL.read_text()
    assert "forget_after" in text
    # The drain action contract documents the optional TTL on CREATE.
    assert '"forget_after"' in text or "forget_after:" in text


def test_learning_template_carries_forget_after_field():
    text = TEMPLATE.read_text()
    assert "forget_after: null" in text


def test_launchd_plist_runs_sweep_hourly():
    text = PLIST.read_text()
    assert "reflect_forget_sweep.py" in text
    assert "<integer>3600</integer>" in text
    assert "com.reflect.forget" in text


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
