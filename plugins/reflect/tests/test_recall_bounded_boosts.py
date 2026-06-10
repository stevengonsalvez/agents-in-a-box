# ABOUTME: Regression tests for port R8 — multiplicative bounded boosts in
# ABOUTME: recall.py rerank. Pins the Hindsight apply_combined_scoring shape
# ABOUTME: (boost = 1 + α·(norm − 0.5), each signal clamped to ±α/2), rank
# ABOUTME: order under known signal mixes, and per-α env configuration.
"""Port R8: multiplicative bounded boosts.

score = CE × confidence_boost × recency_boost × tag_boost × proof_boost,
each boost bounded to [1 − α/2, 1 + α/2] (confidence/recency/tags α=0.2 →
±10%; proof count α=0.1 → ±5%).

Acceptance bullets pinned here:
  1. unit tests fix the rank order under known signal mixes
  2. each boost stays in declared range
"""

from __future__ import annotations

import importlib
import math
import sys
from datetime import datetime, timedelta
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
sys.path.insert(0, str(RECALL_SCRIPTS))

import recall as recall_mod  # noqa: E402
from recall import (  # noqa: E402
    CONFIDENCE_ALPHA,
    PROOF_COUNT_ALPHA,
    RECENCY_ALPHA,
    TAG_ALPHA,
    Learning,
    _ce_sigmoid,
    _env_alpha,
    _learning_key,
    bounded_boost,
    confidence_norm,
    proof_count_boost,
    recency_norm,
    rerank,
    rerank_with_scores,
    tag_norm,
)

NOW = datetime(2026, 6, 10, 12, 0, 0)


def _lrn(
    name: str,
    confidence: str = "medium",
    archived_at: str | None = None,
    tags: list[str] | None = None,
    proof: int | None = None,
) -> Learning:
    fm: dict = {"name": name, "confidence": confidence}
    if tags is not None:
        fm["tags"] = tags
    if proof is not None:
        fm["proof_count"] = proof
    return Learning(
        chunk_text=f"learning body for {name}",
        frontmatter=fm,
        archived_at=archived_at,
    )


def _days_ago(days: float) -> str:
    return (NOW - timedelta(days=days)).isoformat()


# ---------- bounded_boost: the Hindsight shape ----------

def test_bounded_boost_shape_and_neutrality():
    assert bounded_boost(0.5, 0.2) == pytest.approx(1.0)  # neutral norm
    assert bounded_boost(1.0, 0.2) == pytest.approx(1.1)  # ceiling 1 + α/2
    assert bounded_boost(0.0, 0.2) == pytest.approx(0.9)  # floor 1 − α/2
    assert bounded_boost(0.75, 0.1) == pytest.approx(1.025)


def test_bounded_boost_clamps_out_of_range_norms():
    """A buggy upstream normalizer must never escape the declared range."""
    assert bounded_boost(5.0, 0.2) == pytest.approx(1.1)
    assert bounded_boost(-5.0, 0.2) == pytest.approx(0.9)
    assert bounded_boost(math.inf, 0.2) == pytest.approx(1.1)
    assert bounded_boost(-math.inf, 0.2) == pytest.approx(0.9)


# ---------- acceptance 2: each boost stays in declared range ----------

def test_confidence_boost_stays_within_ten_percent():
    lo, hi = 1.0 - CONFIDENCE_ALPHA / 2, 1.0 + CONFIDENCE_ALPHA / 2
    for tier in ("HIGH", "MEDIUM", "LOW", "BOGUS", ""):
        assert lo <= bounded_boost(confidence_norm(tier), CONFIDENCE_ALPHA) <= hi
    assert bounded_boost(confidence_norm("HIGH"), CONFIDENCE_ALPHA) == pytest.approx(1.1)
    assert bounded_boost(confidence_norm("MEDIUM"), CONFIDENCE_ALPHA) == pytest.approx(1.0)
    assert bounded_boost(confidence_norm("LOW"), CONFIDENCE_ALPHA) == pytest.approx(0.9)
    assert bounded_boost(confidence_norm("BOGUS"), CONFIDENCE_ALPHA) == pytest.approx(1.0)


def test_recency_boost_stays_within_ten_percent():
    lo, hi = 1.0 - RECENCY_ALPHA / 2, 1.0 + RECENCY_ALPHA / 2
    dates = [
        _days_ago(0),       # brand new → norm 1.0
        _days_ago(180),     # half a year
        _days_ago(365),     # window edge
        _days_ago(3650),    # decade old → norm floor 0.1
        _days_ago(-30),     # future-dated → clamped to 1.0
        None,               # undated → neutral 0.5
        "not-a-date",       # malformed → neutral 0.5
        "2026-01-01T00:00:00+00:00",  # tz-aware vs naive now → neutral 0.5
    ]
    for archived in dates:
        boost = bounded_boost(recency_norm(archived, NOW), RECENCY_ALPHA)
        assert lo <= boost <= hi, f"recency boost {boost} out of range for {archived!r}"
    # The 0.1 norm floor: ancient notes bottom out at 1 + α·(0.1 − 0.5).
    ancient = bounded_boost(recency_norm(_days_ago(3650), NOW), RECENCY_ALPHA)
    assert ancient == pytest.approx(1.0 + RECENCY_ALPHA * (0.1 - 0.5))
    assert bounded_boost(recency_norm(None, NOW), RECENCY_ALPHA) == pytest.approx(1.0)


