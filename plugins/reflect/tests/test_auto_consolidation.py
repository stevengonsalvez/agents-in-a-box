# ABOUTME: Regression tests for port C2 — auto-trigger consolidation on N learnings.
# ABOUTME: Pins the pending-learnings counter metric (learnings_since_last_
# ABOUTME: consolidation), the threshold-crossing early trigger, the weekly
# ABOUTME: age fallback, and the counter reset after a completed pass.
"""Port C2: trigger the weekly Opus synthesis EARLY when N (default 30)
new learnings have landed (the Hindsight enable_auto_consolidation shape).

Acceptance criteria pinned here:
  1. cross-threshold triggers early run
  2. metric exposes 'learnings since last consolidation'
"""

from __future__ import annotations

import json
import os
import plistlib
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
sys.path.insert(0, str(SCRIPTS))

import reflect_db  # noqa: E402
import reflect_synthesis  # noqa: E402

SYNTHESIS_CLI = SCRIPTS / "reflect_synthesis.py"
METRICS_CLI = SCRIPTS / "metrics_updater.py"
PENDING_KEY = reflect_synthesis.PENDING_LEARNINGS_KEY
LAST_RUN_KEY = reflect_synthesis.LAST_CONSOLIDATION_KEY


@pytest.fixture
def conn(tmp_path):
    """Fresh isolated DB per test; never touches ~/.reflect."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    yield connection
    reflect_db.close_all()


def _iso_ago(days: float) -> str:
    return (datetime.now(timezone.utc) - timedelta(days=days)).isoformat()


def _add(conn, n: int) -> None:
    for i in range(n):
        reflect_db.add_learning(f"learning number {i} about topic {i}", conn=conn)


def _count(conn) -> int:
    return reflect_synthesis.learnings_since_last_consolidation(
        conn=conn, db=reflect_db
    )


def _trigger(conn, threshold, max_age_seconds=7 * 86400):
    return reflect_synthesis.should_auto_trigger(
        threshold, max_age_seconds, conn=conn, db=reflect_db
    )


# ---------- acceptance 2: metric exposes learnings since last consolidation ----------


def test_counter_counts_all_learnings_with_no_baseline(conn):
    _add(conn, 3)
    assert _count(conn) == 3
    # The value is mirrored into the metrics table — the exposed observable.
    assert reflect_db.get_metric(PENDING_KEY, conn=conn) == 3


def test_counter_resets_after_consolidation_run(conn):
    _add(conn, 3)
    assert _count(conn) == 3
    reflect_synthesis.record_consolidation_run(conn=conn, db=reflect_db)
    assert reflect_db.get_metric(PENDING_KEY, conn=conn) == 0
    assert _count(conn) == 0
    assert reflect_db.get_metric(LAST_RUN_KEY, conn=conn)


def test_counter_only_counts_learnings_after_baseline(conn):
    _add(conn, 2)
    reflect_synthesis.record_consolidation_run(
        when=datetime.now(timezone.utc).isoformat(), conn=conn, db=reflect_db
    )
    _add(conn, 1)
    assert _count(conn) == 1


def test_metrics_updater_show_exposes_counter(tmp_path):
    db_file = tmp_path / "reflect.db"
    c = reflect_db.init_db(db_file)
    _add(c, 2)
    reflect_db.close_all()

    env = dict(os.environ, REFLECT_DB_PATH=str(db_file))
    out = subprocess.run(
        [sys.executable, str(METRICS_CLI), "--show"],
        capture_output=True, text=True, env=env, timeout=60,
    )
    assert out.returncode == 0, out.stderr
    assert "Learnings Since Last Consolidation: 2" in out.stdout


def test_metrics_updater_get_key_is_live(tmp_path):
    db_file = tmp_path / "reflect.db"
    c = reflect_db.init_db(db_file)
    _add(c, 4)
    reflect_db.close_all()

    env = dict(os.environ, REFLECT_DB_PATH=str(db_file))
    out = subprocess.run(
        [sys.executable, str(METRICS_CLI),
         "--action", "get", "--key", "learnings_since_last_consolidation",
         "--json"],
        capture_output=True, text=True, env=env, timeout=60,
    )
    assert out.returncode == 0, out.stderr
    payload = json.loads(out.stdout)
    assert payload["key"] == "learnings_since_last_consolidation"
    assert payload["value"] == 4


# ---------- acceptance 1: cross-threshold triggers early run ----------


def test_below_threshold_does_not_trigger(conn):
    reflect_synthesis.record_consolidation_run(conn=conn, db=reflect_db)
    _add(conn, 2)
    triggered, reason, count = _trigger(conn, threshold=5)
    assert triggered is False
    assert count == 2
    assert "below threshold" in reason


def test_cross_threshold_triggers_early_run(conn):
    reflect_synthesis.record_consolidation_run(conn=conn, db=reflect_db)
    _add(conn, 5)
    triggered, reason, count = _trigger(conn, threshold=5)
    assert triggered is True
    assert count == 5
    assert "threshold crossed" in reason


def test_age_fallback_preserves_weekly_cadence(conn):
    # Quiet project: zero new learnings, but the last run is 8 days old.
    reflect_db.set_metric(LAST_RUN_KEY, _iso_ago(8), conn=conn)
    triggered, reason, _ = _trigger(conn, threshold=30)
    assert triggered is True
    assert "age fallback" in reason


def test_fresh_run_does_not_age_trigger(conn):
    reflect_db.set_metric(LAST_RUN_KEY, _iso_ago(1), conn=conn)
    triggered, _, _ = _trigger(conn, threshold=30)
    assert triggered is False


def test_empty_db_never_triggers(conn):
    triggered, _, count = _trigger(conn, threshold=30)
    assert triggered is False
    assert count == 0


def test_no_baseline_anchors_age_on_oldest_learning(conn):
    # Fresh install with a stale, sub-threshold backlog still gets a pass.
    _add(conn, 1)
    conn.execute("UPDATE learnings SET created_at = ?", (_iso_ago(8),))
    conn.commit()
    triggered, reason, _ = _trigger(conn, threshold=30)
    assert triggered is True
    assert "age fallback" in reason


def test_default_threshold_is_30_and_env_overrides(monkeypatch):
    monkeypatch.delenv("REFLECT_SYNTHESIS_AUTO_THRESHOLD", raising=False)
    assert reflect_synthesis.auto_trigger_threshold() == 30
    monkeypatch.setenv("REFLECT_SYNTHESIS_AUTO_THRESHOLD", "5")
    assert reflect_synthesis.auto_trigger_threshold() == 5
    monkeypatch.setenv("REFLECT_SYNTHESIS_AUTO_THRESHOLD", "nonsense")
    assert reflect_synthesis.auto_trigger_threshold() == 30


# ---------- CLI: --check-auto (the launchd tick entry point) ----------


def _run_check_auto(db_file, docs_dir, *extra, threshold="3"):
    env = dict(
        os.environ,
        REFLECT_DB_PATH=str(db_file),
        REFLECT_SYNTHESIS_AUTO_THRESHOLD=threshold,
    )
    return subprocess.run(
        [sys.executable, str(SYNTHESIS_CLI), "--check-auto",
         "--docs-dir", str(docs_dir), *extra],
        capture_output=True, text=True, env=env, timeout=60,
    )


def test_check_auto_cli_skips_below_threshold(tmp_path):
    db_file = tmp_path / "reflect.db"
    c = reflect_db.init_db(db_file)
    reflect_synthesis.record_consolidation_run(conn=c, db=reflect_db)
    _add(c, 1)
    reflect_db.close_all()

    out = _run_check_auto(db_file, tmp_path / "docs")
    assert out.returncode == 0, out.stderr
    assert "below threshold" in out.stdout
    # No-trigger means the docs scan never ran.
    assert "learnings in window" not in out.stdout


def test_check_auto_cli_cross_threshold_runs_and_resets(tmp_path):
    db_file = tmp_path / "reflect.db"
    c = reflect_db.init_db(db_file)
    reflect_synthesis.record_consolidation_run(conn=c, db=reflect_db)
    _add(c, 3)
    reflect_db.close_all()

    docs = tmp_path / "docs"
    docs.mkdir()
    out = _run_check_auto(db_file, docs)
    assert out.returncode == 0, out.stderr
    assert "threshold crossed" in out.stdout
    assert "learnings in window" in out.stdout  # the early run actually ran

    # The completed pass re-arms the trigger: counter zeroed, baseline stamped.
    c = reflect_db.init_db(db_file)
    try:
        assert reflect_db.get_metric("learnings_since_last_consolidation", conn=c) == 0
        assert reflect_synthesis.learnings_since_last_consolidation(
            conn=c, db=reflect_db
        ) == 0
    finally:
        reflect_db.close_all()


def test_check_auto_cli_dry_run_does_not_reset(tmp_path):
    db_file = tmp_path / "reflect.db"
    c = reflect_db.init_db(db_file)
    reflect_synthesis.record_consolidation_run(conn=c, db=reflect_db)
    _add(c, 3)
    reflect_db.close_all()

    docs = tmp_path / "docs"
    docs.mkdir()
    out = _run_check_auto(db_file, docs, "--dry-run")
    assert out.returncode == 0, out.stderr
    assert "threshold crossed" in out.stdout

    c = reflect_db.init_db(db_file)
    try:
        assert reflect_synthesis.learnings_since_last_consolidation(
            conn=c, db=reflect_db
        ) == 3
    finally:
        reflect_db.close_all()


def test_check_auto_cli_survives_broken_db(tmp_path):
    db_file = tmp_path / "reflect.db"
    db_file.write_text("this is not a sqlite database padding padding padding")
    out = _run_check_auto(db_file, tmp_path / "docs")
    # Silent-fail shaped: a broken DB must not crash the launchd tick.
    assert out.returncode == 0
    assert "auto-check skipped" in out.stderr


# ---------- launchd plist wires the tick ----------


def test_synthesis_plist_runs_check_auto_hourly():
    plist_path = PLUGIN_ROOT / "launchd" / "com.reflect.synthesis.plist"
    with open(plist_path, "rb") as fh:
        data = plistlib.load(fh)
    assert "--check-auto" in data["ProgramArguments"]
    assert data["StartInterval"] == 3600
    assert data["Label"] == "com.reflect.synthesis"


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
