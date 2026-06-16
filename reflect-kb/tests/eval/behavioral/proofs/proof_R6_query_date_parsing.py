# ABOUTME: Behavioral proof for R6 — query-time natural-language date parsing.
# ABOUTME: A bare NL date phrase ("between A and B", "in march") surfaces a temporal block; a date-free query yields null.
"""R6 query-time date-parsing proof.

Invariant: recall.py parses a natural-language date expression OUT OF THE QUERY
STRING into the top-level ``temporal`` block of its JSON envelope — and does so
for phrases that contain NO explicit ISO date at all (a bare month name). A
query with no resolvable date expression yields ``temporal: null``. The query
text alone fully determines the outcome — no LLM, no seeded content, and (for
the explicit-range phrase) no wall-clock dependence participate in the
assertion.

Why this is the right R6 invariant (and why it is falsifiable):
  - R6 is the EXTRACTION stage that pairs with R5 (the temporal arm). R5's proof
    asserts date-window INCLUSION; R6's distinct, observable contribution is the
    parsed ``temporal`` block that recall.py emits. The R5 arm only fires when
    this block is non-null, so the block is the load-bearing surface of R6.
  - The bead's whole reason-for-being: "without it, only explicit ISO dates
    trigger temporal ranking." So the strongest falsifiable check is a phrase
    with ZERO digits — a bare natural-language month ("in march") — still
    producing a correct March window. An ISO-only extractor (or no extractor)
    would emit ``temporal: null`` here and the proof would FAIL.
  - The "between A and B" ISO range is asserted to the exact day so the
    extraction wiring is pinned with clock-stable bounds (the ISO literals fix
    the window regardless of today's date), proving the parse flows through the
    live recall.py path into the envelope — not just the unit-tested function.
  - The date-free control pins acceptance bullet 2 ("returns None cleanly when
    nothing matches") at the envelope level: no false temporal window is ever
    invented for an ordinary engineering query.

Each ``temporal`` block is ``{start, end, confidence, matched_text}`` (ISO
strings). recall.py calls ``extract_temporal_constraint(query)`` with no
reference date, so relative phrases resolve against ``datetime.now()``; the
bare-month assertions therefore pin only the CLOCK-STABLE facts of a March
window (month == 3, a sane span, in the past) rather than a wall-clock-fragile
exact year.

PORT: R6
"""
from __future__ import annotations

import datetime as _dt

# A minimal, date-free corpus. R6 extraction reads the QUERY, not the docs, so
# the seed content is irrelevant to the parse — it only needs to be a valid,
# indexable KB so recall.py runs its full pipeline and emits the envelope.
_SEEDS = [
    dict(
        name="r6-generic-note-a",
        title="Connection pools should match worker count",
        category="database",
        tags=["pool", "workers"],
        confidence="medium",
        created="2026-01-10",
        key_insight="Size the connection pool to the worker count.",
        body="A note about sizing connection pools to avoid exhaustion.",
    ),
    dict(
        name="r6-generic-note-b",
        title="Prefer canonical lock ordering",
        category="database",
        tags=["locking", "deadlock"],
        confidence="medium",
        created="2026-01-12",
        key_insight="Acquire locks in a consistent order.",
        body="A note about lock ordering to avoid deadlocks.",
    ),
]

# Explicit NL range — the ISO literals fix the window irrespective of the wall
# clock, so the exact-bound assertion is deterministic.
EXPLICIT_RANGE_QUERY = "anything between 2026-01-05 and 2026-01-12"

# Bare natural-language month — ZERO digits. This is the phrase an ISO-only
# extractor could NOT resolve; if R6 were absent the temporal block would be
# null here and the proof would fail.
BARE_MONTH_QUERY = "what broke in march"

# An ordinary engineering query with no date expression at all.
DATE_FREE_QUERY = "fix the auth bug in the login flow"


def _parse_iso(s: str) -> _dt.datetime:
    return _dt.datetime.fromisoformat(s)


def test_R6_query_date_parsing(behavioral_kb):
    kb = behavioral_kb
    kb.seed(_SEEDS)

    # ---- Phase 1: explicit NL range -> temporal block pinned to exact days ----
    payload = kb.recall(EXPLICIT_RANGE_QUERY)
    block = payload.get("temporal")
    assert block is not None, (
        "an explicit 'between A and B' date range in the query must be parsed "
        f"into a non-null temporal block; got payload temporal={block!r}"
    )
    start = _parse_iso(block["start"])
    end = _parse_iso(block["end"])
    assert (start.year, start.month, start.day) == (2026, 1, 5), (
        f"expected window start 2026-01-05, got {block['start']}"
    )
    assert (end.year, end.month, end.day) == (2026, 1, 12), (
        f"expected window end 2026-01-12, got {block['end']}"
    )
    assert start <= end, f"window must be ordered start<=end; got {block}"
    assert 0.0 < block["confidence"] <= 1.0, (
        f"confidence must be in (0,1]; got {block['confidence']}"
    )
    # The matched span must be the date phrase, not the whole query.
    assert "2026-01-05" in block["matched_text"] and "2026-01-12" in block["matched_text"], (
        f"matched_text should be the date phrase; got {block['matched_text']!r}"
    )

    # ---- Phase 2: BARE natural-language month (no digits) -> March window ----
    # This is the R6-defining case: an ISO-only path could not parse it.
    payload = kb.recall(BARE_MONTH_QUERY)
    block = payload.get("temporal")
    assert block is not None, (
        "a bare natural-language month ('in march') with NO ISO date must STILL "
        "be parsed into a temporal window — this is exactly what R6 adds over an "
        f"ISO-only extractor; got temporal={block!r}"
    )
    start = _parse_iso(block["start"])
    end = _parse_iso(block["end"])
    # Clock-stable facts of a March window: a full calendar March, ordered, past.
    assert start.month == 3 and start.day == 1, (
        f"'in march' must start at March 1; got {block['start']}"
    )
    assert end.month == 3 and end.day == 31, (
        f"'in march' must end at March 31; got {block['end']}"
    )
    assert start.year == end.year, (
        f"a single-month window must stay within one year; got {block}"
    )
    assert start < end, f"window must span the month; got {block}"
    assert start.date() < _dt.date.today(), (
        f"'in march' must resolve to a PAST March, never a future one; got {block['start']}"
    )

    # ---- Phase 3: date-free query -> temporal block is null (no false window) ----
    payload = kb.recall(DATE_FREE_QUERY)
    block = payload.get("temporal")
    assert block is None, (
        "an ordinary engineering query carries no date expression and must NOT "
        f"have a temporal window invented for it; got temporal={block!r}"
    )
