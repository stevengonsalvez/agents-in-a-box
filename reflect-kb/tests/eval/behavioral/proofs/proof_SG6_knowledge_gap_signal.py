# ABOUTME: Behavioral proof for SG6 — a 0-result recall is persisted as a knowledge-gap
# ABOUTME: signal to REFLECT_STATE_DIR/knowledge-gaps.jsonl; a hit logs nothing; RECALL_GAP_LOG=0 disables it.
"""SG6 knowledge-gap signal proof.

Invariant (``log_knowledge_gap`` + the two ``if gap_log and not learnings`` call
sites in recall.py): when a recall's FINAL result set is empty — including the
OOD-gated "nearest junk only" empties this proof drives via ``--min-overlap`` —
recall.py appends ONE line to ``$REFLECT_STATE_DIR/knowledge-gaps.jsonl`` with
the raw ``query``, the stopword-filtered/sorted ``normalized`` dedup key, and a
``session_id``. A recall that returns >=1 learning logs NOTHING. The behavior is
gated by ``RECALL_GAP_LOG`` (env) ANDed with the ``gap_log`` arg (``--no-gap-log``
CLI flag); flipping the env knob OFF suppresses the entry entirely.

Three decisive arms, all on the SAME hermetic KB:

  GAP (knob ON, empty result). An on-topic-only KB is queried with a query
    whose content terms appear in NO seed, at ``--min-overlap 1.0`` so the R7
    OOD gate empties the result set ("nearest junk IS a gap"). The gap file did
    not exist before the call and afterward holds EXACTLY one entry whose
    ``normalized`` is the sorted content-term key and whose ``session_id`` is the
    one we pinned via ``--session-id``. The recall genuinely returned 0 results
    (count == 0, ood_gated true) — the signal tracks real negative recall.

  HIT (knob ON, non-empty result). The SAME KB is queried with an on-topic
    query that returns >=1 learning. NO new line is appended for it — a
    successful recall is not a gap. (Asserted by the gap file's normalized keys
    NOT containing the hit query's key, and the line count not growing for it.)

  KNOB-OFF (RECALL_GAP_LOG=0, empty result). The exact GAP query/flags are
    re-run with ``RECALL_GAP_LOG=0`` in the subprocess env. The result is STILL
    empty (count == 0) but NO gap line is appended — proving the PORT (the gap
    logger), toggled by its documented env knob, caused the entry, not the
    emptiness alone.

No LLM participates in any assertion. The seeds, the off-topic query's content
terms, the ``--min-overlap 1.0`` OOD gate, and the deterministic stdlib
normalizer fully determine every assertion; the gap file is read straight off
disk. recall.py writes the gap log into ``REFLECT_STATE_DIR``, which the
behavioral_kb fixture points at a per-test tmp dir, so the file is hermetic.

PORT: SG6
"""
from __future__ import annotations

import json
from pathlib import Path

# --------------------------------------------------------------------------
# An on-topic-only corpus about ONE subject (Postgres connection pooling).
# The GAP query is about a completely different subject (kubernetes ingress)
# whose content terms appear in none of these bodies, so even the nearest
# neighbour has ~0 query-term coverage and the --min-overlap 1.0 OOD gate
# empties the result set. The HIT query is squarely about pooling.
# --------------------------------------------------------------------------
_SEEDS = [
    dict(
        name="sg6-pool-postgres",
        title="Size a Postgres connection pool below max_connections",
        category="database",
        tags=["postgres", "pooling", "connections"],
        confidence="high",
        created="2026-05-01",
        key_insight="Keep the pool below Postgres max_connections and set statement_timeout.",
        body=(
            "For a Postgres-backed service, size the connection pool below the "
            "backend's max_connections ceiling, set an explicit statement "
            "timeout, and recycle idle connections so a restarted backend does "
            "not strand half-open sockets in the pool."
        ),
    ),
    dict(
        name="sg6-pool-pgbouncer",
        title="Run PgBouncer in transaction mode for many short connections",
        category="database",
        tags=["postgres", "pgbouncer", "pooling"],
        confidence="high",
        created="2026-05-02",
        key_insight="Use PgBouncer transaction pooling to multiplex many clients onto few server conns.",
        body=(
            "With PgBouncer in front of Postgres, transaction-mode pooling "
            "multiplexes many short-lived client connections onto a small set of "
            "server connections, keeping the database's connection count bounded."
        ),
    ),
    dict(
        name="sg6-pool-hikari",
        title="Configure a HikariCP pool size from concurrency, not guesswork",
        category="database",
        tags=["hikaricp", "pooling", "jvm"],
        confidence="high",
        created="2026-05-03",
        key_insight="Derive HikariCP maximumPoolSize from real concurrency, not a round number.",
        body=(
            "When sizing a HikariCP connection pool, derive maximumPoolSize from "
            "measured concurrency and the backend's connection budget rather than "
            "picking a round number, then tune idle timeout to recycle stale "
            "connections."
        ),
    ),
]

