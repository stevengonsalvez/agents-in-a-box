# ABOUTME: Behavioral proof for R5 — the temporal retrieval arm (4th parallel arm).
# ABOUTME: A windowed query surfaces an in-window note the base arms crowd out; arm off, it vanishes.
"""R5 temporal-arm proof.

Invariant: when a query carries a date phrase, the R5 temporal arm scans the
corpus for notes inside the queried window and fuses them into the candidate
pool as a 4th retrieval source. This is the ONLY path by which a note that the
topical (semantic / qmd) arms crowd out of their fetched window can reach the
served results. With ``RECALL_TEMPORAL_ARM=0`` (R6 extraction stays on, only the
R5 arm is killed) that note is no longer fused in and disappears. The seeds, the
explicit ISO date window in the query, and the kill-switch env fully determine
the outcome — no LLM participates in the assertion.

Why this is falsifiable (and why the architecture forces this shape): after RRF
fusion recall.py re-sorts the whole candidate pool by a bounded-boost formula
(confidence × recency × tags × proof × …), discarding the RRF rank among scored
candidates. So the temporal arm's *observable* contribution is CANDIDATE
INCLUSION — pulling an in-window note into the pool that the topical arms,
capped at ``fetched_limit`` (= max(limit*2, 10)), never returned. We engineer:

  * a FLOOD of decoys (> fetched_limit) that are topically STRONG on the query
    terms but dated OUTSIDE the window AND OLDER — so they monopolise every
    base arm's fetched window yet score LOW on the recency-driven formula;
  * one in-window note that is topically WEAK (so the base arms drop it from
    their fetched window) but is the NEWEST datable note (so once it is in the
    pool the formula ranks it FIRST).

The cross-encoder is disabled (RECALL_CROSS_ENCODER=0) so the topic-blind
recency formula governs the final order — isolating the R5 RRF contribution
from CE's topical re-scoring. Arm on: the temporal arm (date filter, not topic)
fuses the lone in-window note into the pool and the recency formula floats it to
the top — it is served. Arm off: the note has NO other path into the pool (the
topical arms ranked it below the fetched cut), so it cannot appear. If R5 were
absent or fused as a no-op, the arm-on phase would fail.

Acceptance criteria covered:
  - "arm returns 0 hits on date-free queries (no false boost)" — the kill-switch
    phase removes the arm's contribution exactly as a date-free query would
    (temporal is None => the arm short-circuits to []).
  - "integrates into RRF cleanly" — the inclusion is observed in the FUSED,
    reranked ranking recall.py returns, through the live pipeline.

PORT: R5
"""
from __future__ import annotations

# Explicit ISO range => temporal_extraction yields a fixed [2026-01-01,
# 2026-01-31] window regardless of the wall clock, so the proof is clock-stable.
WINDOW_QUERY = (
    "redis connection pool exhaustion decision "
    "between 2026-01-01 and 2026-01-31"
)

# In-window note: dated INSIDE the window AND the NEWEST datable note in the
# corpus, so the recency-driven formula ranks it first once it is in the pool.
# Topically the WEAKEST match (plain wording, no query-term repetition) so the
# base topical arms never fetch it on their own.
_IN_WINDOW = dict(
    name="r5-in-window-note",
    title="January note",
    category="general",
    tags=["misc"],
    confidence="high",
    created="2026-01-20",
    archived="2026-01-20T12:00:00Z",
    key_insight="A January note.",
    body="A short January note.",
)

# A flood of topically-STRONG decoys, all dated OUTSIDE the window and OLDER
# (2024) so the recency formula scores them below the in-window note. There are
# more of them than fetched_limit (= max(4*2, 10) = 10), so they monopolise the
# base arms' fetched window and push the weak in-window note out of it.
_DECOYS = [
    dict(
        name=f"r5-decoy-{i:02d}",
        title=f"Redis connection pool exhaustion postmortem {i}",
        category="database",
        tags=["redis", "connection", "pool", "exhaustion"],
        confidence="high",
        created="2024-06-15",
        archived="2024-06-15T12:00:00Z",
        key_insight="Redis connection pool exhaustion under load; size the connection pool to the worker count.",
        body=(
            "The redis connection pool exhausted under load. We made the decision "
            "that the redis connection pool must be sized to the worker count to "
            "avoid redis connection pool exhaustion. This redis connection pool "
            "exhaustion decision concerned redis connection pool exhaustion."
        ),
    )
    for i in range(14)
]

SEEDS = _DECOYS + [_IN_WINDOW]

# Disable the cross-encoder so the topic-blind recency formula — not CE's
# topical re-scoring — governs the final order. This isolates the R5 arm's RRF
# inclusion: whatever the temporal arm fuses in, the formula floats by recency.
NO_CE = {"RECALL_CROSS_ENCODER": "0", "RECALL_MMR": "0"}

# limit=4 => fetched_limit = max(4*2, 10) = 10 < 15 docs, so the base arms must
# drop some docs from their fetched window; the weak in-window note is the drop.
LIMIT = 4


def test_R5_temporal_arm(behavioral_kb):
    kb = behavioral_kb
    kb.seed(SEEDS)

    # ---- Phase 1: arm ON. The temporal arm filters the corpus to the January
    # window — only the in-window note matches — and fuses it into the pool; the
    # recency formula then floats it to the top of the served results. ----
    on = kb.recall_ids(WINDOW_QUERY, limit=LIMIT, env=NO_CE)
    assert _IN_WINDOW["name"] in on, (
        "with the temporal arm ON, the in-window note must be served — the arm "
        f"is its only path into the candidate pool; got {on}"
    )

    # ---- Phase 2: arm OFF (R6 extraction stays on; only the R5 arm dies). The
    # in-window note has no other path into the pool — the topical decoys fill
    # the fetched window — so it must disappear from the served results. ----
    off = kb.recall_ids(WINDOW_QUERY, limit=LIMIT, env={**NO_CE, "RECALL_TEMPORAL_ARM": "0"})
    assert _IN_WINDOW["name"] not in off, (
        "with the temporal arm OFF, the topically-weak in-window note must be "
        f"crowded out of the fetched window and NOT served; got {off}"
    )
