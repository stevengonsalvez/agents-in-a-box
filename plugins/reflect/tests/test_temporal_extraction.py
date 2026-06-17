# ABOUTME: Regression tests for port R6 — query-time date parsing. Pins the
# ABOUTME: fixture-query → expected-range table (acceptance bullet 1), the
# ABOUTME: clean None on date-less queries (bullet 2), the modifier shapes
# ABOUTME: (before/since/until/after), and the recall.py wiring.
"""Port R6: query-time date parsing.

Natural-language date phrases are parsed out of the query into a
TemporalRange{start, end, confidence} or None.

Acceptance bullets pinned here:
  1. covers 10 fixture queries with expected ranges
  2. returns None cleanly when nothing matches
"""

from __future__ import annotations

import sys
from datetime import datetime, timedelta
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
sys.path.insert(0, str(RECALL_SCRIPTS))

import recall as recall_mod  # noqa: E402
from temporal_extraction import (  # noqa: E402
    DISTANT_PAST,
    TemporalRange,
    extract_temporal_constraint,
)

# Wednesday 2026-06-10 — fixed reference so relative phrases are deterministic.
REF = datetime(2026, 6, 10, 12, 0, 0)


def _day_start(y: int, m: int, d: int) -> datetime:
    return datetime(y, m, d)


def _day_end(y: int, m: int, d: int) -> datetime:
    return datetime(y, m, d, 23, 59, 59, 999999)


# ---------- acceptance bullet 1: fixture queries → expected ranges ----------

FIXTURES = [
    # (query, expected_start_day, expected_end_day)
    ("what did we ship yesterday", (2026, 6, 9), (2026, 6, 9)),
    ("deploy failures today", (2026, 6, 10), (2026, 6, 10)),
    ("flaky tests last week", (2026, 6, 1), (2026, 6, 7)),  # prev Mon..Sun
    ("incidents last month", (2026, 5, 1), (2026, 5, 31)),
    ("migrations last year", (2025, 1, 1), (2025, 12, 31)),
    ("what broke in march", (2026, 3, 1), (2026, 3, 31)),  # most recent past
    ("the outage in march 2024", (2024, 3, 1), (2024, 3, 31)),
    ("regression 3 days ago", (2026, 6, 7), (2026, 6, 7)),
    ("anything between 2026-01-05 and 2026-01-12", (2026, 1, 5), (2026, 1, 12)),
    ("bugs from last sprint", (2026, 5, 13), (2026, 5, 27)),  # 2-week window
    ("releases in 2024", (2024, 1, 1), (2024, 12, 31)),
    ("last weekend's deploy", (2026, 6, 6), (2026, 6, 7)),  # prev Sat..Sun
]


@pytest.mark.parametrize("query,start_day,end_day", FIXTURES)
def test_fixture_queries_expected_ranges(query, start_day, end_day):
    rng = extract_temporal_constraint(query, reference_date=REF)
    assert rng is not None, f"no range parsed from {query!r}"
    assert rng.start == _day_start(*start_day)
    assert rng.end == _day_end(*end_day)
    assert 0.0 < rng.confidence <= 1.0
    assert rng.start <= rng.end


def test_fixture_table_covers_at_least_ten_queries():
    assert len(FIXTURES) >= 10  # acceptance: "covers 10 fixture queries"


# ---------- acceptance bullet 2: None cleanly when nothing matches ----------

NO_DATE_QUERIES = [
    "fix the auth bug in the login flow",
    "before the rewrite",  # codebase event, no date anchor to resolve
    "before the auth commit landed",
    "may I see the error handling approach",  # modal "may", not the month
    "march the troops forward",  # verb "march", no preposition context
    "tmux kill-server destroys all sessions",
    "",
    "   ",
]


@pytest.mark.parametrize("query", NO_DATE_QUERIES)
def test_returns_none_when_nothing_matches(query):
    assert extract_temporal_constraint(query, reference_date=REF) is None


def test_never_raises_on_pathological_input():
    # silent-fail discipline: parser bugs must never break the recall path
    for query in (
        "9999-99-99 and 0000-00-00",
        "between 2026-13-45 and 2026-99-99",
        "march " * 500,
        "\x00\x01 in march ￿",
        "1234-56-78 looked like a date",
    ):
        rng = extract_temporal_constraint(query, reference_date=REF)
        assert rng is None or rng.start <= rng.end


# ---------- modifiers: before / since / until / after ----------

def test_since_iso_date_opens_range_to_reference_day():
    rng = extract_temporal_constraint(
        "changes since 2026-06-01", reference_date=REF
    )
    assert rng is not None
    assert rng.start == _day_start(2026, 6, 1)
    assert rng.end == _day_end(2026, 6, 10)


def test_before_month_year_opens_range_from_distant_past():
    rng = extract_temporal_constraint(
        "decisions before march 2024", reference_date=REF
    )
    assert rng is not None
    assert rng.start == DISTANT_PAST
    # last instant before March 2024 starts
    assert rng.end == _day_end(2024, 2, 29)


def test_since_last_week_starts_at_week_start():
    rng = extract_temporal_constraint(
        "what changed since last week", reference_date=REF
    )
    assert rng is not None
    assert rng.start == _day_start(2026, 6, 1)
    assert rng.end == _day_end(2026, 6, 10)


