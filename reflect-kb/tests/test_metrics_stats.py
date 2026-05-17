"""Tests for the JSONL stats aggregator (v4 §Phase 5)."""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path

import pytest

from reflect_kb import metrics_stats

FIXTURE = Path(__file__).parent / "fixtures" / "metrics_sample.jsonl"
# Anchor "now" inside the fixture so the 7d window is deterministic.
NOW = datetime(2026, 4, 24, 12, 0, 0, tzinfo=timezone.utc)


def test_aggregate_against_fixture():
    report = metrics_stats.aggregate(FIXTURE, now=NOW)

    assert report.metrics_path == str(FIXTURE)

    # All-time: garbage line ignored. 12 valid records, 10 of them recall.
    at = report.all_time
    assert at.total_events == 12
    assert at.recall_events == 10
    # 7 of those recalls had hits>0 (rows w/ hits 3,1,5,2,4,2,1).
    assert at.recall_with_hits == 7
    assert at.hit_rate == pytest.approx(0.7, rel=1e-6)

    # Last 7 days (apr 17..24): excludes the apr 15 records → 8 recalls.
    last7 = report.last_7d
    assert last7.label == "last-7d"
    assert last7.total_events == 10  # 8 recalls + 1 add + 1 share inside window
    assert last7.recall_events == 8
    assert last7.recall_with_hits == 6  # all hits>0 except the two zero-hit recalls in window


def test_top_tags_ranked_by_count():
    report = metrics_stats.aggregate(FIXTURE, now=NOW)
    tag_map = dict(report.all_time.top_tags)
    # tokio appears in 4 recalls → most common
    assert tag_map["tokio"] == 4
    assert tag_map["rust"] == 5  # tokio recalls + 1 old "rust" recall
    # Top tags list is bounded to 10
    assert len(report.all_time.top_tags) <= 10


def test_p50_p95_latency_sorted_correctly():
    report = metrics_stats.aggregate(FIXTURE, now=NOW)
    # All-time latencies (recall only): [12,18,7,22,31,15,9,40,10,50] → sorted
    # [7,9,10,12,15,18,22,31,40,50]; p50 ≈ 16.5, p95 ≈ 45.5 (linear interp)
    assert 15.0 <= report.all_time.p50_latency_ms <= 18.0
    assert 40.0 <= report.all_time.p95_latency_ms <= 50.0


def test_missing_metrics_file_returns_empty_stats(tmp_path):
    report = metrics_stats.aggregate(tmp_path / "nope.jsonl", now=NOW)
    assert report.all_time.total_events == 0
    assert report.all_time.recall_events == 0
    assert report.all_time.hit_rate == 0.0
    assert report.all_time.p50_latency_ms is None
    assert report.all_time.p95_latency_ms is None
    assert report.all_time.top_tags == []


def test_to_dict_is_jsonable():
    """Stats payload must be plain-JSON-serialisable."""
    import json
    report = metrics_stats.aggregate(FIXTURE, now=NOW)
    # round-trip — no custom types should survive
    encoded = json.dumps(report.to_dict())
    decoded = json.loads(encoded)
    assert decoded["all_time"]["recall_events"] == 10


def test_window_days_overrides_threshold():
    # Tightening to 1 day with NOW=2026-04-24 12:00 → threshold 04-23 12:00.
    # Records strictly after that timestamp: 4 (3 recalls + 1 share).
    report = metrics_stats.aggregate(FIXTURE, now=NOW, window_days=1)
    assert report.last_7d.label == "last-1d"
    assert report.last_7d.total_events == 4
    assert report.last_7d.recall_events == 3


def test_percentile_helper():
    assert metrics_stats._percentile([], 50) is None
    assert metrics_stats._percentile([10.0], 95) == 10.0
    assert metrics_stats._percentile([1.0, 2.0, 3.0, 4.0, 5.0], 50) == 3.0
