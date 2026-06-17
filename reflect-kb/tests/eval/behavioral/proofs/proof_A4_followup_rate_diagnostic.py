# ABOUTME: Behavioral proof for A4 — recall.py flags a recall as a followup (and writes the
# ABOUTME: verdict to metrics.jsonl) iff a different, disjoint prior recall hit the same session in-window; RECALL_FOLLOWUP=0 suppresses it.
"""A4 followup-rate recall-quality diagnostic proof.

Invariant (``track_followup`` + ``log_followup_metric`` + ``record_followup_diagnostic``
in recall.py): every non-empty recall with a session anchor appends ONE
``op="recall_search"`` line to the engine's metrics log (``REFLECT_METRICS_PATH``,
falling back to ``~/.learnings/metrics.jsonl``) carrying a boolean ``followup``
verdict. The verdict is TRUE iff the SAME session's immediately-prior recall:

  (a) happened within ``RECALL_FOLLOWUP_WINDOW_SECONDS`` of this one, AND
  (b) asked a DIFFERENT query (same query in-window is a flaky-caller RETRY,
      not a followup), AND
  (c) returned a result-id set that is DISJOINT from this one (a followup is
      the empirical signal that the first recall did NOT satisfy — the agent
      searched again and got entirely different notes).

A recall with no in-window prior (the session's first search) is ``followup:false``.
The whole diagnostic is gated by ``RECALL_FOLLOWUP`` (env) ANDed with the
``followup_track`` arg (``--no-followup`` CLI flag); flipping the env knob OFF
suppresses the metric line entirely.

Three decisive arms, EACH on its OWN fresh hermetic KB (no cross-arm KB sharing):

  FOLLOWUP (knob ON). One KB holds two disjoint topic clusters (Postgres pooling
    vs Rust borrow-checking). In a single pinned session we run recall A (a
    pooling query) then recall B (a borrow-checker query). A has no prior -> its
    metric line is ``followup:false``. B's prior (A) is in-window, a different
    query, and — verified DIRECTLY off the two recall payloads' returned ids —
    returns a DISJOINT id set, so B's metric line is ``followup:true``. The
    window is set wide (``RECALL_FOLLOWUP_WINDOW_SECONDS`` huge) so the minutes
    between two cold torch-model recalls still count as in-window.

  RETRY (knob ON, predicate (b)). A fresh KB, fresh session: recall A then the
    SAME query A again, in-window. Both touch the SAME ids (id set NOT disjoint)
    AND the query is identical, so the second is a RETRY -> ``followup:false``,
    not a followup. This isolates the "different + disjoint" predicate from mere
    "second search in-window".

  KNOB-OFF (``RECALL_FOLLOWUP=0``). A fresh KB re-runs the FOLLOWUP arm's A-then-B
    sequence with ``RECALL_FOLLOWUP=0`` in the subprocess env. Both recalls still
    return results (non-empty), but NO ``recall_search`` line is appended for
    EITHER — proving the env knob (the PORT's documented gate), not the sequence,
    caused the metric.

No LLM participates in any assertion. The seeds, the two on-topic queries, the
``--session-id`` we pin, and ``RECALL_FOLLOWUP_WINDOW_SECONDS`` fully determine
every verdict; the metrics file is read straight off disk and disjointness is
checked against the REAL returned result ids, never an LLM judgement. The metrics
log is redirected into the per-test tmp dir via ``REFLECT_METRICS_PATH`` and the
recent-searches state lives under ``REFLECT_STATE_DIR`` (the fixture's tmp), so
the whole signal is hermetic.

PORT: A4
"""
from __future__ import annotations

import json
from pathlib import Path

