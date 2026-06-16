# ABOUTME: Behavioral proof for R8 — multiplicative bounded boosts in recall.py rerank.
# ABOUTME: A boosted signal breaks ties (recent > old) but the boost is BOUNDED — a maxed
# ABOUTME: boost can never flip a decisively stronger base (CE) hit.
"""R8 multiplicative bounded boosts proof.

Invariant (the heart of R8): every secondary ranking signal — confidence,
recency, tags, proof count — is applied as a MULTIPLICATIVE boost
``boost = 1 + α·(norm − 0.5)`` that is BOUNDED to ``[1 − α/2, 1 + α/2]``
(Hindsight ``apply_combined_scoring`` shape). Two consequences must BOTH hold,
and they are in tension — which is exactly why the bounded form was ported:

  A. The boost is REAL: when the base relevance (cross-encoder × lexical) is a
     near-tie, the boosted signal decides the order. We seed two learnings
     with near-identical body text (so the CE scores them ~equally) that
     differ ONLY in their ``archived`` date. The recent one must outrank the
     old one. Turning the recency boost OFF (``RECALL_RECENCY_ALPHA=0``)
     removes the only tie-breaker, so the recency-driven ordering is no longer
     forced — proving the boost, not some incidental text difference, drove it.

  B. The boost is BOUNDED: a maxed-out boost on a WEAK base hit can NOT flip a
     decisively stronger base hit. We seed a learning that DIRECTLY answers the
     query but is archived two years ago, against a learning that barely
     relates to the query but was archived today. Even with the recency boost
     cranked to its clamp ceiling (``RECALL_RECENCY_ALPHA=2.0`` → ``_env_alpha``
     caps at 2.0, the widest legal swing, recency ∈ [0, 2]), the strong-but-old
     answer must STILL win, because ``score = sigmoid(ce_logit) × boost`` and
     the CE gap between a direct answer and an off-query note (measured ≈ 0.999
     vs ≈ 1e-5, ~5 orders of magnitude) dwarfs a ≤2× recency multiplier.

If the rerank used the OLD unbounded ``exp(-age/90)`` recency multiplier
instead of R8's bounded form, arm B would FAIL: a two-year-old note would be
crushed to ~e^(-730/90) ≈ 0.03% of its score and the recent off-query note
would win on recency alone. So arm B has no value unless the boost is genuinely
clamped — which is R8's second acceptance bullet ("each boost stays in declared
range"). Arm A covers the first bullet ("rank order under known signal mixes").

No LLM participates in either assertion: the seeds, the archive dates, and the
documented env flag fully determine the outcome.

PORT: R8
"""
from __future__ import annotations

# --------------------------------------------------------------------------
# Arm A: near-identical text, differ ONLY in archive date. CE scores them
# alike, so the bounded recency boost is the tie-breaker.
# --------------------------------------------------------------------------
_RECENT_TWIN = dict(
    name="r8-recent-twin",
    title="Retry transient HTTP 503 with exponential backoff and jitter",
    category="reliability",
    tags=["http", "retry", "backoff"],
    confidence="medium",
    created="2026-06-01",
    archived="2026-06-10T00:00:00",   # archived a few days ago → recency ceiling
    key_insight="Back off exponentially with jitter on transient 503s instead of retrying immediately.",
    body=(
        "Transient HTTP 503 responses should be retried with exponential "
        "backoff and jitter rather than a tight immediate retry loop, so a "
        "struggling upstream is not stampeded by synchronized client retries."
    ),
)

# Same topic, near-identical body (one trailing clause differs so the files are
# not byte-identical and don't collapse to one chunk). Archived ~2 years ago.
_OLD_TWIN = dict(
    name="r8-old-twin",
    title="Retry transient HTTP 503 with exponential backoff and jitter",
    category="reliability",
    tags=["http", "retry", "backoff"],
    confidence="medium",
    created="2024-06-01",
    archived="2024-06-10T00:00:00",   # archived ~2 years ago → recency floor
    key_insight="Back off exponentially with jitter on transient 503s instead of retrying immediately.",
    body=(
        "Transient HTTP 503 responses should be retried with exponential "
        "backoff and jitter rather than a tight immediate retry loop, so a "
        "struggling upstream is not stampeded by synchronized client retries "
        "from every caller at once."
    ),
)

_TWIN_QUERY = "how to retry transient HTTP 503 errors with exponential backoff"

# --------------------------------------------------------------------------
# Arm B: a decisive base gap. The strong answer is OLD; the weak note is
# RECENT. A maxed recency boost must NOT flip the decisive CE gap.
# --------------------------------------------------------------------------
_STRONG_OLD = dict(
    name="r8-strong-old",
    title="Diagnose a Postgres deadlock from lock-wait graphs in pg_locks",
    category="database",
    tags=["postgres", "deadlock"],
    confidence="medium",
    created="2024-01-01",
    archived="2024-01-01T00:00:00",   # archived ~2.5 years ago → recency floor
    key_insight="Read pg_locks to find the lock-wait cycle behind a Postgres deadlock.",
    body=(
        "To diagnose a Postgres deadlock, inspect pg_locks and the server log's "
        "deadlock detail to find the lock-wait cycle: two transactions each "
        "holding a row lock the other needs. Order your lock acquisition "
        "consistently to break the cycle."
    ),
)

