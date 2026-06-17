# ABOUTME: Behavioral proof for S5 — belief revision on ingest (CREATE/UPDATE/DELETE).
# ABOUTME: Drives the REAL reflect_cascade.execute_revision_actions + recall_related_learnings
# ABOUTME: against an on-disk reflect_db: UPDATE supersedes the prior belief AS EVIDENCE
# ABOUTME: (proof_count++, source appended, S6 history snapshot), DELETE tombstones it
# ABOUTME: non-destructively (status->reverted) so it is no longer recalled as current.
"""S5 belief-revision-on-ingest proof.

Port S5 (bead agents-in-a-box-kdo.8) is a STORAGE/STATE port. Its behaviour lives
in ``plugins/reflect/scripts/reflect_cascade.py`` — NOT in the file-engine recall
pipeline — so this proof drives the real module + the real on-disk
``reflect_db`` sqlite store directly. No LLM, no torch model, no vector engine is
involved: the seeds plus the literal action objects fully determine every
asserted outcome. (The drain LLM only *chooses* which action to emit in
production; here we hand the executor the actions verbatim, so the assertions
test the executor's deterministic state transitions, never an LLM decision.)

The TRUE invariant (corrected against the real diff — the hypothesis guessed
``is_latest``/``superseded_by``, but S5's executor does NOT touch those columns):

  reflect_cascade.execute_revision_actions applies the structured
  CREATE/UPDATE/DELETE action contract to the learnings DB such that:

  1. UPDATE-SUPERSEDES-AS-EVIDENCE: an UPDATE of an existing learning does NOT
     write a duplicate note. It merges the new transcript as evidence via
     reflect_db.add_learning_proof — proof_count increments (1 -> 2), the new
     source id is appended to source_memory_ids, and an S6 history snapshot of
     the PRIOR form is recorded. The single-proof belief as-first-written is
     thereby superseded by a strengthened, two-proof belief; the row count does
     NOT grow. summary['updated'] == 1.

  2. DELETE-TOMBSTONES (non-destructive): a DELETE flips status to 'reverted'
     with the action's reason, the row PERSISTS (ledger keeps it so "why was
     this retired?" stays answerable), and — because 'reverted' is in
     reflect_cascade._RETIRED_STATUSES — the tombstoned belief is NO LONGER
     surfaced by recall_related_learnings as a revision candidate, while a live
     CREATE'd sibling with the same topic IS. summary['deleted'] == 1.

  3. KNOB / FALSIFIABLE CONTROL — idempotency by source id: re-running the SAME
     UPDATE with the SAME source_memory_id is an idempotent no-op
     (add_learning_proof returns False). proof_count stays at 2, no second
     history snapshot for that source, summary['skipped'] == 1 and
     summary['updated'] == 0. This pins that the supersession-as-evidence is
     driven by the structured action + source identity, NOT by text luck or a
     blind "bump on every call" — flipping the source id back to a NEW one DOES
     bump again (knob OFF -> ON), proving the executor caused the difference.

Falsifiability: if the UPDATE path were the old "always CREATE" behaviour,
assertion 1 would see a SECOND row and proof_count still 1. If DELETE were a
hard delete (Hindsight's shape), assertion 2's get_learning would return None
instead of a reverted row. If DELETE did not retire to a _RETIRED_STATUS,
recall_related_learnings would still surface the tombstoned belief. If the
executor bumped blindly, assertion 3's idempotent re-run would inflate
proof_count to 3.

PORT: S5
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# The reflect plugin scripts live alongside reflect-kb/. Resolve them the same
# way the SG1 storage proof does so this runs from either checkout layout.
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


class _Sig:
    """Minimal signal shape recall_related_learnings reads (.signal/.source_quote)."""

    def __init__(self, signal: str, source_quote: str = ""):
        self.signal = signal
        self.source_quote = source_quote
        self.line_number = 1


@pytest.fixture
def db(tmp_path, monkeypatch):
    """Fresh isolated on-disk reflect DB wired as the MODULE-DEFAULT connection.

    reflect_cascade's executor and recall call reflect_db helpers WITHOUT a
    conn= argument (production shape), so they resolve via reflect_db.get_conn.
    Pointing get_conn at this sandbox makes the real module drive THIS db, not
    the developer's ~/.reflect.
    """
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


def _row(conn, lid):
    return reflect_db.get_learning(lid, conn=conn)


def _learning_count(conn) -> int:
    return conn.execute("SELECT COUNT(*) FROM learnings").fetchone()[0]


def test_S5_update_supersedes_prior_belief_as_evidence(db):
    """UPDATE merges new evidence into the prior belief — no duplicate, proof++."""
    conn = db

    # ---- seed a learning (the prior belief, proof_count starts at 1, S4). ----
    lid = reflect_db.add_learning(
        title="Use uv instead of pip for python deps",
        category="tooling",
        confidence="high",
        scope="project",
        source_memory_ids=["transcript-A"],
        conn=conn,
    )
    before = _row(conn, lid)
    assert before["proof_count"] == 1, "seed should start at one proof (S4 CREATE semantics)"
    assert _learning_count(conn) == 1

    # ---- ingest an UPDATE of it (the drain decided UPDATE over CREATE). ----
    summary = reflect_cascade.execute_revision_actions(
        [{
            "action": "UPDATE",
            "target_id": lid,
            "reason": "session restated the uv-over-pip rule",
        }],
        source_memory_id="transcript-B",
    )

    assert summary["updated"] == 1 and summary["created"] == 0 and summary["errors"] == [], (
        f"UPDATE must merge as evidence, not create a duplicate; got {summary}"
    )

    after = _row(conn, lid)
    # PROOF-COUNT SUPERSEDES THE SINGLE-PROOF BELIEF: 1 -> 2, no new row.
    assert after["proof_count"] == 2, (
        "UPDATE must increment proof_count on the existing learning (evidence "
        f"merge), not leave it at 1; got {after['proof_count']}. If S5 still did "
        "the old 'always CREATE', this would stay 1 and a 2nd row would appear."
    )
    assert _learning_count(conn) == 1, (
        "UPDATE must NOT write a duplicate note — the whole point of the port; "
        f"row count grew to {_learning_count(conn)}"
    )
    sources = json.loads(after["source_memory_ids"])
    assert "transcript-A" in sources and "transcript-B" in sources, (
        f"the new transcript must be appended as evidence; got {sources}"
    )

    # S6: the PRIOR form (the superseded single-proof belief) is archived.
    history = reflect_db.get_learning_history(lid, conn=conn)
    assert any(h["change_type"] == "proof_added" for h in history), (
        "an S6 history snapshot of the prior belief must be recorded on UPDATE; "
        f"got change_types {[h['change_type'] for h in history]}"
    )


def test_S5_delete_tombstones_and_drops_from_recall(db):
    """DELETE non-destructively retires; the tombstone is no longer recalled."""
    conn = db

    stale = reflect_db.add_learning(
        title="Always disable the cache header in prod",
        category="config",
        confidence="medium",
        scope="project",
        conn=conn,
    )
    # A live sibling on the SAME topic — the control that MUST stay recallable.
    live = reflect_db.add_learning(
        title="Enable the cache header in prod for static assets",
        category="config",
        confidence="medium",
        scope="project",
        conn=conn,
    )

    # Pre-DELETE: a topical signal surfaces BOTH as revision candidates.
    sig = [_Sig("revisiting the cache header in prod policy",
                source_quote="the cache header in prod")]
    pre = {r["id"] for r in reflect_cascade.recall_related_learnings(sig)}
    assert stale in pre and live in pre, (
        f"both topical learnings should be recall candidates before the delete; got {pre}"
    )

    # ---- ingest a DELETE of the stale belief (new evidence supersedes it). ----
    summary = reflect_cascade.execute_revision_actions(
        [{
            "action": "DELETE",
            "target_id": stale,
            "reason": "superseded: prod now ENABLES the cache header",
        }],
    )
    assert summary["deleted"] == 1 and summary["errors"] == [], (
        f"DELETE must retire exactly one learning; got {summary}"
    )

    # NON-DESTRUCTIVE TOMBSTONE: row persists, status -> reverted, reason kept.
    tomb = _row(conn, stale)
    assert tomb is not None, (
        "DELETE must be non-destructive (status flip), NOT a hard delete — the "
        "ledger keeps the row so 'why retired?' stays answerable"
    )
    assert tomb["status"] == "reverted", (
        f"the tombstoned belief must be status='reverted'; got {tomb['status']!r}"
    )
    assert tomb["revert_reason"] == "superseded: prod now ENABLES the cache header", (
        f"the retire reason must be persisted; got {tomb['revert_reason']!r}"
    )

    # NO LONGER RETURNED AS CURRENT: the retired belief drops out of recall,
    # the live sibling survives. This is the "not returned as current" half.
    post = {r["id"] for r in reflect_cascade.recall_related_learnings(sig)}
    assert stale not in post, (
        "a tombstoned (reverted) belief must NOT be surfaced as a current "
        f"revision candidate — _RETIRED_STATUSES excludes it; got {post}"
    )
    assert live in post, (
        "the live sibling on the same topic must still be recallable, proving the "
        f"exclusion is status-driven, not a topic-wide drop; got {post}"
    )


def test_S5_update_idempotent_by_source_no_inflation(db):
    """Knob/control: supersession-as-evidence keys on source id, not text luck.

    Re-running the SAME UPDATE with the SAME source is an idempotent no-op
    (proof_count frozen). Flipping the source id to a NEW one bumps again —
    proving the executor's behaviour is determined by the structured action +
    source identity, not by repeated calls inflating evidence.
    """
    conn = db

    lid = reflect_db.add_learning(
        title="Prefer ast-grep over grep for structural code search",
        category="tooling",
        confidence="high",
        scope="project",
        source_memory_ids=["t0"],
        conn=conn,
    )
    assert _row(conn, lid)["proof_count"] == 1

    # First UPDATE with source t1 -> bumps to 2 (knob ON).
    s1 = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid, "reason": "restated"}],
        source_memory_id="t1",
    )
    assert s1["updated"] == 1 and _row(conn, lid)["proof_count"] == 2, (
        f"first UPDATE (new source t1) must bump proof_count to 2; got {s1}"
    )

    # SAME UPDATE, SAME source t1 again -> idempotent no-op (knob effectively OFF
    # for evidence: source already recorded). proof_count MUST stay 2.
    s2 = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid, "reason": "restated again"}],
        source_memory_id="t1",
    )
    assert s2["updated"] == 0 and s2["skipped"] == 1, (
        f"re-UPDATE with an already-recorded source must be skipped, not bump; got {s2}"
    )
    assert _row(conn, lid)["proof_count"] == 2, (
        "evidence must NOT inflate on a duplicate source — the idempotency guard "
        f"is load-bearing; proof_count became {_row(conn, lid)['proof_count']}"
    )

    # Flip the source id to a genuinely NEW one -> bumps again to 3 (knob ON),
    # proving the executor DID cause the bump, not text/topic coincidence.
    s3 = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid, "reason": "new corroborating session"}],
        source_memory_id="t2",
    )
    assert s3["updated"] == 1 and _row(conn, lid)["proof_count"] == 3, (
        f"a new source id must bump proof_count to 3, proving source-keyed evidence "
        f"merge (not blind/text-driven); got {s3} count={_row(conn, lid)['proof_count']}"
    )
