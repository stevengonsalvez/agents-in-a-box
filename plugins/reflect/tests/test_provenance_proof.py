# ABOUTME: Regression tests for port S4 — provenance + proof_count first-class.
# ABOUTME: Pins CREATE=1 / UPDATE-increments semantics, source-id uniqueness,
# ABOUTME: legacy DB migration, the cascade dup-evidence path, and the
# ABOUTME: proof-count boost in recall rerank (hindsight reranking, alpha=0.1).
"""Port S4: every learning carries source_memory_ids + proof_count.

Acceptance criteria pinned here:
  1. UPDATE on an existing learning increments proof_count
  2. source_ids stay unique
  3. proof-count boost active in rerank
"""

from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
sys.path.insert(0, str(SCRIPTS))
sys.path.insert(0, str(RECALL_SCRIPTS))

import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402
from recall import Learning, PROOF_COUNT_ALPHA, proof_count_boost, rerank  # noqa: E402


@pytest.fixture
def conn(tmp_path):
    """Fresh isolated DB per test; never touches ~/.reflect."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    yield connection
    reflect_db.close_all()


def _sources(conn, lid: str) -> list[str]:
    row = reflect_db.get_learning(lid, conn=conn)
    return json.loads(row["source_memory_ids"])


def _proof(conn, lid: str) -> int:
    return reflect_db.get_learning(lid, conn=conn)["proof_count"]


# ---------- CREATE: proof_count starts at 1 ----------

def test_create_defaults_proof_count_one(conn):
    lid = reflect_db.add_learning("Prefer uv over pip", conn=conn)
    assert _proof(conn, lid) == 1
    assert _sources(conn, lid) == []


def test_create_stores_unique_source_ids(conn):
    lid = reflect_db.add_learning(
        "tmux kill-server is forbidden",
        source_memory_ids=["mem-a", "mem-b", "mem-a", "  ", "mem-b"],
        conn=conn,
    )
    assert _sources(conn, lid) == ["mem-a", "mem-b"]


def test_create_clamps_proof_count_to_minimum_one(conn):
    lid = reflect_db.add_learning("clamped", proof_count=0, conn=conn)
    assert _proof(conn, lid) == 1


def test_create_accepts_pre_aggregated_proof_count(conn):
    lid = reflect_db.add_learning("aggregated", proof_count=4, conn=conn)
    assert _proof(conn, lid) == 4


# ---------- UPDATE: append source + bump count ----------

def test_update_increments_proof_count_and_appends_source(conn):
    lid = reflect_db.add_learning("rule", source_memory_ids=["mem-1"], conn=conn)
    assert reflect_db.add_learning_proof(lid, "mem-2", conn=conn) is True
    assert _proof(conn, lid) == 2
    assert _sources(conn, lid) == ["mem-1", "mem-2"]


def test_update_same_source_is_idempotent(conn):
    """source_ids stay unique — re-ingesting one transcript can't inflate proof."""
    lid = reflect_db.add_learning("rule", source_memory_ids=["mem-1"], conn=conn)
    assert reflect_db.add_learning_proof(lid, "mem-1", conn=conn) is False
    assert _proof(conn, lid) == 1
    assert _sources(conn, lid) == ["mem-1"]


def test_update_repeated_distinct_sources_keep_counting(conn):
    lid = reflect_db.add_learning("rule", conn=conn)
    for i in range(5):
        assert reflect_db.add_learning_proof(lid, f"mem-{i}", conn=conn) is True
    assert _proof(conn, lid) == 6
    assert _sources(conn, lid) == [f"mem-{i}" for i in range(5)]
    assert len(set(_sources(conn, lid))) == 5


def test_update_anonymous_evidence_bumps_count_only(conn):
    lid = reflect_db.add_learning("rule", conn=conn)
    assert reflect_db.add_learning_proof(lid, "", conn=conn) is True
    assert _proof(conn, lid) == 2
    assert _sources(conn, lid) == []


def test_update_missing_learning_is_noop(conn):
    assert reflect_db.add_learning_proof("nope", "mem-1", conn=conn) is False


def test_update_writes_proof_added_audit_event(conn):
    lid = reflect_db.add_learning("rule", conn=conn)
    reflect_db.add_learning_proof(lid, "mem-9", conn=conn)
    events = reflect_db.get_events_by_type("proof_added", conn=conn)
    assert len(events) == 1
    details = json.loads(events[0]["details_json"])
    assert details["source_memory_id"] == "mem-9"
    assert details["proof_count"] == 2


# ---------- migration: legacy DBs gain the columns ----------

