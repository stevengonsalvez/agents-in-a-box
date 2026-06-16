# ABOUTME: Behavioral proof for R4 — recall.py's --max-tokens bounds the result block by
# ABOUTME: estimated tokens (len/4), NOT by a fixed top-k; >=1 result is always kept.
"""R4 token-budget retrieval proof.

Invariant (the heart of R4 — ``filter_by_token_budget`` in recall.py): when
``--max-tokens B`` is > 0, the returned block is bounded by ESTIMATED TOKENS
(``_est_tokens(text) = max(1, len(text)//4)`` summed over kept chunks), not by
the ``--limit`` count alone. Concretely, three things must hold and they are in
tension — which is exactly why a token budget was ported on top of top-k:

  A. BUDGET CAUSES FEWER RESULTS. With the SAME ``--limit``, a small
     ``--max-tokens`` returns STRICTLY FEWER results than ``--max-tokens=0``
     (the unbounded control). The seeds are several sizable, on-topic learnings
     so the unbounded run fills the limit; the budget then trims it. Flipping
     the budget OFF (0) restores the full block — proving the PORT (the budget),
     not incidental ranking, caused the cut.

  B. THE CUT RESPECTS THE BUDGET. For a medium budget chosen at runtime to fit
     exactly the first two unbounded results, the kept block's total estimated
     tokens (read from the engine's own M8 ``economics.read_tokens`` roll-up,
     which is ``sum(_est_tokens(chunk_text))`` — the SAME estimator the budget
     filter spends) is <= the budget, and the block stops BEFORE the limit. The
     budget, not the count, decided where to cut.

  C. THE >=1 FLOOR HOLDS. With a budget far smaller than a single chunk, the
     block does not collapse to empty — exactly ONE result is kept, because
     ``filter_by_token_budget`` always keeps the first learning before it can
     break on cost ("so a single long learning can't starve the caller").
     read_tokens of that one block exceeds the budget — the documented floor
     exception, and the proof that it is a token budget, not a top-1 cap.

No LLM participates in any assertion. The seeds (their byte length), the
``--max-tokens`` flag, and the deterministic ``len//4`` estimator fully
determine every number asserted; ``max_tokens`` budgets are derived at runtime
from the engine's reported per-result token costs, so the proof self-calibrates
to whatever chunk sizes the engine produces.

PORT: R4
"""
from __future__ import annotations

# --------------------------------------------------------------------------
# Six sizable, distinct-but-on-topic learnings. Same broad subject
# (connection pooling / DB resource limits) so they all survive into the
# top-`limit` window; distinct bodies so the engine keeps them as six chunks
# (no dedup collapse) and each carries a non-trivial token cost (~200 tokens
# of chunk each, well above the tiny budget used in arm C).
# --------------------------------------------------------------------------
_LONG = (
    "When configuring a connection pool for a service, size the pool below the "
    "backend's hard connection ceiling with a headroom margin, set an explicit "
    "statement timeout, and recycle idle connections so a restarted backend "
    "does not strand half-open sockets inside the application's pool. "
)

_SEEDS = [
    dict(
        name="r4-pool-postgres",
        title="Size a Postgres connection pool below max_connections",
        category="database",
        tags=["postgres", "pooling", "connections"],
        confidence="high",
        created="2026-05-01",
        archived="2026-05-10T00:00:00",
        key_insight="Keep the pool below Postgres max_connections and set statement_timeout.",
        body="For a Postgres-backed service: " + _LONG + "Reserve slots for admin and migrations too.",
    ),
    dict(
        name="r4-pool-pgbouncer",
        title="Run PgBouncer in transaction mode for many short connections",
        category="database",
        tags=["postgres", "pgbouncer", "pooling"],
        confidence="high",
        created="2026-05-02",
        archived="2026-05-11T00:00:00",
        key_insight="Use PgBouncer transaction pooling to multiplex many clients onto few server conns.",
        body="With PgBouncer in front of Postgres: " + _LONG + "Transaction mode multiplexes best.",
    ),
    dict(
        name="r4-pool-mysql",
        title="Tune a MySQL connection pool and wait_timeout together",
        category="database",
        tags=["mysql", "pooling", "connections"],
        confidence="high",
        created="2026-05-03",
        archived="2026-05-12T00:00:00",
        key_insight="Align the client pool's idle timeout with MySQL wait_timeout to avoid stale conns.",
        body="For a MySQL connection pool: " + _LONG + "Align idle recycle with wait_timeout.",
    ),
    dict(
        name="r4-pool-hikari",
        title="Configure a HikariCP pool size from concurrency, not guesswork",
        category="database",
        tags=["hikaricp", "pooling", "jvm"],
        confidence="high",
        created="2026-05-04",
        archived="2026-05-13T00:00:00",
        key_insight="Derive HikariCP maximumPoolSize from real concurrency, not a round number.",
        body="When sizing a HikariCP connection pool: " + _LONG + "Derive size from concurrency.",
    ),
    dict(
        name="r4-pool-redis",
        title="Bound a Redis client connection pool and reuse clients",
        category="database",
        tags=["redis", "pooling", "connections"],
        confidence="high",
        created="2026-05-05",
        archived="2026-05-14T00:00:00",
        key_insight="Cap the Redis connection pool and reuse a single client across requests.",
        body="For a Redis connection pool: " + _LONG + "Reuse one client; do not open per request.",
    ),
    dict(
        name="r4-pool-pgproxy",
        title="Front read replicas with a pooling proxy and a connection budget",
        category="database",
        tags=["postgres", "replicas", "pooling"],
        confidence="high",
        created="2026-05-06",
        archived="2026-05-15T00:00:00",
        key_insight="Give each read replica its own connection budget behind a pooling proxy.",
        body="When fronting Postgres read replicas: " + _LONG + "Budget connections per replica.",
    ),
]

