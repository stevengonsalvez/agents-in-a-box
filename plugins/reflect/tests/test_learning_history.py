# ABOUTME: Regression tests for port S6 — history snapshots on UPDATE.
# ABOUTME: Pins the learning_history table (every UPDATE path archives the
# ABOUTME: old row form), the git-readable .history.yaml sidecar written on
# ABOUTME: knowledge-note overwrite, and the status-skill update-count view.
"""Port S6: belief revision becomes non-destructive.

Acceptance criteria pinned here:
  1. UPDATE produces a history row
  2. sidecar diff is git-readable
  3. status skill shows update count per learning
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
sys.path.insert(0, str(SCRIPTS))

import output_generator  # noqa: E402
import reflect_db  # noqa: E402

try:
    import yaml
except ImportError:  # pragma: no cover — exercised only on slim envs
    yaml = None


@pytest.fixture
def conn(tmp_path):
    """Fresh isolated DB per test; never touches ~/.reflect."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    yield connection
    reflect_db.close_all()


def _history(conn, lid: str) -> list[dict]:
    return reflect_db.get_learning_history(lid, conn=conn)


# ---------- acceptance 1: UPDATE produces a history row ----------

def test_status_update_snapshots_old_form(conn):
    lid = reflect_db.add_learning("never use var", conn=conn)
    reflect_db.update_learning_status(lid, "approved", conn=conn)

    rows = _history(conn, lid)
    assert len(rows) == 1
    row = rows[0]
    assert row["change_type"] == "status_change"
    snapshot = json.loads(row["snapshot_json"])
    assert snapshot["status"] == "pending", "snapshot must hold the OLD form"
    assert snapshot["title"] == "never use var"
    assert "status" in json.loads(row["changed_fields"])


def test_proof_update_snapshots_old_form(conn):
    lid = reflect_db.add_learning("rule", source_memory_ids=["mem-1"], conn=conn)
    assert reflect_db.add_learning_proof(lid, "mem-2", conn=conn) is True

    rows = _history(conn, lid)
    assert len(rows) == 1
    row = rows[0]
    assert row["change_type"] == "proof_added"
    snapshot = json.loads(row["snapshot_json"])
    assert snapshot["proof_count"] == 1
    assert json.loads(snapshot["source_memory_ids"]) == ["mem-1"]
    assert sorted(json.loads(row["changed_fields"])) == [
        "proof_count",
        "source_memory_ids",
    ]


def test_every_update_appends_another_row(conn):
    """The audit trail is append-only: N updates → N snapshots, in order."""
    lid = reflect_db.add_learning("rule", conn=conn)
    reflect_db.update_learning_status(lid, "approved", conn=conn)
    reflect_db.update_learning_status(lid, "indexed", conn=conn)
    reflect_db.add_learning_proof(lid, "mem-1", conn=conn)

    rows = _history(conn, lid)  # newest first
    assert len(rows) == 3
    statuses = [json.loads(r["snapshot_json"])["status"] for r in reversed(rows)]
    assert statuses == ["pending", "approved", "indexed"]


def test_idempotent_proof_noop_writes_no_history(conn):
    """A rejected UPDATE (dup source id) must not pollute the audit trail."""
    lid = reflect_db.add_learning("rule", source_memory_ids=["mem-1"], conn=conn)
    assert reflect_db.add_learning_proof(lid, "mem-1", conn=conn) is False
    assert _history(conn, lid) == []


def test_create_writes_no_history(conn):
    """CREATE is not a revision — history starts at the first UPDATE."""
    lid = reflect_db.add_learning("fresh", conn=conn)
    assert _history(conn, lid) == []


def test_snapshot_missing_learning_returns_none(conn):
    assert reflect_db.snapshot_learning_history("nope", conn=conn) is None
    assert _history(conn, "nope") == []


def test_revert_reason_lands_in_history_reason(conn):
    lid = reflect_db.add_learning("rule", conn=conn)
    reflect_db.update_learning_status(
        lid, "reverted", revert_reason="superseded by stricter rule", conn=conn
    )
    rows = _history(conn, lid)
    assert rows[0]["reason"] == "superseded by stricter rule"


def test_legacy_db_gains_history_table(tmp_path):
    """Pre-S6 DBs (no learning_history) migrate transparently on init."""
    db_file = tmp_path / "legacy.db"
    legacy = sqlite3.connect(db_file)
    legacy.executescript(
        """
        CREATE TABLE learnings (
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
        );
        INSERT INTO learnings (id, title, created_at)
        VALUES ('old-1', 'pre-S6 learning', '2026-01-01T00:00:00+00:00');
        """
    )
    legacy.commit()
    legacy.close()

    conn = reflect_db.init_db(db_file)
    try:
        reflect_db.update_learning_status("old-1", "approved", conn=conn)
        rows = reflect_db.get_learning_history("old-1", conn=conn)
        assert len(rows) == 1
        assert json.loads(rows[0]["snapshot_json"])["status"] == "pending"
    finally:
        reflect_db.close_all()


# ---------- acceptance 2: sidecar diff is git-readable ----------

def _note_kwargs(**overrides):
    base = dict(
        title="Webpack chunk error",
        category="build-errors",
        tags=["webpack"],
        symptoms=["chunk load failed"],
        root_cause="stale manifest",
        key_insight="bust the cache",
        problem="Chunks 404 after deploy.",
        solution="Hash chunk filenames.",
    )
    base.update(overrides)
    return base


