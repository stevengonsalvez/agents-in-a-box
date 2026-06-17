# ABOUTME: Behavioral proof for S8 — document -> chunks -> learnings grouping (Hindsight model).
# ABOUTME: Drives the REAL reflect_db S8 accessors (record_chunk_with_learnings,
# ABOUTME: get_learnings_for_transcript, get_transcript_grouping, chunk_already_processed) against a
# ABOUTME: hermetic on-disk reflect.db: one transcript with 2 chunks producing 3 learnings groups
# ABOUTME: under the transcript, idempotent re-record adds no dup, and a sibling transcript can't leak.
"""S8: Document -> chunks -> learnings grouping (persistence + grouping layer).

Port S8 is a STORAGE/grouping port. Its behaviour lives entirely in the reflect
plugin DB layer (``plugins/reflect/scripts/reflect_db.py``): three additive
tables — ``transcripts``, ``transcript_chunks`` (keyed UNIQUE on
``(transcript_id, hash)``, reusing the S7 slice-chunk hash), and
``chunk_learnings`` — plus the accessors that record and query them. There is no
retrieval ranking, no embedding model, and no LLM on this path, so this proof
drives the REAL ``reflect_db`` accessors against an isolated on-disk SQLite file
(``init_db(db_path)``), never the developer's ``~/.reflect``.

The TRUE invariant:

  A transcript groups every learning drained from its chunks. Recording 2 chunks
  under transcript X that produce 3 learnings total makes the grouping query
  ``get_learnings_for_transcript(X)`` return EXACTLY those 3 learning ids, and
  ``get_transcript_grouping(X)`` map each chunk hash to its own learnings. Two
  load-bearing idempotency knobs guard correctness:

    1. CHUNK NOT DOUBLE-PROCESSED — the UNIQUE (transcript_id, hash) constraint
       means re-recording the same chunk + learnings is a no-op: chunk-row count
       and learning grouping are unchanged after a repeat call. If S8 dropped the
       UNIQUE/ON-CONFLICT handling, the repeat record would duplicate rows and
       this assertion would FAIL.

    2. NO CROSS-TRANSCRIPT LEAK — a different transcript's learnings never appear
       in transcript X's grouping. If the query forgot to scope by
       ``transcript_id``, the sibling's learning would leak in and FAIL.

DECISIVE: the seeds (which learning ids are linked to which chunk under which
transcript) fully determine each assertion; no model participates. If S8 were
absent (tables/accessors missing) the import or the grouping query would error;
if it grouped by chunk-hash globally instead of per-transcript, the cross-leak
arm would fail.

PORT: S8
"""
from __future__ import annotations

import sys
from pathlib import Path

import pytest

# reflect_db lives in the reflect plugin scripts, alongside reflect-kb/. Resolve
# it from either checkout layout (same pattern the SG-series capture proofs use).
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]  # reflect-kb/tests/eval/behavioral
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next((p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0])
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import reflect_db  # noqa: E402


@pytest.fixture()
def db(tmp_path, monkeypatch):
    """Hermetic on-disk reflect.db, fully isolated from ~/.reflect.

    Points the resolved default DB path (``REFLECT_DB_PATH``) at a temp file and
    forces a config reload, so BOTH explicitly-passed connections and the
    cascade helper's no-arg ``get_conn()`` resolve to the SAME isolated DB.
    Resets the process-global connection + config caches before and after.
    """
    import reflect_config

    db_path = tmp_path / "reflect.db"
    monkeypatch.setenv("REFLECT_DB_PATH", str(db_path))
    reflect_config.load_config(force_reload=True)
    reflect_db.close_all()
    conn = reflect_db.init_db(db_path)
    try:
        yield conn
    finally:
        reflect_db.close_all()
        reflect_config.load_config(force_reload=True)


def _new_learning(conn, title: str) -> str:
    return reflect_db.add_learning(title=title, conn=conn)


def test_S8_transcript_groups_its_chunks_learnings(db):
    """One transcript, 2 chunks, 3 learnings -> grouping returns exactly those 3
    grouped under the transcript and per-chunk."""
    conn = db
    sid = "sess-S8-arm"

    # 3 learnings drained from this session: 2 from chunk-1, 1 from chunk-2.
    l1 = _new_learning(conn, "use uv not pip")
    l2 = _new_learning(conn, "WAL mode on sqlite")
    l3 = _new_learning(conn, "scope grouping query by transcript")

    # Reuse the S7 slice-chunk hash as the chunk key (here: deterministic seeds).
    reflect_db.record_chunk_with_learnings(sid, "hashA", [l1, l2], conn=conn)
    reflect_db.record_chunk_with_learnings(sid, "hashB", [l3], conn=conn)

    # "show me everything that came out of session X" returns exactly the 3.
    got = reflect_db.get_learnings_for_transcript(sid, conn=conn)
    assert sorted(got) == sorted([l1, l2, l3])
    assert len(got) == 3

    # Per-chunk grouping keeps each chunk's learnings under its own hash.
    grouping = reflect_db.get_transcript_grouping(sid, conn=conn)
    assert set(grouping.keys()) == {"hashA", "hashB"}
    assert sorted(grouping["hashA"]) == sorted([l1, l2])
    assert grouping["hashB"] == [l3]


