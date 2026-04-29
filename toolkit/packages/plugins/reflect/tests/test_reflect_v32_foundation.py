#!/usr/bin/env python3
"""
Foundation tests for the reflect v3.2 schema/domain upgrade.

These stay focused on the new control-plane primitives:
  - domain enums/models import cleanly
  - existing DBs migrate forward in place
  - provenance fields persist on learnings
  - event idempotency works for concurrent-style callers
  - recall telemetry updates learning counters
  - provider/source identity survives upserts
  - artifact/index-job tables round-trip
"""

from __future__ import annotations

import shutil
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
PLUGIN_DIR = TESTS_DIR.parent
SCRIPTS_DIR = PLUGIN_DIR / "scripts"

sys.path.insert(0, str(SCRIPTS_DIR))

import reflect_db  # noqa: E402
from domain.enums import LearningStatus, PrivacyLevel  # noqa: E402
from domain.models import ArtifactRecord, LearningRecord, SourceRecord  # noqa: E402


class ReflectV32FoundationTest(unittest.TestCase):
    sandbox: Path
    db_path: Path

    @classmethod
    def setUpClass(cls) -> None:
        cls.sandbox = Path(tempfile.mkdtemp(prefix="reflect-v32-"))
        cls.db_path = cls.sandbox / "reflect.db"

    @classmethod
    def tearDownClass(cls) -> None:
        shutil.rmtree(cls.sandbox, ignore_errors=True)

    def setUp(self) -> None:
        reflect_db.close_all()
        if self.db_path.exists():
            self.db_path.unlink()
        reflect_db.init_db(self.db_path)

    def test_01_domain_models_import(self) -> None:
        learning = LearningRecord(title="Prefer uv", source_tool="codex")
        source = SourceRecord(provider="claude", path="/tmp/MEMORY.md")
        artifact = ArtifactRecord(
            learning_id="lrn-1",
            artifact_type="knowledge_note",
            path="docs/solutions/example.md",
        )
        self.assertEqual(learning.status, LearningStatus.PENDING.value)
        self.assertEqual(source.status, "active")
        self.assertEqual(artifact.status, "created")

    def test_02_schema_has_v32_tables_and_columns(self) -> None:
        conn = reflect_db.get_conn(self.db_path)
        tables = {
            row["name"]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table'"
            ).fetchall()
        }
        for table in (
            "learnings",
            "proposals",
            "events",
            "sources",
            "index_jobs",
            "recall_events",
            "artifacts",
        ):
            self.assertIn(table, tables)

        learning_columns = {
            row["name"] for row in conn.execute("PRAGMA table_info(learnings)")
        }
        for column in (
            "scope",
            "source_provider",
            "source_kind",
            "source_quote",
            "source_quote_hash",
            "session_id",
            "thread_id",
            "privacy_level",
            "artifact_path",
            "sidecar_path",
            "supersedes_learning_id",
            "superseded_by_learning_id",
            "last_recalled_at",
            "recall_count",
            "helpful_count",
            "ignored_count",
            "stale_count",
        ):
            self.assertIn(column, learning_columns)

    def test_03_legacy_schema_migrates_forward(self) -> None:
        # Drop the v3.2 DB this test class created so we can seed a v3.1 DB
        # in its place and prove the migration path.
        reflect_db.close_all()
        if self.db_path.exists():
            self.db_path.unlink()

        legacy = sqlite3.connect(self.db_path)
        legacy.executescript(
            """
            CREATE TABLE learnings (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                category TEXT NOT NULL DEFAULT 'Unknown',
                confidence TEXT NOT NULL DEFAULT 'LOW',
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'approved', 'rejected', 'indexed', 'reverted')),
                source_tool TEXT NOT NULL DEFAULT '',
                source_path TEXT NOT NULL DEFAULT '',
                content_hash TEXT NOT NULL DEFAULT '',
                commit_hash TEXT,
                created_at TEXT NOT NULL,
                approved_at TEXT,
                indexed_at TEXT,
                reverted_at TEXT,
                revert_reason TEXT
            );
            CREATE TABLE proposals (
                id TEXT PRIMARY KEY,
                learning_id TEXT NOT NULL REFERENCES learnings(id),
                agent_file TEXT NOT NULL DEFAULT '',
                diff TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'accepted', 'rejected')),
                created_at TEXT NOT NULL
            );
            CREATE TABLE metrics (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE events (
                id TEXT PRIMARY KEY,
                type TEXT NOT NULL,
                learning_id TEXT,
                details_json TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL
            );
            CREATE TABLE sources (
                id TEXT PRIMARY KEY,
                provider TEXT NOT NULL,
                path TEXT NOT NULL,
                project_name TEXT NOT NULL DEFAULT '',
                content_hash TEXT NOT NULL DEFAULT '',
                last_seen TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'stale', 'archived'))
            );
            """
        )
        legacy.execute(
            """
            INSERT INTO learnings
                (id, title, status, source_tool, source_path, content_hash, created_at)
            VALUES
                ('lrn-legacy', 'Legacy learning', 'pending', 'claude', '/tmp/memory.md',
                 'abc123', '2026-04-20T00:00:00+00:00')
            """
        )
        legacy.commit()
        legacy.close()

        conn = reflect_db.init_db(self.db_path)
        reflect_db.update_learning_status(
            "lrn-legacy",
            LearningStatus.RECALLED.value,
            conn=conn,
        )
        updated = conn.execute(
            "SELECT status, last_recalled_at FROM learnings WHERE id = 'lrn-legacy'"
        ).fetchone()
        self.assertEqual(updated["status"], LearningStatus.RECALLED.value)
        self.assertTrue(updated["last_recalled_at"])

        proposal_sql = conn.execute(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='proposals'"
        ).fetchone()["sql"]
        self.assertIn("materialized", proposal_sql)

    def test_04_add_learning_persists_provenance(self) -> None:
        conn = reflect_db.get_conn(self.db_path)
        lid = reflect_db.add_learning(
            title="Prefer uv over pip",
            category="tooling",
            confidence="HIGH",
            source_tool="codex",
            source_provider="codex",
            source_kind="memory_note",
            source_path="/tmp/codex/memories/python.md",
            source_quote="Prefer uv over pip for speed.",
            content_hash="deadbeefcafefeed",
            session_id="sess-123",
            thread_id="thread-456",
            privacy_level=PrivacyLevel.RESTRICTED.value,
            artifact_path="docs/solutions/uv.md",
            sidecar_path="docs/solutions/uv.entities.yaml",
            conn=conn,
        )
        row = conn.execute(
            """SELECT source_provider, source_kind, source_quote, source_quote_hash,
                      session_id, thread_id, privacy_level, artifact_path, sidecar_path
               FROM learnings WHERE id = ?""",
            (lid,),
        ).fetchone()
        self.assertEqual(row["source_provider"], "codex")
        self.assertEqual(row["source_kind"], "memory_note")
        self.assertEqual(row["source_quote"], "Prefer uv over pip for speed.")
        self.assertEqual(len(row["source_quote_hash"]), 16)
        self.assertEqual(row["session_id"], "sess-123")
        self.assertEqual(row["thread_id"], "thread-456")
        self.assertEqual(row["privacy_level"], PrivacyLevel.RESTRICTED.value)
        self.assertTrue(row["artifact_path"].endswith("uv.md"))
        self.assertTrue(row["sidecar_path"].endswith("uv.entities.yaml"))

    def test_05_add_event_idempotency_key_is_stable(self) -> None:
        conn = reflect_db.get_conn(self.db_path)
        eid1 = reflect_db.add_event(
            "index_requested",
            "lrn-xyz",
            {"backend": "qmd"},
            actor="reflect:ingest",
            idempotency_key="job-123",
            conn=conn,
        )
        eid2 = reflect_db.add_event(
            "index_requested",
            "lrn-xyz",
            {"backend": "qmd"},
            actor="reflect:ingest",
            idempotency_key="job-123",
            conn=conn,
        )
        self.assertEqual(eid1, eid2)
        count = conn.execute(
            "SELECT COUNT(*) AS n FROM events WHERE idempotency_key = 'job-123'"
        ).fetchone()["n"]
        self.assertEqual(count, 1)

    def test_06_recall_event_updates_learning_counters(self) -> None:
        conn = reflect_db.get_conn(self.db_path)
        lid = reflect_db.add_learning(title="Cache pip wheels in CI", conn=conn)
        reflect_db.add_recall_event(
            lid,
            "python ci speedup",
            rank=1,
            feedback="helpful",
            conn=conn,
        )
        row = conn.execute(
            """SELECT recall_count, helpful_count, ignored_count, stale_count,
                      last_recalled_at
               FROM learnings WHERE id = ?""",
            (lid,),
        ).fetchone()
        self.assertEqual(row["recall_count"], 1)
        self.assertEqual(row["helpful_count"], 1)
        self.assertEqual(row["ignored_count"], 0)
        self.assertEqual(row["stale_count"], 0)
        self.assertTrue(row["last_recalled_at"])

    def test_07_source_upsert_tracks_provider_identity(self) -> None:
        conn = reflect_db.get_conn(self.db_path)
        sid = reflect_db.upsert_source(
            provider="claude",
            path="/tmp/claude/projects/myrepo/memory/MEMORY.md",
            project_name="myrepo",
            source_kind="memory_file",
            provider_id="claude-local",
            canonical_project_id="github.com/acme/myrepo",
            content_hash="1111aaaa2222bbbb",
            ingest_state="discovered",
            conn=conn,
        )
        sid2 = reflect_db.upsert_source(
            provider="claude",
            path="/tmp/claude/projects/myrepo/memory/MEMORY.md",
            project_name="myrepo",
            source_kind="memory_file",
            provider_id="claude-local",
            canonical_project_id="github.com/acme/myrepo",
            content_hash="3333cccc4444dddd",
            ingest_state="indexed",
            conn=conn,
        )
        self.assertEqual(sid, sid2)
        row = conn.execute(
            """SELECT provider_id, canonical_project_id, source_kind, ingest_state,
                      first_seen, last_seen
               FROM sources WHERE id = ?""",
            (sid,),
        ).fetchone()
        self.assertEqual(row["provider_id"], "claude-local")
        self.assertEqual(row["canonical_project_id"], "github.com/acme/myrepo")
        self.assertEqual(row["source_kind"], "memory_file")
        self.assertEqual(row["ingest_state"], "indexed")
        self.assertTrue(row["first_seen"])
        self.assertTrue(row["last_seen"])

    def test_08_index_jobs_and_artifacts_roundtrip(self) -> None:
        conn = reflect_db.get_conn(self.db_path)
        lid = reflect_db.add_learning(title="Ship note sidecars", conn=conn)
        jid = reflect_db.add_index_job(
            lid,
            "graphrag",
            idempotency_key="index-lrn-1",
            conn=conn,
        )
        aid = reflect_db.add_artifact(
            lid,
            "entity_sidecar",
            "docs/solutions/ship-note.entities.yaml",
            content_hash="abc12345abc12345",
            conn=conn,
        )
        job = conn.execute(
            "SELECT backend, status FROM index_jobs WHERE id = ?",
            (jid,),
        ).fetchone()
        artifact = conn.execute(
            "SELECT artifact_type, status FROM artifacts WHERE id = ?",
            (aid,),
        ).fetchone()
        self.assertEqual(job["backend"], "graphrag")
        self.assertEqual(job["status"], "pending")
        self.assertEqual(artifact["artifact_type"], "entity_sidecar")
        self.assertEqual(artifact["status"], "created")

    def test_09_init_db_is_idempotent_on_v32_db(self) -> None:
        """Re-running init_db on an already-migrated v3.2 DB must be a no-op.

        Regression test against migration loops that re-add columns or rebuild
        tables when called twice in the same process.
        """
        conn = reflect_db.get_conn(self.db_path)
        lid = reflect_db.add_learning(
            title="Idempotent init",
            confidence="HIGH",
            conn=conn,
        )
        before_columns = {
            row["name"] for row in conn.execute("PRAGMA table_info(learnings)")
        }
        before_indexes = {
            row["name"]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='index'"
            )
        }

        # Drop the cached connection without deleting the DB file, then re-init.
        reflect_db.close_all()
        conn2 = reflect_db.init_db(self.db_path)

        after_columns = {
            row["name"] for row in conn2.execute("PRAGMA table_info(learnings)")
        }
        after_indexes = {
            row["name"]
            for row in conn2.execute(
                "SELECT name FROM sqlite_master WHERE type='index'"
            )
        }
        self.assertEqual(before_columns, after_columns)
        self.assertEqual(before_indexes, after_indexes)

        # Row written before the second init_db is still there.
        row = reflect_db.get_learning(lid, conn=conn2)
        self.assertIsNotNone(row)
        self.assertEqual(row["title"], "Idempotent init")

    def test_10_increment_metric_is_atomic_and_handles_non_numeric(self) -> None:
        """Single-SQL upsert: no read-modify-write race, non-numeric coerces to 0."""
        conn = reflect_db.get_conn(self.db_path)

        # Fresh key — first increment seeds value to delta.
        result = reflect_db.increment_metric("recall_hits", 1, conn=conn)
        self.assertEqual(result, 1)
        self.assertEqual(reflect_db.get_metric("recall_hits", conn=conn), 1)

        # Multiple increments accumulate without losing updates.
        for _ in range(5):
            reflect_db.increment_metric("recall_hits", 1, conn=conn)
        self.assertEqual(reflect_db.get_metric("recall_hits", conn=conn), 6)

        # Negative delta works.
        reflect_db.increment_metric("recall_hits", -2, conn=conn)
        self.assertEqual(reflect_db.get_metric("recall_hits", conn=conn), 4)

        # Non-numeric existing value coerces to 0 before the addition.
        reflect_db.set_metric("last_reflection", "2026-04-28T12:00:00Z", conn=conn)
        result = reflect_db.increment_metric("last_reflection", 7, conn=conn)
        self.assertEqual(result, 7)


if __name__ == "__main__":
    unittest.main(verbosity=2)