# Off-topic query: none of its content terms occur in the pooling corpus, so at
# --min-overlap 1.0 the R7 OOD gate suppresses the nearest neighbours -> 0
# results -> SG6 gap. normalize_gap_query lowercases, drops stopwords, and
# sorts the content terms, so the dedup key is fixed regardless of word order.
_GAP_QUERY = "kubernetes ingress nginx tls termination"
_GAP_NORMALIZED = "ingress kubernetes nginx termination tls"  # sorted content terms

# On-topic query: squarely about the seeded subject -> >=1 result -> NO gap.
_HIT_QUERY = "how should I size and configure a database connection pool"

_SESSION_ID = "sg6-proof-session-001"
_GAP_LOG_NAME = "knowledge-gaps.jsonl"


def _gap_path(kb) -> Path:
    """The jsonl recall.py appends gaps to == REFLECT_STATE_DIR/knowledge-gaps.jsonl.

    The behavioral_kb fixture points REFLECT_STATE_DIR at kb.state_dir, and
    log_knowledge_gap writes ``<REFLECT_STATE_DIR>/knowledge-gaps.jsonl``."""
    return kb.state_dir / _GAP_LOG_NAME


def _gap_entries(kb) -> list[dict]:
    """Every parsed gap entry currently on disk (empty list if the file is absent)."""
    p = _gap_path(kb)
    if not p.exists():
        return []
    return [json.loads(line) for line in p.read_text().splitlines() if line.strip()]


def _warm_recall(kb, query, **flags) -> dict:
    """A recall whose payload is guaranteed warm (carries ``count``).

    recall.py emits an EMPTY stdout document (parsed by the fixture as ``{}`` with
    no ``count`` key) on its silent ``result.error`` path — the cold-index /
    transient ``reflect`` subprocess-failure artifact the R4 proof also documents.
    In isolation a seeded recall is reliable (verified: 5/5 warm payloads), but
    when several proofs run CONCURRENTLY the contended ``reflect`` subprocess can
    transiently take the error path and return ``{}``. That path returns BEFORE
    the SG6 ``log_knowledge_gap`` call (recall.py ``main``: ``if result.error: …
    return 0``), so a ``{}`` attempt writes NO gap line — retrying it is
    side-effect-free and can never double-log a gap. A genuine OOD/empty recall is
    distinguishable: it carries ``count == 0`` (and ``ood_gated``), so it is NOT a
    ``{}`` and is returned immediately, never retried. We retry only the empty
    ``{}`` (missing ``count``), generously, to ride out concurrent contention."""
    payload: dict = {}
    for _ in range(8):
        payload = kb.recall(query, **flags)
        if "count" in payload:
            return payload
    raise AssertionError(
        f"recall.py returned an empty error payload 8x for {query!r} (last "
        f"{payload!r}); the seeded KB never produced a result — engine "
        "cold-start / subprocess contention, not an SG6 fault"
    )


