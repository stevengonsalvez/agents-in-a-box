# ABOUTME: Behavioral proof for port S6 — history snapshot of the PRIOR form on UPDATE.
# ABOUTME: Drives the REAL reflect_db module over a real sqlite DB: an UPDATE archives the
# ABOUTME: pre-mutation row into learning_history (capturing the OLD status) while the live
# ABOUTME: row carries the NEW form; a freshly CREATEd-but-never-UPDATEd learning writes
# ABOUTME: zero history rows — so the snapshot is provably caused by the UPDATE path itself.
"""Port S6: non-destructive belief revision via prior-form history snapshots.

INVARIANT (storage surface, decisive by knob = whether an UPDATE happened):
  When a learning is UPDATEd through the real reflect_db UPDATE paths
  (`update_learning_status`, `add_learning_proof`), a snapshot of the
  learning's PRIOR form is appended to the `learning_history` table BEFORE
  the live `learnings` row is mutated. The snapshot's `snapshot_json` holds
  the OLD field values (e.g. the old status / old proof_count), while the
  live row afterwards carries the NEW values. A learning that is only
  CREATEd and never UPDATEd has ZERO history rows — proving the snapshot is
  caused by the S6 UPDATE hook, not by mere row existence (CREATE writes no
  history).

WHY NO LLM: the outcome is fully determined by the seeded literals and the
explicit UPDATE call. snapshot_learning_history copies the current row into
learning_history via plain sqlite INSERT inside the same transaction as the
UPDATE; reading it back is a plain SELECT. No model, no ranker, no LLM
participates in seeding, acting, or asserting. The before/after status values
are literals we set; the audit row's content is a deterministic JSON dump of
the pre-mutation row.

This is a *storage*-surface port, so per the harness contract we drive the
real reflect_db module directly over a real (temp) sqlite DB rather than the
file-KB recall harness — recall never reads learning_history.

Each test arm builds its OWN fresh temp DB; no DB state is shared across arms.

PORT: S6
"""
from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

import pytest

# Locate the real reflect plugin scripts (../../../../../plugins/reflect/scripts
# from this proof file under reflect-kb/tests/eval/behavioral/proofs/).
_REPO_ROOT = Path(__file__).resolve().parents[5]
_SCRIPTS = _REPO_ROOT / "plugins" / "reflect" / "scripts"
assert _SCRIPTS.is_dir(), f"reflect scripts dir not found: {_SCRIPTS}"
sys.path.insert(0, str(_SCRIPTS))

import reflect_db  # noqa: E402  (real production module under test)


@pytest.fixture
def db(tmp_path) -> sqlite3.Connection:
    """A fresh, isolated reflect.db per test arm — never touches ~/.reflect."""
    conn = reflect_db.get_conn(tmp_path / "reflect.db")
    try:
        yield conn
    finally:
        conn.close()


def _seed_learning(conn: sqlite3.Connection, *, status: str) -> str:
    """Insert one learning with a pinned status; return its id."""
    return reflect_db.add_learning(
        "S6 proof: always pin the clock in TTL tests",
        category="Testing",
        confidence="MEDIUM",
        source_tool="proof-harness",
        status=status,
        conn=conn,
    )


def test_status_update_snapshots_prior_form(db):
    """ACT = UPDATE status: history captures the OLD status; live row = NEW.

    Knob-ON arm. The decisive observable is that the audit snapshot holds the
    PRE-mutation status ('pending') while the live row holds the post-mutation
    status ('approved').
    """
    lid = _seed_learning(db, status="pending")

    # Sanity: CREATE alone writes no history (the control baseline, in-arm).
    assert reflect_db.get_learning_history(lid, conn=db) == []

    # ACT: the real S6 UPDATE path.
    reflect_db.update_learning_status(lid, "approved", conn=db)

    # ASSERT live row carries the NEW form.
    live = reflect_db.get_learning(lid, conn=db)
    assert live is not None
    assert live["status"] == "approved"

    # ASSERT a single history snapshot exists capturing the PRIOR form.
    history = reflect_db.get_learning_history(lid, conn=db)
    assert len(history) == 1, history
    snap = history[0]
    assert snap["learning_id"] == lid
    assert snap["change_type"] == "status_change"
    assert "status" in json.loads(snap["changed_fields"])

    prior = json.loads(snap["snapshot_json"])
    assert prior["id"] == lid
    # The snapshot is the PRIOR form: old status, NOT the new one.
    assert prior["status"] == "pending"
    assert prior["status"] != live["status"]


def test_create_only_writes_no_history(db):
    """KNOB-OFF control: a learning that is only CREATEd, never UPDATEd,
    has ZERO history rows. Proves the snapshot is caused by the UPDATE
    path, not by row existence."""
    lid = _seed_learning(db, status="pending")

    # No UPDATE performed.
    assert reflect_db.get_learning_history(lid, conn=db) == []
    # And the global history table is empty in this fresh DB.
    total = db.execute("SELECT COUNT(*) FROM learning_history").fetchone()[0]
    assert total == 0


def test_proof_added_snapshots_prior_proof_count(db):
    """A second real UPDATE path (add_learning_proof) also snapshots the
    PRIOR form: the audit row holds the OLD proof_count while the live row
    holds the incremented count. Fresh DB — no cross-arm state."""
    lid = reflect_db.add_learning(
        "S6 proof: provenance bump archives prior count",
        category="Testing",
        confidence="MEDIUM",
        source_tool="proof-harness",
        source_memory_ids=["mem-aaa"],
        proof_count=1,
        conn=db,
    )
    assert reflect_db.get_learning_history(lid, conn=db) == []

    # ACT: real S6-instrumented provenance UPDATE.
    updated = reflect_db.add_learning_proof(lid, "mem-bbb", conn=db)
    assert updated is True

    live = reflect_db.get_learning(lid, conn=db)
    assert live is not None
    assert int(live["proof_count"]) == 2

    history = reflect_db.get_learning_history(lid, conn=db)
    assert len(history) == 1, history
    snap = history[0]
    assert snap["change_type"] == "proof_added"
    assert "proof_count" in json.loads(snap["changed_fields"])

    prior = json.loads(snap["snapshot_json"])
    # Snapshot holds the PRIOR proof_count (1), live row holds the new (2).
    assert int(prior["proof_count"]) == 1
    assert int(prior["proof_count"]) != int(live["proof_count"])