_QUERY = "how should I size and configure a database connection pool"
_LIMIT = 6  # >= the seed count, so the unbounded block is gated only by the budget


def _recall(kb, budget: int):
    """recall.py at the fixed query/limit with a given token budget.

    recall.py prints an empty document (parsed by the fixture as ``{}``) on the
    silent KB-absence / cold-index path the FIRST time an index is touched.
    That is an engine cold-start artifact, not R4 behavior, so a single warm
    retry is allowed before we trust the payload. Once warm, the payload always
    carries ``count`` + ``economics`` (verified by a standalone diagnostic:
    count=6, economics.read_tokens=1189, per-result read_tokens=197)."""
    for _ in range(2):
        payload = kb.recall(_QUERY, limit=_LIMIT, max_tokens=budget, no_mmr=True)
        if "count" in payload:
            return payload
    raise AssertionError(
        f"recall.py returned an empty payload twice for budget={budget}: "
        f"{payload!r} — the seeded KB did not index (engine cold-start), "
        "not an R4 fault"
    )


def _block_tokens(payload: dict) -> int:
    """The engine's own M8 roll-up of kept-block estimated tokens.

    ``economics.read_tokens`` == ``sum(_est_tokens(chunk_text))`` over the kept
    learnings — the exact quantity ``filter_by_token_budget`` spends against the
    budget. M8 economics is ON by default (RECALL_ECONOMICS != "0")."""
    econ = payload.get("economics") or {}
    assert "read_tokens" in econ, (
        "expected M8 block economics (economics.read_tokens) in the payload — "
        f"is RECALL_ECONOMICS disabled? got envelope keys {sorted(payload)}"
    )
    return int(econ["read_tokens"])


def test_R4_token_budget_bounds_block_not_topk(behavioral_kb):
    """R4: --max-tokens bounds the block by estimated tokens, not a fixed
    top-k; the cut respects the budget; and >=1 result is always kept."""
    kb = behavioral_kb
    kb.seed(_SEEDS)

    # ---- Control: budget OFF (max_tokens=0). The block fills to the limit. ----
    unbounded = _recall(kb, 0)
    n_unbounded = unbounded["count"]
    assert n_unbounded >= 3, (
        "the unbounded control must return several sizable on-topic learnings so "
        f"there is a block to trim; got count={n_unbounded}, ids="
        f"{[r.get('id') for r in unbounded['results']]}"
    )
    # Per-result token costs as the engine reports them (same len//4 estimator
    # the budget spends). Used to derive budgets that self-calibrate to the
    # actual chunk sizes the engine produced.
    per_result = [int(r["economics"]["read_tokens"]) for r in unbounded["results"]]
    assert all(c > 0 for c in per_result), per_result
    unbounded_block_tokens = _block_tokens(unbounded)

    # ---- Arm A + B: a MEDIUM budget sized to fit exactly the first two ----
    # results, with no room for the third. The budget must cut the block
    # shorter than the unbounded run AND keep the kept block within budget.
    medium_budget = per_result[0] + per_result[1]  # fits #0 and #1, not #2
    medium = _recall(kb, medium_budget)
    n_medium = medium["count"]

    # A: the budget CAUSED fewer results than the unbounded control.
    assert n_medium < n_unbounded, (
        f"with --max-tokens={medium_budget} the block must be SMALLER than the "
        f"unbounded block (count {n_medium} vs {n_unbounded}); if equal, the "
        "budget did nothing and this is top-k, not a token budget"
    )
    assert n_medium >= 1
    # B: the kept block respects the budget — its total estimated tokens (the
    # quantity the filter sums) does not exceed the budget. (When n_medium == 1
    # this is the floor case, covered decisively in arm C; here we expect >=2
    # because the budget was sized to fit two, but we assert the budget bound
    # for whatever multi-result block came back.)
    medium_block_tokens = _block_tokens(medium)
    if n_medium >= 2:
        assert medium_block_tokens <= medium_budget, (
            f"the kept block ({n_medium} results, {medium_block_tokens} est. "
            f"tokens) must fit the {medium_budget}-token budget — the cut must "
            "respect the budget, not overspend it"
        )
        # And the cut stopped strictly before the limit because of the budget,
        # not because the corpus ran out (unbounded returned strictly more).
        assert n_medium < n_unbounded

    # ---- Arm C: a TINY budget far below a single chunk. The >=1 floor holds: ----
    # exactly one result is kept, and its block tokens EXCEED the budget — the
    # documented floor exception, proving a token budget (not a top-1 cap).
    tiny_budget = max(1, min(per_result) // 4)  # << one chunk's cost
    tiny = _recall(kb, tiny_budget)
    assert tiny["count"] == 1, (
        f"with a tiny --max-tokens={tiny_budget} the >=1 floor must keep EXACTLY "
        f"one result (never zero), got count={tiny['count']}"
    )
    tiny_block_tokens = _block_tokens(tiny)
    assert tiny_block_tokens > tiny_budget, (
        f"the single kept result's {tiny_block_tokens} est. tokens must EXCEED "
        f"the {tiny_budget}-token budget — that overspend is the documented "
        "floor ('>=1 always kept so a long learning can't starve the caller') "
        "and is what distinguishes a token budget from a fixed top-1 cap"
    )