def test_SG6_empty_recall_logs_gap_hit_does_not_and_knob_disables(behavioral_kb):
    """SG6: a 0-result recall persists a knowledge-gap entry; a hit logs none;
    RECALL_GAP_LOG=0 suppresses it."""
    kb = behavioral_kb
    kb.seed(_SEEDS)

    # ---- Warm the index (concurrency determinism). The first recall to touch a
    # freshly reindexed KB — or any recall whose `reflect` subprocess is starved
    # under concurrent proof runs — can return recall.py's empty error document
    # (parsed as {} with no "count"), an engine artifact unrelated to SG6. We
    # absorb that with a throwaway on-topic recall run with --no-gap-log (so it
    # can NEVER write a gap line), retried until warm. After this the gap file is
    # still pristine and every counted arm below queries a warm index.
    _warm_recall(kb, _HIT_QUERY, extra_args=["--no-gap-log"])
    assert not _gap_path(kb).exists(), (
        "the warm-up ran with --no-gap-log, so no knowledge-gaps.jsonl may exist "
        "yet; the gap file's first appearance must be caused by the GAP arm below"
    )

    # ---- Arm GAP: off-topic query, OOD gate empties it -> ONE gap entry. ----
    gap_payload = _warm_recall(
        kb, _GAP_QUERY,
        min_overlap=1.0,  # R7 OOD gate: suppress unless the best hit fully covers the query
        extra_args=["--session-id", _SESSION_ID],
    )
    assert gap_payload.get("count", -1) == 0, (
        "the off-topic query at --min-overlap 1.0 must return 0 results so SG6 "
        f"sees an empty final set; got payload={gap_payload!r}"
    )
    assert gap_payload.get("ood_gated") is True, (
        "the empty result must come from the R7 OOD gate (nearest junk "
        f"suppressed), which SG6 explicitly treats as a gap; got {gap_payload!r}"
    )

    entries = _gap_entries(kb)
    assert len(entries) == 1, (
        f"exactly one knowledge-gap line must be appended for the single empty "
        f"recall; got {len(entries)}: {entries!r}"
    )
    gap = entries[0]
    assert gap["normalized"] == _GAP_NORMALIZED, (
        "the gap's normalized dedup key must be the stopword-filtered, sorted "
        f"content terms of the query; got {gap['normalized']!r}"
    )
    assert gap["query"] == _GAP_QUERY, (
        f"the gap must record the raw query verbatim; got {gap['query']!r}"
    )
    assert gap["session_id"] == _SESSION_ID, (
        "the gap must record the --session-id we pinned (cross-session repeat "
        f"detection keys on it); got {gap['session_id']!r}"
    )

    # ---- Arm HIT: on-topic query returns results -> NO new gap entry. ----
    hit_payload = _warm_recall(
        kb, _HIT_QUERY,
        extra_args=["--session-id", _SESSION_ID],
    )
    assert hit_payload.get("count", 0) >= 1, (
        "the on-topic query must return >=1 learning so it is NOT a gap; got "
        f"payload count={hit_payload.get('count')}, results="
        f"{[r.get('id') for r in hit_payload.get('results', [])]}"
    )
    after_hit = _gap_entries(kb)
    assert len(after_hit) == 1, (
        "a successful (non-empty) recall must append NO gap line — the gap file "
        f"must still hold exactly the one GAP entry; got {len(after_hit)}: "
        f"{after_hit!r}"
    )
    # The only gap entry on disk is still the GAP arm's; nothing the hit query
    # produced (its content terms include "pool"/"database"/"connection") was
    # logged as a gap.
    assert [e["normalized"] for e in after_hit] == [_GAP_NORMALIZED], (
        "the gap log's only normalized key must remain the GAP query's; a "
        f"successful recall must not contribute a gap. Got {after_hit!r}"
    )

    # ---- Arm KNOB-OFF: same empty query, RECALL_GAP_LOG=0 -> still empty, NO gap. ----
    off_payload = _warm_recall(
        kb, _GAP_QUERY,
        min_overlap=1.0,
        env={"RECALL_GAP_LOG": "0"},
        extra_args=["--session-id", _SESSION_ID],
    )
    assert off_payload.get("count", -1) == 0, (
        "the knob-off run must still produce an empty result (same query/gate) "
        f"so the only difference is the gap knob; got {off_payload!r}"
    )
    after_off = _gap_entries(kb)
    assert len(after_off) == 1, (
        "with RECALL_GAP_LOG=0 the empty recall must NOT append a gap line — the "
        "gap file must remain at the single entry from the knob-ON GAP arm. A "
        f"growth here means the env knob did nothing; got {len(after_off)}: "
        f"{after_off!r}"
    )