def test_tag_boost_stays_within_ten_percent():
    lo, hi = 1.0 - TAG_ALPHA / 2, 1.0 + TAG_ALPHA / 2
    qt = {"redis", "pool"}
    cases = [
        (set(), ["redis", "pool"]),       # no query tags → neutral
        (qt, []),                          # zero overlap
        (qt, ["redis"]),                   # partial
        (qt, ["redis", "pool", "extra"]),  # full coverage + extras
        (qt, ["redis"] * 50 + ["pool"]),   # duplicates can't overflow
    ]
    for query_tags, learning_tags in cases:
        assert lo <= bounded_boost(tag_norm(query_tags, learning_tags), TAG_ALPHA) <= hi
    assert bounded_boost(tag_norm(set(), ["redis"]), TAG_ALPHA) == pytest.approx(1.0)
    assert bounded_boost(tag_norm(qt, ["redis", "pool"]), TAG_ALPHA) == pytest.approx(1.1)
    assert bounded_boost(tag_norm(qt, []), TAG_ALPHA) == pytest.approx(0.9)


def test_proof_boost_stays_within_five_percent():
    lo, hi = 1.0 - PROOF_COUNT_ALPHA / 2, 1.0 + PROOF_COUNT_ALPHA / 2
    for pc in (None, 0, 1, 2, 20, 150, 10**9):
        assert lo <= proof_count_boost(pc) <= hi
    assert proof_count_boost(10**9) == pytest.approx(hi)  # ln clamp at norm 1.0
    assert proof_count_boost(None) == pytest.approx(1.0)
    assert proof_count_boost(1) == pytest.approx(1.0)


def test_combined_boost_product_is_bounded():
    """Worst-case stack of all four boosts stays within the product of the
    declared per-signal ranges — no hidden unbounded term remains."""
    hi = (1 + CONFIDENCE_ALPHA / 2) * (1 + RECENCY_ALPHA / 2) * (1 + TAG_ALPHA / 2) * (1 + PROOF_COUNT_ALPHA / 2)
    lo = (1 - CONFIDENCE_ALPHA / 2) * (1 - RECENCY_ALPHA / 2) * (1 - TAG_ALPHA / 2) * (1 - PROOF_COUNT_ALPHA / 2)
    best = _lrn("best", confidence="high", archived_at=_days_ago(0),
                tags=["redis", "pool"], proof=10**9)
    worst = _lrn("worst", confidence="low", archived_at=_days_ago(3650),
                 tags=[], proof=1)
    ce = {"best": 0.0, "worst": 0.0}  # sigmoid(0) = 0.5 exactly
    _, scores = rerank_with_scores([best, worst], query_tags=["redis", "pool"],
                                   now=NOW, ce_scores=ce)
    assert 0.5 * lo <= scores["worst"] <= scores["best"] <= 0.5 * hi


# ---------- acceptance 1: rank order under known signal mixes ----------

def test_recent_low_quality_cannot_outrank_old_high_quality():
    """The bead's WHY, pinned: under the old exp(-age/90) recency multiplier
    a year-old HIGH note scored ~2% of a fresh LOW note. Bounded boosts cap
    recency at ±10%, so quality (±10%) holds the line."""
    old_high = _lrn("old-high", confidence="high", archived_at=_days_ago(365))
    fresh_low = _lrn("fresh-low", confidence="low", archived_at=_days_ago(0))
    out = rerank([fresh_low, old_high], now=NOW)
    assert [x.id for x in out] == ["old-high", "fresh-low"]


def test_recency_breaks_ties_between_equal_quality():
    newer = _lrn("newer", confidence="high", archived_at=_days_ago(5))
    older = _lrn("older", confidence="high", archived_at=_days_ago(300))
    out = rerank([older, newer], now=NOW)
    assert [x.id for x in out] == ["newer", "older"]


def test_confidence_breaks_ties_between_equal_recency():
    high = _lrn("high", confidence="high", archived_at=_days_ago(10))
    low = _lrn("low", confidence="low", archived_at=_days_ago(10))
    out = rerank([low, high], now=NOW)
    assert [x.id for x in out] == ["high", "low"]


def test_tag_overlap_breaks_ties():
    tagged = _lrn("tagged", tags=["redis", "pool"])
    untagged = _lrn("untagged", tags=["unrelated"])
    out = rerank([untagged, tagged], query_tags=["redis", "pool"], now=NOW)
    assert [x.id for x in out] == ["tagged", "untagged"]


def test_proof_count_breaks_ties():
    evidenced = _lrn("evidenced", proof=50)
    single = _lrn("single", proof=1)
    out = rerank([single, evidenced], now=NOW)
    assert [x.id for x in out] == ["evidenced", "single"]