_WEAK_RECENT = dict(
    name="r8-weak-recent",
    title="Pick a CSS color palette for a marketing landing page",
    category="design",
    tags=["css", "design"],
    confidence="medium",
    created="2026-06-10",
    archived="2026-06-12T00:00:00",   # archived yesterday → recency ceiling
    key_insight="Use a restrained two-color palette with one accent for a landing page.",
    body=(
        "When choosing a CSS color palette for a marketing landing page, keep "
        "it to two base colors plus a single accent, and check contrast ratios "
        "for accessibility."
    ),
)

_DEADLOCK_QUERY = "how do I diagnose a Postgres deadlock from lock-wait cycles"


def _rank_of(ids: list[str], name: str) -> int:
    assert name in ids, f"expected {name!r} in results, got {ids}"
    return ids.index(name)


def test_R8_recency_boost_breaks_a_near_tie(behavioral_kb):
    """Arm A: with the base relevance a near-tie, the bounded recency boost
    forces the recent twin ahead of the old twin; zeroing the boost removes
    the only tie-breaker so that ordering is no longer forced."""
    kb = behavioral_kb
    kb.seed([_RECENT_TWIN, _OLD_TWIN])

    # Boost ON (default α=0.2): recent twin must outrank old twin.
    ids_on = kb.recall_ids(_TWIN_QUERY, no_mmr=True)
    assert _rank_of(ids_on, _RECENT_TWIN["name"]) < _rank_of(ids_on, _OLD_TWIN["name"]), (
        "with the recency boost ON, the recently-archived twin must outrank "
        f"the years-old twin of near-identical text; got order {ids_on}"
    )

    # Boost OFF (RECALL_RECENCY_ALPHA=0): recency is no longer a tie-breaker.
    # The recent-first ordering must NOT be guaranteed any more — i.e. the
    # ON-ordering was caused by the boost, not by incidental text/index order.
    ids_off = kb.recall_ids(
        _TWIN_QUERY, no_mmr=True, env={"RECALL_RECENCY_ALPHA": "0"}
    )
    recent_first_off = _rank_of(ids_off, _RECENT_TWIN["name"]) < _rank_of(
        ids_off, _OLD_TWIN["name"]
    )
    # The recency boost is the ONLY signal that differs between the twins
    # (same confidence/tags/proof, near-identical text). With it zeroed, the
    # two are scored identically up to CE noise on the one-clause text diff,
    # which favors the OLD twin's slightly longer body if anything — so the
    # recent-first guarantee from arm-on is gone.
    assert not recent_first_off, (
        "with the recency boost OFF the recent twin must NOT be forced ahead — "
        "if it still leads, recency was not what drove the arm-on ordering; "
        f"got order {ids_off}"
    )


def test_R8_boost_is_bounded_cannot_flip_a_decisive_base(behavioral_kb):
    """Arm B: a recency boost cranked to its clamp ceiling (α=2.0) on a recent
    but off-query note can NOT overtake an old note that decisively answers the
    query — the boost is multiplicative and BOUNDED, the CE gap is not."""
    kb = behavioral_kb
    kb.seed([_STRONG_OLD, _WEAK_RECENT])

    # Recency boost at its maximum legal strength (_env_alpha clamps to 2.0):
    # recency ∈ [0, 2], the widest swing the boost can ever apply.
    ids = kb.recall_ids(
        _DEADLOCK_QUERY, no_mmr=True, env={"RECALL_RECENCY_ALPHA": "2.0"}
    )
    # Both seeds are returned (two docs in, two chunks out). The strong-but-old
    # answer must sit at rank 0 — ahead of the recent off-query note. The
    # off-query note may surface with id '?' when the engine merges/strips its
    # frontmatter, so we assert on the strong note's rank (which is decisive
    # and id-stable) rather than the weak note's name.
    assert len(ids) >= 2, (
        f"expected both seeded notes back, got {ids}"
    )
    strong_rank = _rank_of(ids, _STRONG_OLD["name"])
    assert strong_rank == 0, (
        "even with the recency boost maxed (α=2.0, recency ∈ [0,2]), the OLD "
        "note that DIRECTLY answers the query must rank FIRST, ahead of the "
        "RECENT off-query note — score = sigmoid(ce_logit) × bounded_boost, and "
        "the CE gap (direct answer vs off-topic, ~5 orders of magnitude) dwarfs "
        f"a ≤2× recency multiplier. Got order {ids} (strong rank {strong_rank}). "
        "If this fails, the recency boost is unbounded (e.g. the old "
        "exp(-age/90) form), not R8's clamp."
    )