def test_legacy_db_migrates_to_proof_columns(tmp_path):
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
        VALUES ('old-1', 'pre-S4 learning', '2026-01-01T00:00:00+00:00');
        """
    )
    legacy.commit()
    legacy.close()

    conn = reflect_db.init_db(db_file)
    try:
        row = reflect_db.get_learning("old-1", conn=conn)
        assert row["proof_count"] == 1
        assert json.loads(row["source_memory_ids"]) == []
        # And the UPDATE path works on migrated rows.
        assert reflect_db.add_learning_proof("old-1", "mem-x", conn=conn) is True
        assert reflect_db.get_learning("old-1", conn=conn)["proof_count"] == 2
    finally:
        reflect_db.close_all()


# ---------- cascade: dup signal hash = new evidence ----------

def test_cascade_dup_signal_bumps_proof(tmp_path, monkeypatch):
    """A skipped duplicate transcript must strengthen the matching learning."""
    db_file = tmp_path / "cascade.db"
    conn = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: conn)

    transcript = tmp_path / "dup.jsonl"
    transcript.write_text(
        json.dumps({"message": {
            "role": "user",
            "content": "No, never use var here. The root cause was a missing index.",
        }}) + "\n"
    )

    # First pass: signal set is new → reflect; capture its hash.
    first = reflect_cascade.prepare(transcript, out_path=str(tmp_path / "s.txt"))
    assert first.action == "reflect"
    assert first.signal_hash

    lid = reflect_db.add_learning(
        "never use var", content_hash=first.signal_hash,
        source_memory_ids=["mem-orig"], conn=conn,
    )

    # Second pass: dup hash → skip, but proof is recorded with the transcript
    # path as the source memory id.
    second = reflect_cascade.prepare(transcript)
    assert second.action == "skip" and second.reason == "dup-signal-hash"
    assert second.proof_bumped == 1
    assert _proof(conn, lid) == 2
    assert _sources(conn, lid) == ["mem-orig", str(transcript)]

    # Third pass: same transcript again → unique source ids, no inflation.
    third = reflect_cascade.prepare(transcript)
    assert third.action == "skip" and third.proof_bumped == 0
    assert _proof(conn, lid) == 2
    assert _sources(conn, lid) == ["mem-orig", str(transcript)]

    reflect_db.close_all()


def test_cascade_proof_recording_fails_silently(tmp_path, monkeypatch):
    """DB unavailable → the dedup skip still happens (silent-fail contract)."""
    def boom(*a, **k):
        raise RuntimeError("db gone")

    monkeypatch.setattr(reflect_cascade, "_signal_hash_seen", lambda h: True)
    monkeypatch.setattr(reflect_db, "get_learnings_by_content_hash", boom)
    transcript = tmp_path / "sig.jsonl"
    transcript.write_text(
        json.dumps({"message": {
            "role": "user",
            "content": "No, never use var. The root cause was a missing index.",
        }}) + "\n"
    )
    prep = reflect_cascade.prepare(transcript)
    assert prep.action == "skip" and prep.reason == "dup-signal-hash"
    assert prep.proof_bumped == 0


# ---------- rerank: proof-count boost (acceptance bullet 3) ----------

def _lrn(name: str, proof: int | None = None, nested: bool = False) -> Learning:
    fm: dict = {"name": name, "confidence": "high"}
    if proof is not None:
        if nested:
            fm["provenance"] = {"proof_count": proof}
        else:
            fm["proof_count"] = proof
    return Learning(chunk_text=f"learning body for {name}", frontmatter=fm)


def test_boost_neutral_for_missing_and_single_proof():
    assert proof_count_boost(None) == pytest.approx(1.0)
    assert proof_count_boost(1) == pytest.approx(1.0)
    assert proof_count_boost(0) == pytest.approx(1.0)  # invalid → neutral


def test_boost_monotonic_and_bounded_to_five_percent():
    assert proof_count_boost(2) > proof_count_boost(1)
    assert proof_count_boost(20) > proof_count_boost(2)
    # ln clamps at proof_norm=1.0: ceiling is exactly 1 + alpha/2 = 1.05
    assert proof_count_boost(10**9) == pytest.approx(1.0 + PROOF_COUNT_ALPHA / 2)
    for pc in (None, 1, 2, 20, 10**9):
        assert 1.0 <= proof_count_boost(pc) <= 1.05


def test_rerank_prefers_well_evidenced_learning():
    weak = _lrn("weak", proof=1)
    strong = _lrn("strong", proof=50)
    out = rerank([weak, strong])
    assert out[0] is strong, "higher proof_count must win between near-ties"


def test_rerank_reads_proof_count_from_provenance_block():
    weak = _lrn("weak")  # no proof_count anywhere → neutral
    strong = _lrn("strong", proof=50, nested=True)
    assert strong.proof_count == 50
    out = rerank([weak, strong])
    assert out[0] is strong


def test_rerank_neutral_for_legacy_notes():
    """Learnings without proof_count score exactly as before the port."""
    a = _lrn("a")
    b = _lrn("b")
    out = rerank([a, b])
    assert out == [a, b], "no proof data → original (stable) ordering preserved"


def test_proof_count_property_tolerates_garbage():
    assert Learning("x", {"proof_count": "seven"}).proof_count is None
    assert Learning("x", {"proof_count": True}).proof_count is None
    assert Learning("x", {"provenance": "not-a-dict"}).proof_count is None
    assert Learning("x", {"proof_count": "3"}).proof_count == 3


# ---------- template: frontmatter carries the fields ----------

def test_learning_template_declares_provenance_fields():
    template = (PLUGIN_ROOT / "assets" / "learning_template.md").read_text()
    assert "proof_count: 1" in template
    assert "source_memory_ids:" in template


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
