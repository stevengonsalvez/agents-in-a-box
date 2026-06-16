# ABOUTME: Behavioral proof for R7 — the out-of-domain (OOD) recall gate.
# ABOUTME: Off-topic query => ood_gated true & count 0; on-topic => ood_gated false & hit appears.
"""R7 OOD gate proof.

Invariant: with `min_overlap` raised, recall suppresses results that share no
meaningful lexical overlap with the query (sets ood_gated=True, count=0) rather
than injecting irrelevant noise — but still serves a genuinely on-topic hit
(ood_gated=False). Seeds + the --min-overlap flag fully determine the outcome;
no LLM participates in the assertion.

PORT: R7
"""
from __future__ import annotations

# Three learnings about ONE unrelated domain (marine biology). None of them
# overlaps the off-topic engineering query below.
_OFF_TOPIC_SEEDS = [
    dict(
        name="r7-coral-bleaching",
        title="Coral bleaching tracks sustained ocean temperature anomalies",
        category="marine-biology",
        tags=["coral", "reef", "ocean-temperature"],
        confidence="high",
        created="2026-01-10",
        key_insight="Sustained sea-surface temperature anomalies expel zooxanthellae, bleaching coral.",
        body="Coral bleaching follows multi-week heat stress on reef ecosystems; recovery depends on the symbiont returning.",
    ),
    dict(
        name="r7-whale-migration",
        title="Humpback whale migration corridors follow krill density",
        category="marine-biology",
        tags=["whale", "migration", "krill"],
        confidence="medium",
        created="2026-01-12",
        key_insight="Humpback migration routes correlate with seasonal krill blooms.",
        body="Baleen whales migrate along corridors shaped by prey density rather than fixed routes.",
    ),
    dict(
        name="r7-tide-pool-ecology",
        title="Tide pool zonation is set by desiccation tolerance",
        category="marine-biology",
        tags=["tide-pool", "intertidal", "ecology"],
        confidence="medium",
        created="2026-01-14",
        key_insight="Intertidal zonation bands organisms by their tolerance to air exposure.",
        body="Upper tide pools host desiccation-tolerant species; lower zones host competition-limited ones.",
    ),
]

# An on-topic learning that DOES overlap the engineering query.
_ON_TOPIC_SEED = dict(
    name="r7-postgres-deadlock",
    title="Postgres deadlocks resolve by consistent lock ordering",
    category="database",
    tags=["postgres", "deadlock", "locking"],
    confidence="high",
    created="2026-02-01",
    key_insight="Acquire row locks in a consistent order across transactions to avoid deadlocks.",
    body="Postgres deadlock errors arise when two transactions lock the same rows in opposite order; "
         "enforce a canonical lock ordering to eliminate them.",
)

# Off-topic query: nothing in the marine corpus is relevant to it.
OFF_TOPIC_QUERY = "how do I fix a postgres database deadlock from out-of-order row locking"
ON_TOPIC_QUERY = OFF_TOPIC_QUERY  # same query; only the corpus changes between phases.

MIN_OVERLAP = 0.2


def test_R7_ood_gate(behavioral_kb):
    kb = behavioral_kb

    # ---- Phase 1: only off-topic docs -> gate fires, nothing injected ----
    kb.seed(_OFF_TOPIC_SEEDS)
    gated = kb.recall(OFF_TOPIC_QUERY, min_overlap=MIN_OVERLAP)
    assert gated["ood_gated"] is True, (
        f"expected OOD gate to fire on off-topic corpus, got payload={gated}"
    )
    assert gated["count"] == 0, (
        f"expected zero results when gated, got count={gated['count']} "
        f"ids={[r.get('id') for r in gated.get('results', [])]}"
    )

    # ---- Phase 2: add the on-topic doc -> gate releases, hit appears ----
    kb.seed([_ON_TOPIC_SEED])
    served = kb.recall(ON_TOPIC_QUERY, min_overlap=MIN_OVERLAP)
    assert served["ood_gated"] is False, (
        f"expected gate to release once an on-topic doc exists, got payload={served}"
    )
    ids = [r.get("id") for r in served.get("results", [])]
    assert _ON_TOPIC_SEED["name"] in ids, (
        f"expected on-topic doc {_ON_TOPIC_SEED['name']!r} in results, got {ids}"
    )