# --------------------------------------------------------------------------
# Two disjoint topic clusters. A pooling query hits ONLY the pool-* ids; a
# borrow-checker query hits ONLY the rust-* ids. No id is shared, so the
# second recall's id set is genuinely DISJOINT from the first's -> a followup.
# --------------------------------------------------------------------------
_SEEDS = [
    dict(
        name="a4-pool-postgres",
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
            "not strand half-open sockets in the connection pool."
        ),
    ),
    dict(
        name="a4-pool-pgbouncer",
        title="Run PgBouncer in transaction mode for many short connections",
        category="database",
        tags=["postgres", "pgbouncer", "pooling"],
        confidence="high",
        created="2026-05-02",
        key_insight="Use PgBouncer transaction pooling to multiplex many clients onto few server conns.",
        body=(
            "With PgBouncer in front of Postgres, transaction-mode pooling "
            "multiplexes many short-lived client connections onto a small set of "
            "server connections, keeping the database connection pool count bounded."
        ),
    ),
    dict(
        name="a4-rust-borrow",
        title="Resolve a Rust borrow-checker conflict by narrowing a mutable borrow",
        category="rust",
        tags=["rust", "borrow-checker", "lifetimes"],
        confidence="high",
        created="2026-05-03",
        key_insight="Narrow the scope of a mutable borrow so it ends before the next immutable borrow.",
        body=(
            "When the Rust borrow checker rejects overlapping mutable and "
            "immutable borrows, narrow the mutable borrow's lifetime scope so it "
            "ends before the immutable borrow begins, or split the struct so the "
            "borrow checker sees disjoint fields."
        ),
    ),
    dict(
        name="a4-rust-lifetime",
        title="Annotate a lifetime to satisfy the Rust borrow checker on returned refs",
        category="rust",
        tags=["rust", "lifetimes", "borrow-checker"],
        confidence="high",
        created="2026-05-04",
        key_insight="Tie the returned reference's lifetime to the input it borrows from.",
        body=(
            "To return a reference from a Rust function the borrow checker needs "
            "an explicit lifetime annotation tying the returned reference to the "
            "input it borrows from, otherwise the borrow checker cannot prove the "
            "reference outlives its referent."
        ),
    ),
]

# Cluster-A query (pooling) and cluster-B query (rust borrow checker). Their
# content terms occur ONLY in their own cluster, so the two recalls return
# disjoint id sets. We still VERIFY disjointness from the live payloads below.
_QUERY_POOL = "how should I size and configure a database connection pool"
_QUERY_RUST = "how do I fix a rust borrow checker lifetime conflict"

_SESSION_ID = "a4-proof-session-001"
_METRICS_NAME = "metrics.jsonl"
# Wide window: two cold-start torch recalls are minutes apart; a 30s default
# would put the prior out-of-window. A huge window keeps (a) satisfied so the
# verdict turns purely on predicates (b)+(c), which is what we are proving.
_WIDE_WINDOW = "100000"


def _metrics_path(kb) -> Path:
    """The metrics log A4 appends recall_search lines to.

    We pin REFLECT_METRICS_PATH into the fixture's per-test state dir so the
    signal is hermetic (the production default is ~/.learnings/metrics.jsonl)."""
    return kb.state_dir / _METRICS_NAME


def _recall_search_lines(kb) -> list[dict]:
    """Every parsed op=='recall_search' (A4) entry on disk, in append order."""
    p = _metrics_path(kb)
    if not p.exists():
        return []
    out = []
    for line in p.read_text().splitlines():
        if not line.strip():
            continue
        rec = json.loads(line)
        if rec.get("op") == "recall_search":
            out.append(rec)
    return out


def _warm_recall(kb, query, **flags) -> dict:
    """A recall whose payload is guaranteed warm (carries ``count``).

    recall.py emits an EMPTY stdout document (parsed as ``{}`` with no
    ``count``) on its silent ``result.error`` path — the cold-index / transient
    ``reflect`` subprocess-failure artifact. That error path returns BEFORE A4's
    ``record_followup_diagnostic`` call (recall(): the graph-err branch returns
    a RecallResult with ``error=`` set, well above the followup tail), so a
    ``{}`` attempt writes NO followup state and NO metric line — retrying it is
    side-effect-free and can never double-count a followup. A genuine recall
    carries ``count`` (>=1 here, every query is on-topic) and is returned
    immediately, never retried. We retry only the empty ``{}`` to ride out
    cold-start / concurrent-contention flakes."""
    payload: dict = {}
    for _ in range(8):
        payload = kb.recall(query, **flags)
        if "count" in payload:
            return payload
    raise AssertionError(
        f"recall.py returned an empty error payload 8x for {query!r} (last "
        f"{payload!r}); the seeded KB never produced a result — engine "
        "cold-start / subprocess contention, not an A4 fault"
    )


