"""Tests for reflect_kb.metrics — best-effort JSONL metrics writer."""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import patch

import pytest

from reflect_kb import metrics


@pytest.fixture
def tmp_metrics(tmp_path: Path, monkeypatch):
    """Redirect METRICS_PATH to a tmp dir."""
    target = tmp_path / ".learnings" / "metrics.jsonl"
    monkeypatch.setattr(metrics, "METRICS_PATH", target)
    return target


def test_write_metric_appends_jsonl_line(tmp_metrics: Path):
    metrics.write_metric("search", query="foo", hits=3, latency_ms=42)
    assert tmp_metrics.exists()
    lines = tmp_metrics.read_text().strip().splitlines()
    assert len(lines) == 1
    record = json.loads(lines[0])
    assert record["op"] == "search"
    assert record["query"] == "foo"
    assert record["hits"] == 3
    assert record["latency_ms"] == 42
    assert "ts" in record
    assert "harness" in record


def test_write_metric_multiple_lines(tmp_metrics: Path):
    metrics.write_metric("search", query="a")
    metrics.write_metric("add", doc="b")
    lines = tmp_metrics.read_text().strip().splitlines()
    assert len(lines) == 2
    assert json.loads(lines[0])["op"] == "search"
    assert json.loads(lines[1])["op"] == "add"


def test_write_metric_never_raises_on_broken_path(monkeypatch, tmp_path: Path):
    # Point METRICS_PATH at a path whose parent can't be created.
    # We simulate by monkeypatching mkdir to raise.
    monkeypatch.setattr(metrics, "METRICS_PATH", tmp_path / "nope.jsonl")
    with patch.object(Path, "mkdir", side_effect=PermissionError("denied")):
        # Must not raise.
        metrics.write_metric("search", query="x")


def test_detect_harness_env(monkeypatch):
    monkeypatch.setenv("CLAUDECODE", "1")
    assert metrics._detect_harness() == "claude"
    monkeypatch.delenv("CLAUDECODE")
    monkeypatch.setenv("CODEX_CLI", "1")
    assert metrics._detect_harness() == "codex"
    monkeypatch.delenv("CODEX_CLI")
    assert metrics._detect_harness() == "other"


def test_rotation_at_10mb(tmp_metrics: Path, monkeypatch):
    # Force tiny rotation threshold so we can trigger it quickly.
    monkeypatch.setattr(metrics, "MAX_BYTES", 100)
    tmp_metrics.parent.mkdir(parents=True, exist_ok=True)
    tmp_metrics.write_text("x" * 200)  # exceed threshold

    metrics.write_metric("search", query="after-rotate")

    # Old contents rotated to a .bak file; new file has just one line.
    baks = list(tmp_metrics.parent.glob("metrics-*.jsonl.bak"))
    assert len(baks) == 1
    assert tmp_metrics.exists()
    new_lines = tmp_metrics.read_text().strip().splitlines()
    assert len(new_lines) == 1
    assert json.loads(new_lines[0])["query"] == "after-rotate"
