# ABOUTME: Behavioral proof for R2 — the cross-encoder rerank step in recall.py.
# ABOUTME: CE score is the PRIMARY sort key; the legacy formula is only a modifier.
"""R2 cross-encoder rerank proof.

Invariant (the heart of R2): after RRF fusion the candidates are scored
jointly with the query by a local cross-encoder (`reflect rerank`,
ms-marco-MiniLM-L-6-v2), and that CE score becomes the PRIMARY sort key —
``score = sigmoid(ce_logit) × formula``. The legacy
``confidence × recency × tags`` formula degrades to a bounded multiplicative
MODIFIER that cannot override a decisive CE verdict.

We prove this by engineering a tension that only the CE can resolve, then
toggling the CE off to show the ordering flips:

  - ``ce_winner``  — a learning that directly *answers* the query. The
    cross-encoder scores it high (logit ≈ +6.6, sigmoid ≈ 0.999). But we
    deliberately starve its formula signals: low confidence, archived long
    ago, no query-tag overlap.
  - ``formula_winner`` — a learning that does NOT answer the query (the
    cross-encoder scores it ≈ -11, sigmoid ≈ 1e-5) but is formula-advantaged:
    high confidence, archived today, full query-tag overlap.

The formula alone (each boost bounded to ≈[0.9, 1.1], so the product spans at
most ≈[0.53, 1.77] — a <4× ratio) prefers ``formula_winner``. The CE prefers
``ce_winner`` by ~5 orders of magnitude. So:

  - CE ON  (default)               → ce_winner outranks formula_winner.
  - CE OFF (RECALL_CROSS_ENCODER=0) → formula_winner outranks ce_winner.

The ranking *flips* on the toggle. With the CE step absent the proof's
CE-ON assertion necessarily fails, so the proof has no value unless the
cross-encoder is genuinely the primary sort key. No LLM participates in the
assertion — the seeds + the CE logits + the documented flag fully determine
the outcome.

The raw CE logits used to size this tension were measured against the real
model on this branch; they are reproduced in the module docstring above so a
future reader can see why the separation is decisive rather than marginal.

PORT: R2
"""
from __future__ import annotations

import pytest

# A learning that DIRECTLY answers the query. Surface wording overlaps the
# query enough that ms-marco scores it strongly (measured logit ≈ +6.6).
# Formula signals are deliberately weak so the formula does NOT prefer it.
_CE_WINNER = dict(
    name="r2-ce-winner",
    title="Flaky integration tests from shared database state",
    category="testing",
    tags=["unrelated-tag-z"],          # zero overlap with the query tag below
    confidence="low",                   # weak confidence boost
    created="2024-01-01",
    archived="2024-01-01T00:00:00",    # archived ~2.5 years ago → recency floor
    key_insight="Roll each test back in a transaction so no test sees another test's rows.",
    body=(
        "Flaky integration tests usually come from shared database state. Wrap "
        "each test in a transaction and roll it back in teardown, or truncate "
        "tables between tests, so no test sees another test's rows."
    ),
)

# A learning that does NOT answer the query (measured CE logit ≈ -11) but is
# formula-advantaged: high confidence, archived today, full query-tag overlap.
_FORMULA_WINNER = dict(
    name="r2-formula-winner",
    title="Dependency injection makes services testable",
    category="testing",
    tags=["testing", "di"],            # 'testing' matches the query tag → full overlap
    confidence="high",                  # strong confidence boost
    created="2026-06-12",
    archived="2026-06-12T00:00:00",    # archived yesterday → recency ceiling
    key_insight="Constructor injection lets you swap a real repository for a fake.",
    body=(
        "Use dependency injection to make services testable. Constructor "
        "injection lets you swap a real repository for a fake in unit tests, "
        "decoupling business logic from infrastructure."
    ),
)

QUERY = "fix flaky integration tests caused by shared database state between tests"
# A single query tag that ONLY formula_winner carries — biasing the formula
# toward formula_winner so the CE has to overcome it.
QUERY_TAGS = "testing"


def _rank_of(ids: list[str], name: str) -> int:
    assert name in ids, f"expected {name!r} in results, got {ids}"
    return ids.index(name)


def test_R2_cross_encoder_is_primary_sort_key(behavioral_kb):
    kb = behavioral_kb
    kb.seed([_CE_WINNER, _FORMULA_WINNER])

    # ---- CE ON (default): the cross-encoder verdict wins ----
    ids_on = kb.recall_ids(QUERY, tags=QUERY_TAGS, no_mmr=True)
    on_winner = _rank_of(ids_on, _CE_WINNER["name"])
    on_loser = _rank_of(ids_on, _FORMULA_WINNER["name"])
    assert on_winner < on_loser, (
        "with the cross-encoder ON the doc that ANSWERS the query "
        f"({_CE_WINNER['name']}) must outrank the formula-advantaged but "
        f"off-query doc ({_FORMULA_WINNER['name']}); got order {ids_on}"
    )

    # ---- CE OFF: only the legacy formula decides; the order FLIPS ----
    ids_off = kb.recall_ids(
        QUERY, tags=QUERY_TAGS, no_mmr=True, env={"RECALL_CROSS_ENCODER": "0"}
    )
    off_winner = _rank_of(ids_off, _FORMULA_WINNER["name"])
    off_loser = _rank_of(ids_off, _CE_WINNER["name"])
    assert off_winner < off_loser, (
        "with the cross-encoder OFF the formula-advantaged doc "
        f"({_FORMULA_WINNER['name']}) must win on confidence × recency × tags "
        f"alone; got order {ids_off}"
    )

    # The pair's relative order genuinely flipped on the CE toggle: this is
    # only possible if sigmoid(ce_logit) is the primary sort key and the
    # formula is a bounded modifier — exactly R2's acceptance contract.
    assert (ids_on.index(_CE_WINNER["name"]) < ids_on.index(_FORMULA_WINNER["name"])) != (
        ids_off.index(_CE_WINNER["name"]) < ids_off.index(_FORMULA_WINNER["name"])
    ), (
        f"expected the CE toggle to FLIP the pair order; "
        f"CE-on={ids_on} CE-off={ids_off}"
    )