@pytest.fixture
def project_dir(tmp_path, monkeypatch):
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(tmp_path))
    return tmp_path


def test_first_write_creates_no_sidecar(project_dir):
    note_path, _ = output_generator.create_knowledge_note(**_note_kwargs())
    assert note_path.exists()
    assert not output_generator.get_history_sidecar_path(note_path).exists()


def test_update_writes_history_sidecar_with_old_form(project_dir):
    note_path, _ = output_generator.create_knowledge_note(**_note_kwargs())
    original = note_path.read_text()

    output_generator.create_knowledge_note(
        **_note_kwargs(solution="Hash chunk filenames AND purge the CDN.")
    )

    sidecar = output_generator.get_history_sidecar_path(note_path)
    assert sidecar.exists()
    assert sidecar.name.endswith(".history.yaml")
    assert sidecar.parent == note_path.parent

    text = sidecar.read_text()
    assert "Hash chunk filenames." in text, "old solution must be archived"
    if yaml is not None:
        entries = yaml.safe_load(text)
        assert isinstance(entries, list) and len(entries) == 1
        entry = entries[0]
        assert entry["reason"] == "update"
        assert entry["snapshot_at"]
        assert entry["content_hash"]
        # The literal block round-trips the previous note byte-shape
        # (whitespace-only lines normalised to empty).
        assert entry["previous"].strip() == original.strip()


def test_sidecar_is_append_only_so_diffs_are_additive(project_dir):
    """Git-readable: each UPDATE only appends bytes — prior entries untouched."""
    note_path, _ = output_generator.create_knowledge_note(**_note_kwargs())
    output_generator.create_knowledge_note(**_note_kwargs(solution="v2"))
    sidecar = output_generator.get_history_sidecar_path(note_path)
    after_first = sidecar.read_text()

    output_generator.create_knowledge_note(**_note_kwargs(solution="v3"))
    after_second = sidecar.read_text()

    assert after_second.startswith(after_first), "append-only contract broken"
    if yaml is not None:
        entries = yaml.safe_load(after_second)
        assert len(entries) == 2
        assert "Hash chunk filenames." in entries[0]["previous"]
        assert "v2" in entries[1]["previous"]


def test_identical_rewrite_is_not_an_update(project_dir, monkeypatch):
    from datetime import datetime as real_datetime

    class _FrozenDatetime(real_datetime):
        """Pin now() so provenance timestamps can't perturb the bytes."""

        @classmethod
        def now(cls, tz=None):
            return cls(2026, 6, 10, 12, 0, 0)

    monkeypatch.setattr(output_generator, "datetime", _FrozenDatetime)
    note_path, _ = output_generator.create_knowledge_note(**_note_kwargs())
    # Same payload, same timestamps → byte-identical → no history entry.
    output_generator.create_knowledge_note(**_note_kwargs())
    assert not output_generator.get_history_sidecar_path(note_path).exists()


def test_append_history_sidecar_fails_silently(tmp_path):
    """Sidecar trouble must never block the note write (silent-fail shape)."""
    bogus = tmp_path / "no-such-dir" / "note.md"
    assert output_generator.append_history_sidecar(bogus, "old body") is None


# ---------- acceptance 3: status skill shows update count per learning ----------

def test_get_update_counts_per_learning(conn):
    busy = reflect_db.add_learning("busy rule", conn=conn)
    quiet = reflect_db.add_learning("quiet rule", conn=conn)
    reflect_db.update_learning_status(busy, "approved", conn=conn)
    reflect_db.update_learning_status(busy, "indexed", conn=conn)
    reflect_db.add_learning_proof(busy, "mem-1", conn=conn)
    reflect_db.update_learning_status(quiet, "approved", conn=conn)

    counts = reflect_db.get_update_counts(conn=conn)
    assert [(c["learning_id"], c["update_count"]) for c in counts] == [
        (busy, 3),
        (quiet, 1),
    ]
    assert counts[0]["title"] == "busy rule"
    assert counts[0]["last_updated_at"]


def test_cli_history_command_shows_update_counts(tmp_path):
    db_file = tmp_path / "cli.db"
    conn = reflect_db.init_db(db_file)
    lid = reflect_db.add_learning("cli visible rule", conn=conn)
    reflect_db.update_learning_status(lid, "approved", conn=conn)
    reflect_db.update_learning_status(lid, "indexed", conn=conn)
    reflect_db.close_all()

    env = dict(os.environ, REFLECT_DB_PATH=str(db_file))
    proc = subprocess.run(
        [sys.executable, str(SCRIPTS / "reflect_db.py"), "history"],
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )
    assert proc.returncode == 0, proc.stderr
    assert lid in proc.stdout
    assert "updates=2" in proc.stdout
    assert "cli visible rule" in proc.stdout


def test_cli_stats_includes_history_table(tmp_path):
    db_file = tmp_path / "cli.db"
    reflect_db.init_db(db_file)
    reflect_db.close_all()

    env = dict(os.environ, REFLECT_DB_PATH=str(db_file))
    proc = subprocess.run(
        [sys.executable, str(SCRIPTS / "reflect_db.py"), "stats"],
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )
    assert proc.returncode == 0, proc.stderr
    assert "learning_history" in proc.stdout


def test_status_skill_documents_update_history_view():
    skill = (
        PLUGIN_ROOT / "skills" / "reflect-status" / "SKILL.md"
    ).read_text()
    assert "reflect_db.py history" in skill
    assert "learning_history" in skill
    assert "Update" in skill and "per" in skill.lower()


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