def _ids(payload: dict) -> set[str]:
    """The CONCRETE result ids the recall returned (frontmatter name == id).

    recall.py renders a chunk whose frontmatter id/name didn't survive as the
    placeholder ``"?"``. That placeholder is NOT a real shared id — internally
    A4's ``track_followup`` keys such a chunk on a sha1 of its chunk_text
    (``_learning_key``'s fallback), which differs per chunk, so two disjoint-topic
    recalls stay disjoint in the real followup logic even when both surface a
    ``"?"`` row. We drop ``"?"`` here so the payload-level disjointness check
    reflects the same reality the port computes, not a display artifact."""
    return {
        r.get("id")
        for r in payload.get("results", [])
        if r.get("id") and r.get("id") != "?"
    }


def test_A4_followup_true_when_prior_disjoint_false_on_retry_knob_disables(behavioral_kb):
    """A4: in-window prior + different query + disjoint ids -> followup:true;
    first search / retry -> followup:false; RECALL_FOLLOWUP=0 -> no metric."""
    kb = behavioral_kb
    kb.seed(_SEEDS)

    metrics_env = {
        "REFLECT_METRICS_PATH": str(_metrics_path(kb)),
        "RECALL_FOLLOWUP_WINDOW_SECONDS": _WIDE_WINDOW,
    }

    # ---- Warm the index. The first recall to touch a freshly reindexed KB (or
    # any recall whose `reflect` subprocess is starved under concurrent runs) can
    # return recall.py's empty error document, unrelated to A4. Absorb it with a
    # throwaway recall run --no-followup (so it can NEVER write a recall_search
    # line), retried until warm. After this the metrics file is still pristine.
    _warm_recall(kb, _QUERY_POOL, env=metrics_env, extra_args=["--no-followup"])
    assert _recall_search_lines(kb) == [], (
        "the warm-up ran with --no-followup, so no recall_search line may exist "
        "yet; the metric's first appearance must be caused by arm FOLLOWUP below"
    )

    # ---- Arm FOLLOWUP: A (pooling) then B (rust), same session, wide window. ----
    payload_a = _warm_recall(
        kb, _QUERY_POOL, env=metrics_env,
        extra_args=["--session-id", _SESSION_ID],
    )
    assert payload_a.get("count", 0) >= 1, (
        f"pooling query A must return >=1 result; got {payload_a!r}"
    )

    payload_b = _warm_recall(
        kb, _QUERY_RUST, env=metrics_env,
        extra_args=["--session-id", _SESSION_ID],
    )
    assert payload_b.get("count", 0) >= 1, (
        f"rust query B must return >=1 result; got {payload_b!r}"
    )

    # Disjointness is the load-bearing predicate (c). Verify it DIRECTLY off the
    # two recalls' returned ids — no LLM, just set logic on the real id sets.
    ids_a, ids_b = _ids(payload_a), _ids(payload_b)
    assert ids_a and ids_b, (
        f"both recalls must return concrete ids to reason about; A={ids_a}, B={ids_b}"
    )
    assert ids_a.isdisjoint(ids_b), (
        "arm FOLLOWUP requires the two recalls' id sets to be DISJOINT (so B is a "
        f"genuine followup, not a refinement); A={sorted(ids_a)} B={sorted(ids_b)}"
    )

    lines = _recall_search_lines(kb)
    assert len(lines) == 2, (
        "exactly two recall_search metric lines must be appended — one per "
        f"counted recall in this session; got {len(lines)}: {lines!r}"
    )
    first, second = lines
    assert all(l["session_id"] == _SESSION_ID for l in lines), (
        f"both metric lines must carry the pinned session id; got {lines!r}"
    )
    assert first["followup"] is False, (
        "the session's FIRST recall has no in-window prior, so it must be "
        f"followup:false; got {first!r}"
    )
    assert second["followup"] is True, (
        "the SECOND recall has an in-window prior (A) with a DIFFERENT query and "
        "a DISJOINT id set, so A4 must flag it followup:true; got "
        f"{second!r}"
    )
    assert second["window_seconds"] == float(_WIDE_WINDOW), (
        "the metric must record the env-configured detection window verbatim; "
        f"got {second.get('window_seconds')!r}"
    )

    # ---- Arm RETRY: fresh KB/session, SAME query twice -> second is followup:false. ----
    kb_retry = type(kb)(kb.workdir / "a4-retry")
    kb_retry.seed(_SEEDS)
    retry_env = {
        "REFLECT_METRICS_PATH": str(_metrics_path(kb_retry)),
        "RECALL_FOLLOWUP_WINDOW_SECONDS": _WIDE_WINDOW,
    }
    _warm_recall(kb_retry, _QUERY_POOL, env=retry_env, extra_args=["--no-followup"])
    assert _recall_search_lines(kb_retry) == [], (
        "retry-arm warm-up ran --no-followup; no recall_search line may exist yet"
    )
    retry_sid = "a4-retry-session-001"
    r1 = _warm_recall(
        kb_retry, _QUERY_POOL, env=retry_env,
        extra_args=["--session-id", retry_sid],
    )
    r2 = _warm_recall(
        kb_retry, _QUERY_POOL, env=retry_env,  # SAME query
        extra_args=["--session-id", retry_sid],
    )
    assert r1.get("count", 0) >= 1 and r2.get("count", 0) >= 1, (
        f"both retry recalls must return results; r1={r1!r} r2={r2!r}"
    )
    # Same query -> overlapping id set (NOT disjoint). The real retry verdict
    # short-circuits on query==query before disjointness, but we still confirm
    # the payloads overlap so the arm is not accidentally a disjoint case.
    assert _ids(r1) and not _ids(r1).isdisjoint(_ids(r2)), (
        "the identical retry query must return overlapping (non-disjoint) ids; "
        f"r1={sorted(_ids(r1))} r2={sorted(_ids(r2))}"
    )
    retry_lines = _recall_search_lines(kb_retry)
    assert len(retry_lines) == 2, (
        f"two recall_search lines expected for the retry session; got {retry_lines!r}"
    )
    assert retry_lines[0]["followup"] is False, (
        f"the retry session's first recall must be followup:false; got {retry_lines[0]!r}"
    )
    assert retry_lines[1]["followup"] is False, (
        "the SECOND retry recall asks the SAME query in-window — a flaky-caller "
        "RETRY, NOT a followup — so A4 must keep it followup:false even though a "
        f"prior exists in-window; got {retry_lines[1]!r}"
    )

    # ---- Arm KNOB-OFF: fresh KB/session, A-then-B with RECALL_FOLLOWUP=0. ----
    kb_off = type(kb)(kb.workdir / "a4-off")
    kb_off.seed(_SEEDS)
    off_env = {
        "REFLECT_METRICS_PATH": str(_metrics_path(kb_off)),
        "RECALL_FOLLOWUP_WINDOW_SECONDS": _WIDE_WINDOW,
        "RECALL_FOLLOWUP": "0",  # the PORT's documented kill switch
    }
    off_sid = "a4-off-session-001"
    off_a = _warm_recall(
        kb_off, _QUERY_POOL, env=off_env, extra_args=["--session-id", off_sid],
    )
    off_b = _warm_recall(
        kb_off, _QUERY_RUST, env=off_env, extra_args=["--session-id", off_sid],
    )
    assert off_a.get("count", 0) >= 1 and off_b.get("count", 0) >= 1, (
        "both knob-off recalls must still return results so the ONLY difference "
        f"from arm FOLLOWUP is RECALL_FOLLOWUP=0; off_a={off_a!r} off_b={off_b!r}"
    )
    assert _recall_search_lines(kb_off) == [], (
        "with RECALL_FOLLOWUP=0 NO recall_search line may be appended for EITHER "
        "recall — the env knob gates the whole A4 diagnostic. A line here means "
        f"the knob did nothing; got {_recall_search_lines(kb_off)!r}"
    )