def test_ce_stays_primary_over_worst_case_boost_stack():
    """A meaningful CE gap must survive every boost stacked against it.

    The worst-case stack ratio is (1.1³·1.05)/(0.9·0.92·0.9·0.95) ≈ 1.97×,
    so any CE ratio above that — here sigmoid(2)/sigmoid(−1) ≈ 3.3× — keeps
    semantic relevance primary."""
    relevant = _lrn("relevant", confidence="low", archived_at=_days_ago(3650),
                    tags=[], proof=1)
    stacked = _lrn("stacked", confidence="high", archived_at=_days_ago(0),
                   tags=["redis", "pool"], proof=10**9)
    ce = {"relevant": 2.0, "stacked": -1.0}
    out = rerank([stacked, relevant], query_tags=["redis", "pool"],
                 now=NOW, ce_scores=ce)
    assert [x.id for x in out] == ["relevant", "stacked"]


def test_equal_ce_ordering_reduces_to_boost_product():
    docs = [
        _lrn("low", confidence="low"),
        _lrn("high", confidence="high"),
        _lrn("mid", confidence="medium"),
    ]
    ce = {d.id: 3.0 for d in docs}
    assert [x.id for x in rerank(list(docs), ce_scores=ce, now=NOW)] == ["high", "mid", "low"]


def test_score_is_exact_product_of_ce_and_boosts():
    """The combined score decomposes into exactly CE × the four boosts —
    nothing else is mixed in."""
    lrn = _lrn("doc", confidence="high", archived_at=_days_ago(100),
               tags=["redis"], proof=7)
    ce = {"doc": 1.5}
    _, scores = rerank_with_scores([lrn], query_tags=["redis", "pool"],
                                   now=NOW, ce_scores=ce)
    expected = (
        _ce_sigmoid(1.5)
        * bounded_boost(confidence_norm("HIGH"), CONFIDENCE_ALPHA)
        * bounded_boost(recency_norm(_days_ago(100), NOW), RECENCY_ALPHA)
        * bounded_boost(tag_norm({"redis", "pool"}, ["redis"]), TAG_ALPHA)
        * proof_count_boost(7)
    )
    assert scores[_learning_key(lrn)] == pytest.approx(expected)


def test_all_neutral_signals_score_exactly_ce():
    """Undated, medium-confidence, untagged-query, proofless learning:
    every boost collapses to 1.0 and the score is the CE sigmoid alone."""
    lrn = _lrn("plain", confidence="medium")
    ce = {"plain": -1.0}
    _, scores = rerank_with_scores([lrn], now=NOW, ce_scores=ce)
    assert scores[_learning_key(lrn)] == pytest.approx(_ce_sigmoid(-1.0))


def test_without_ce_stable_order_for_identical_signals():
    a = _lrn("a")
    b = _lrn("b")
    assert rerank([a, b], now=NOW) == [a, b]


# ---------- per-α configuration ----------

def test_env_alpha_parses_overrides_and_rejects_garbage(monkeypatch):
    monkeypatch.setenv("RECALL_TEST_ALPHA", "0.5")
    assert _env_alpha("RECALL_TEST_ALPHA", 0.2) == 0.5
    monkeypatch.setenv("RECALL_TEST_ALPHA", "junk")
    assert _env_alpha("RECALL_TEST_ALPHA", 0.2) == 0.2
    monkeypatch.setenv("RECALL_TEST_ALPHA", "-3")
    assert _env_alpha("RECALL_TEST_ALPHA", 0.2) == 0.0  # clamped: never negative
    monkeypatch.setenv("RECALL_TEST_ALPHA", "99")
    assert _env_alpha("RECALL_TEST_ALPHA", 0.2) == 2.0  # clamped ceiling
    monkeypatch.delenv("RECALL_TEST_ALPHA")
    assert _env_alpha("RECALL_TEST_ALPHA", 0.2) == 0.2


def test_each_alpha_has_its_own_env_knob(monkeypatch):
    monkeypatch.setenv("RECALL_CONFIDENCE_ALPHA", "0.4")
    monkeypatch.setenv("RECALL_RECENCY_ALPHA", "0.3")
    monkeypatch.setenv("RECALL_TAG_ALPHA", "0.1")
    monkeypatch.setenv("RECALL_PROOF_ALPHA", "0.05")
    try:
        importlib.reload(recall_mod)
        assert recall_mod.CONFIDENCE_ALPHA == 0.4
        assert recall_mod.RECENCY_ALPHA == 0.3
        assert recall_mod.TAG_ALPHA == 0.1
        assert recall_mod.PROOF_COUNT_ALPHA == 0.05
    finally:
        monkeypatch.undo()
        importlib.reload(recall_mod)
    assert recall_mod.CONFIDENCE_ALPHA == CONFIDENCE_ALPHA
    assert recall_mod.PROOF_COUNT_ALPHA == PROOF_COUNT_ALPHA


def test_default_alphas_match_hindsight_calibration():
    assert RECENCY_ALPHA == 0.2
    assert PROOF_COUNT_ALPHA == 0.1
    assert CONFIDENCE_ALPHA == 0.2
    assert TAG_ALPHA == 0.2


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