def test_after_modifier_starts_past_anchor_end():
    rng = extract_temporal_constraint(
        "merges after 2026-06-01", reference_date=REF
    )
    assert rng is not None
    assert rng.start == _day_start(2026, 6, 2)
    assert rng.end == _day_end(2026, 6, 10)


def test_until_modifier_keeps_anchor_end():
    rng = extract_temporal_constraint(
        "history until march 2024", reference_date=REF
    )
    assert rng is not None
    assert rng.start == DISTANT_PAST
    assert rng.end == _day_end(2024, 3, 31)


# ---------- confidence ordering ----------

def test_confidence_explicit_beats_calendar_beats_fuzzy():
    explicit = extract_temporal_constraint("on 2026-06-01", reference_date=REF)
    calendar = extract_temporal_constraint("last week", reference_date=REF)
    bare_month = extract_temporal_constraint("in march", reference_date=REF)
    sprint = extract_temporal_constraint("last sprint", reference_date=REF)
    assert explicit.confidence == 1.0
    assert explicit.confidence > calendar.confidence
    assert calendar.confidence > bare_month.confidence
    assert bare_month.confidence > sprint.confidence


def test_month_with_year_more_confident_than_bare_month():
    with_year = extract_temporal_constraint("march 2024", reference_date=REF)
    bare = extract_temporal_constraint("in march", reference_date=REF)
    assert with_year.confidence > bare.confidence


# ---------- assorted phrase coverage ----------

def test_fuzzy_couple_of_days_ago_is_a_range():
    rng = extract_temporal_constraint(
        "a couple of days ago", reference_date=REF
    )
    assert rng is not None
    assert rng.start == _day_start(2026, 6, 7)  # ref - 3d
    assert rng.end == _day_end(2026, 6, 9)  # ref - 1d
    assert rng.confidence < 0.9  # imprecise phrase, lower confidence


def test_last_n_days_window_ends_at_reference():
    rng = extract_temporal_constraint(
        "errors in the last 7 days", reference_date=REF
    )
    assert rng is not None
    assert rng.start == _day_start(2026, 6, 3)
    assert rng.end == _day_end(2026, 6, 10)


def test_last_weekday_resolves_to_single_day():
    rng = extract_temporal_constraint("last friday", reference_date=REF)
    assert rng is not None
    assert rng.start == _day_start(2026, 6, 5)  # Friday before Wed 06-10
    assert rng.end == _day_end(2026, 6, 5)


def test_this_sprint_ends_at_reference():
    rng = extract_temporal_constraint("this sprint", reference_date=REF)
    assert rng is not None
    assert rng.start == _day_start(2026, 5, 27)  # ref - 14d
    assert rng.end == _day_end(2026, 6, 10)


def test_bare_month_after_reference_resolves_to_previous_year():
    # ref is June 2026; "in november" must mean November 2025, not the future
    rng = extract_temporal_constraint("in november", reference_date=REF)
    assert rng is not None
    assert rng.start == _day_start(2025, 11, 1)
    assert rng.end == _day_end(2025, 11, 30)


def test_word_number_quantities_parse():
    rng = extract_temporal_constraint("two days ago", reference_date=REF)
    assert rng is not None
    assert rng.start == _day_start(2026, 6, 8)
    assert rng.end == _day_end(2026, 6, 8)


def test_to_dict_shape():
    rng = extract_temporal_constraint("last week", reference_date=REF)
    d = rng.to_dict()
    assert set(d) == {"start", "end", "confidence", "matched_text"}
    assert d["start"] == "2026-06-01T00:00:00"
    assert d["end"] == "2026-06-07T23:59:59.999999"
    assert d["matched_text"] == "last week"
    assert isinstance(d["confidence"], float)


# ---------- recall.py wiring ----------

def test_recall_result_carries_temporal_even_on_error_path(monkeypatch):
    # CLI-missing error return must still surface the parsed range — the R5
    # temporal arm reads it regardless of retrieval outcome.
    monkeypatch.setattr(recall_mod, "find_learnings_cli", lambda: None)
    result = recall_mod.recall("deploys last week")
    assert result.error is not None
    assert isinstance(result.temporal, TemporalRange)
    assert result.temporal.start <= result.temporal.end


def test_recall_result_temporal_none_for_dateless_query(monkeypatch):
    monkeypatch.setattr(recall_mod, "find_learnings_cli", lambda: None)
    result = recall_mod.recall("fix the auth bug")
    assert result.temporal is None


def test_recall_temporal_env_gate(monkeypatch):
    monkeypatch.setattr(recall_mod, "find_learnings_cli", lambda: None)
    monkeypatch.setattr(recall_mod, "TEMPORAL_ENABLED", False)
    result = recall_mod.recall("deploys last week")
    assert result.temporal is None


def test_render_json_includes_temporal_block():
    import json

    rng = extract_temporal_constraint("last week", reference_date=REF)
    blob = json.loads(
        recall_mod.render_json([], "deploys last week", "naive", temporal=rng)
    )
    assert blob["temporal"]["start"] == "2026-06-01T00:00:00"
    assert blob["temporal"]["end"] == "2026-06-07T23:59:59.999999"
    assert blob["temporal"]["confidence"] == pytest.approx(0.9)


def test_render_json_temporal_null_when_absent():
    import json

    blob = json.loads(recall_mod.render_json([], "auth bug", "naive"))
    assert blob["temporal"] is None


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
