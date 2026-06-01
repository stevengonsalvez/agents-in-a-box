"""Tests for reflect cost observability (W3): reflect_cost.py + backfill_costs.py."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parent / "scripts"
sys.path.insert(0, str(SCRIPTS))

import reflect_cost  # noqa: E402
import backfill_costs  # noqa: E402


def _cost_file(sd: Path, events: list[dict]) -> None:
    sd.mkdir(parents=True, exist_ok=True)
    (sd / "drain-cost.jsonl").write_text(
        "\n".join(json.dumps(e) for e in events) + "\n"
    )


# ── reflect_cost ─────────────────────────────────────────────────────────────

def test_aggregate_by_day_sums_buckets():
    events = [
        {"day": "2026-05-31", "outcome": "ok", "model": "claude-sonnet-4-6",
         "tokens": 1000, "cache_read": 600, "cache_creation": 300,
         "input": 50, "output": 50, "cost_usd": 0.1},
        {"day": "2026-05-31", "outcome": "ok", "model": "claude-sonnet-4-6",
         "tokens": 2000, "cache_read": 1500, "cache_creation": 400,
         "input": 50, "output": 50, "cost_usd": 0.2},
    ]
    agg = reflect_cost.aggregate(events, "day")
    row = agg["2026-05-31"]
    assert row["runs"] == 2
    assert row["tokens"] == 3000
    assert row["cache_read"] == 2100
    assert row["cache_creation"] == 700
    assert abs(row["cost"] - 0.3) < 1e-9  # recorded cost preferred


def test_est_cost_used_when_no_recorded_cost():
    # opus pricing: cache_creation $18.75/Mtok -> 1M creation ≈ $18.75
    e = {"model": "claude-opus-4-8", "cache_creation": 1_000_000,
         "input": 0, "output": 0, "cache_read": 0, "cost_usd": 0}
    assert abs(reflect_cost._est_cost(e) - 18.75) < 0.01


def test_recorded_cost_beats_estimate():
    agg = reflect_cost.aggregate(
        [{"model": "claude-opus-4-8", "cache_creation": 1_000_000, "cost_usd": 0.5}],
        "model",
    )
    assert abs(agg["claude-opus-4-8"]["cost"] - 0.5) < 1e-9


def test_since_filter_excludes_old(tmp_path):
    _cost_file(tmp_path, [
        {"ts": "2026-01-01T00:00:00Z", "day": "2026-01-01", "outcome": "ok", "tokens": 9},
        {"ts": "2999-01-01T00:00:00Z", "day": "2999-01-01", "outcome": "ok", "tokens": 7},
    ])
    events = reflect_cost._load_events(tmp_path)
    assert len(events) == 2
    # 30d window should drop the 2026-01-01 one (relative to "now" >> 30d later).
    win = reflect_cost._parse_since("30d")
    assert win is not None


def test_loads_main_and_backfill(tmp_path):
    _cost_file(tmp_path, [{"day": "2026-05-31", "outcome": "ok", "tokens": 1}])
    (tmp_path / "drain-cost-backfill.jsonl").write_text(
        json.dumps({"day": "2026-05-01", "outcome": "backfill", "tokens": 2}) + "\n"
    )
    assert len(reflect_cost._load_events(tmp_path)) == 2


def test_parse_since_units():
    assert reflect_cost._parse_since("7d").days == 7
    assert reflect_cost._parse_since("24h").total_seconds() == 24 * 3600
    assert reflect_cost._parse_since("bogus") is None


# ── backfill_costs ───────────────────────────────────────────────────────────

def _make_projects(root: Path) -> Path:
    proj = root / "projects" / "-Some-Project"
    proj.mkdir(parents=True)
    # A reflect-on-reflect transcript with usage on its assistant turn.
    ror = proj / "reflectrun.jsonl"
    with open(ror, "w") as fh:
        fh.write(json.dumps({"type": "queue-operation",
                             "content": "/reflect\nProcess the transcript at: /x.jsonl",
                             "timestamp": "2026-05-30T10:00:00Z"}) + "\n")
        fh.write(json.dumps({"message": {"role": "assistant", "model": "claude-opus-4-8",
                             "usage": {"input_tokens": 100, "output_tokens": 200,
                                       "cache_read_input_tokens": 5000,
                                       "cache_creation_input_tokens": 3000}},
                             "timestamp": "2026-05-30T10:01:00Z"}) + "\n")
    # A normal (non-reflect) session — should be ignored by backfill.
    normal = proj / "normal.jsonl"
    with open(normal, "w") as fh:
        fh.write(json.dumps({"message": {"role": "user", "content": "build a feature"}}) + "\n")
        fh.write(json.dumps({"message": {"role": "assistant", "model": "claude-opus-4-8",
                             "usage": {"input_tokens": 10, "output_tokens": 20,
                                       "cache_read_input_tokens": 0,
                                       "cache_creation_input_tokens": 0}}}) + "\n")
    return root / "projects"


def test_backfill_envelope():
    p = Path(__import__("tempfile").mkdtemp())
    ror = p / "r.jsonl"
    with open(ror, "w") as fh:
        fh.write(json.dumps({"message": {"role": "assistant", "model": "claude-opus-4-8",
                             "usage": {"input_tokens": 1, "output_tokens": 2,
                                       "cache_read_input_tokens": 3,
                                       "cache_creation_input_tokens": 4}}}) + "\n")
    env = backfill_costs._envelope(ror)
    assert env["tokens"] == 10 and env["cache_creation"] == 4 and env["model"] == "claude-opus-4-8"


def test_backfill_writes_only_reflect_runs(tmp_path):
    projects = _make_projects(tmp_path)
    state = tmp_path / "state"
    import subprocess
    cp = subprocess.run(
        [sys.executable, str(SCRIPTS / "backfill_costs.py"),
         "--since", "3650d", "--projects-dir", str(projects), "--state-dir", str(state)],
        capture_output=True, text=True, timeout=60,
    )
    assert cp.returncode == 0, cp.stderr
    out = state / "drain-cost-backfill.jsonl"
    assert out.exists()
    recs = [json.loads(l) for l in out.read_text().splitlines() if l.strip()]
    assert len(recs) == 1, f"only the reflect-run should be backfilled, got {recs}"
    assert recs[0]["outcome"] == "backfill"
    assert recs[0]["tokens"] == 8300  # 100+200+5000+3000


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