def test_S8_chunk_not_double_processed_on_idempotent_rerecord(db):
    """Re-recording the same chunk + learnings adds no duplicate rows and does
    not change the grouping (UNIQUE (transcript_id, hash) + ON CONFLICT)."""
    conn = db
    sid = "sess-S8-idem"

    l1 = _new_learning(conn, "learning one")
    l2 = _new_learning(conn, "learning two")

    assert reflect_db.chunk_already_processed(sid, "hdup", conn=conn) is False
    reflect_db.record_chunk_with_learnings(sid, "hdup", [l1, l2], conn=conn)
    assert reflect_db.chunk_already_processed(sid, "hdup", conn=conn) is True

    rows_before = conn.execute(
        "SELECT COUNT(*) FROM transcript_chunks WHERE transcript_id = ?", (sid,)
    ).fetchone()[0]
    links_before = conn.execute("SELECT COUNT(*) FROM chunk_learnings").fetchone()[0]

    # Idempotent re-record of the identical chunk + learnings.
    reflect_db.record_chunk_with_learnings(sid, "hdup", [l1, l2], conn=conn)

    rows_after = conn.execute(
        "SELECT COUNT(*) FROM transcript_chunks WHERE transcript_id = ?", (sid,)
    ).fetchone()[0]
    links_after = conn.execute("SELECT COUNT(*) FROM chunk_learnings").fetchone()[0]

    assert rows_after == rows_before == 1, "chunk must not be double-processed"
    assert links_after == links_before == 2, "learning links must not duplicate"

    # The grouping query is still exactly {l1, l2} (deduped via DISTINCT).
    got = reflect_db.get_learnings_for_transcript(sid, conn=conn)
    assert sorted(got) == sorted([l1, l2])


def test_S8_no_cross_transcript_leak(db):
    """A sibling transcript's learnings never appear in transcript X's grouping."""
    conn = db
    x, y = "sess-S8-X", "sess-S8-Y"

    lx1 = _new_learning(conn, "X learning 1")
    lx2 = _new_learning(conn, "X learning 2")
    ly1 = _new_learning(conn, "Y learning 1")

    reflect_db.record_chunk_with_learnings(x, "xhash", [lx1, lx2], conn=conn)
    reflect_db.record_chunk_with_learnings(y, "yhash", [ly1], conn=conn)

    got_x = reflect_db.get_learnings_for_transcript(x, conn=conn)
    got_y = reflect_db.get_learnings_for_transcript(y, conn=conn)

    assert sorted(got_x) == sorted([lx1, lx2])
    assert ly1 not in got_x
    assert got_y == [ly1]
    assert lx1 not in got_y and lx2 not in got_y


def test_S8_record_drain_chunk_via_cascade_groups_learnings(db):
    """The cascade drain helper (record_drain_chunk) reuses the S7 signal_hash as
    the chunk key and persists the grouping end-to-end — no LLM, no slicing."""
    conn = db
    if str(_PLUGIN_SCRIPTS) not in sys.path:
        sys.path.insert(0, str(_PLUGIN_SCRIPTS))
    import reflect_cascade  # noqa: E402

    l1 = _new_learning(conn, "drained learning A")
    l2 = _new_learning(conn, "drained learning B")

    # A Prep as `prepare` would return it: signal_hash is the S7 slice-chunk hash.
    prep = reflect_cascade.Prep(
        action="reflect",
        reason="has-signal",
        signal_count=2,
        orig_tokens=1000,
        slice_tokens=200,
        slice_path="/tmp/slice.txt",
        signal_hash="sig-deadbeef",
    )

    ok = reflect_cascade.record_drain_chunk(
        prep, transcript_id="sess-S8-cascade", learning_ids=[l1, l2]
    )
    assert ok is True

    got = reflect_db.get_learnings_for_transcript("sess-S8-cascade", conn=conn)
    assert sorted(got) == sorted([l1, l2])
    assert reflect_db.chunk_already_processed("sess-S8-cascade", "sig-deadbeef", conn=conn)


def test_S8_record_chunk_for_transcript_links_via_real_drain_write_path(db, tmp_path, monkeypatch):
    """Regression (PR #294 review): record_chunk_for_transcript must link the
    learnings a REAL drain wrote, not assume content_hash==signal_hash.

    The drain writes learnings via `revise --source "<transcript>"`
    (execute_revision_actions CREATE), which stamps source_memory_ids=[transcript]
    and leaves content_hash=''. Driving that real write path (no LLM) and then
    record_chunk_for_transcript must group the learning under the transcript.
    The earlier content_hash-based lookup linked 0 here — this is the guard.
    """
    import reflect_cascade
    conn = db
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))

    # A transcript with a clear correction signal (so prepare/record-chunk see a hash).
    transcript = tmp_path / "t.jsonl"
    rows = [
        '{"type":"user","message":{"role":"user","content":"no, that is wrong — use os.replace not os.rename for atomic moves"}}',
        '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Right, fixing."}]}}',
    ]
    transcript.write_text("\n".join(rows) + "\n")
    tid = str(transcript)

    # REAL write path: a CREATE through execute_revision_actions with --source = transcript.
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Use os.replace for atomic file moves",
          "category": "reliability", "confidence": "0.8", "dedup_adjudicated": True}],
        source_memory_id=tid,
    )
    assert summary["created"] == 1, summary
    # The learning carries the transcript in source_memory_ids, NOT content_hash==signal_hash.
    created = reflect_db.get_learnings_by_source_memory_id(tid, conn=conn)
    assert len(created) == 1, "real CREATE must link the transcript via source_memory_ids"

    # Now the drain-time call must group it under the transcript.
    res = reflect_cascade.record_chunk_for_transcript(transcript)
    assert res.get("recorded") is True, res
    assert res.get("learnings") == 1, res
    got = reflect_db.get_learnings_for_transcript(tid, conn=conn)
    assert len(got) == 1, "the drained learning must be queryable by transcript"

    # Idempotent re-run: still exactly one, no duplicate link.
    reflect_cascade.record_chunk_for_transcript(transcript)
    assert len(reflect_db.get_learnings_for_transcript(tid, conn=conn)) == 1
