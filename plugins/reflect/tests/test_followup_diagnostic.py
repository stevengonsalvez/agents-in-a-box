# ABOUTME: Regression tests for port A4 — the followup-rate diagnostic
# ABOUTME: (recall-quality self-monitor, agentmemory smart-search shape).
# ABOUTME: Pins the three acceptance bullets: (1) the followup flag is set
# ABOUTME: when the next search in the same session within the window returns
# ABOUTME: a disjoint result set, (2) the cost skill shows the followup rate
# ABOUTME: (reflect_cost.py --followup + SKILL.md), (3) metrics.jsonl carries
# ABOUTME: the counter (op="recall_search" lines with a followup field).
"""Port A4: followup-rate diagnostic.

Acceptance bullets pinned here:
  1. followup flag set when next search within window returns disjoint ids
     (reflect_db.record_recall_search → recall_events.followup; and the
     session-side recall.py track_followup state machine)
  2. cost skill shows followup rate (reflect_cost.py --followup; cost
     SKILL.md invokes it)
  3. metrics.jsonl carries the counter (recall.py appends op="recall_search"
     lines with followup true/false; reflect_db bumps
     recall_searches_total / recall_followups_total)
"""

from __future__ import annotations

import json
import sqlite3
import subprocess
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
RECALL = PLUGIN_ROOT / "skills" / "recall" / "scripts" / "recall.py"
COST = SCRIPTS / "reflect_cost.py"
COST_SKILL_MD = PLUGIN_ROOT / "skills" / "cost" / "SKILL.md"
SESSION_START_HOOK = (
    PLUGIN_ROOT / "skills" / "recall" / "hooks" / "session_start_recall.py"
)
sys.path.insert(0, str(SCRIPTS))
sys.path.insert(0, str(RECALL.parent))

import recall as recall_mod  # noqa: E402
import reflect_cost  # noqa: E402
import reflect_db  # noqa: E402


# =========================================================================
# Fixtures
# =========================================================================


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB per test, wired as the module default connection."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


@pytest.fixture
def state(tmp_path, monkeypatch):
    """Isolated REFLECT_STATE_DIR + metrics.jsonl; no ambient session id."""
    state_dir = tmp_path / "state"
    metrics = tmp_path / "metrics.jsonl"
    monkeypatch.setenv("REFLECT_STATE_DIR", str(state_dir))
    monkeypatch.setenv("REFLECT_METRICS_PATH", str(metrics))
    monkeypatch.delenv("CLAUDE_SESSION_ID", raising=False)
    monkeypatch.delenv("RECALL_FOLLOWUP_WINDOW_SECONDS", raising=False)
    return {"state_dir": state_dir, "metrics": metrics}


def _mk_learnings(connection, n: int) -> list[str]:
    return [
        reflect_db.add_learning(f"learning number {i}", conn=connection)
        for i in range(n)
    ]


def _events_for(connection, session_id: str, query: str):
    return connection.execute(
        "SELECT * FROM recall_events WHERE session_id = ? AND query = ?",
        (session_id, query),
    ).fetchall()


def _backdate_recall_events(connection, seconds: int) -> None:
    old = (datetime.now(timezone.utc) - timedelta(seconds=seconds)).isoformat()
    with connection:
        connection.execute("UPDATE recall_events SET created_at = ?", (old,))


def _lrn(lid: str) -> "recall_mod.Learning":
    return recall_mod.Learning(chunk_text=f"body {lid}", frontmatter={"id": lid})


# =========================================================================
# Acceptance 1 — followup flag set when the next search within the window
# returns disjoint ids (DB layer: reflect_db.record_recall_search)
# =========================================================================


def test_followup_flag_set_on_disjoint_within_window(conn):
    a, b, c, d = _mk_learnings(conn, 4)
    first = reflect_db.record_recall_search(
        "how does auth work", [a, b], session_id="s1", conn=conn,
    )
    second = reflect_db.record_recall_search(
        "auth token refresh flow", [c, d], session_id="s1", conn=conn,
    )
    assert first["followup"] is False
    assert second["followup"] is True
    # The flag lands on the SECOND search's recall_events rows.
    rows = _events_for(conn, "s1", "auth token refresh flow")
    assert len(rows) == 2
    assert all(r["followup"] == 1 for r in rows)
    assert all(r["session_id"] == "s1" for r in rows)
    prior_rows = _events_for(conn, "s1", "how does auth work")
    assert all(r["followup"] == 0 for r in prior_rows)


def test_overlapping_result_sets_are_not_followups(conn):
    a, b, c = _mk_learnings(conn, 3)
    reflect_db.record_recall_search("q one", [a, b], session_id="s1", conn=conn)
    res = reflect_db.record_recall_search(
        "q two", [b, c], session_id="s1", conn=conn,
    )
    assert res["followup"] is False  # ANY overlap → first recall was used


def test_same_query_within_window_is_a_retry_not_followup(conn):
    a, b = _mk_learnings(conn, 2)
    reflect_db.record_recall_search("same ask", [a], session_id="s1", conn=conn)
    res = reflect_db.record_recall_search(
        "same ask", [b], session_id="s1", conn=conn,
    )
    assert res["followup"] is False


def test_search_outside_window_is_not_followup(conn):
    a, b = _mk_learnings(conn, 2)
    reflect_db.record_recall_search("q one", [a], session_id="s1", conn=conn)
    _backdate_recall_events(conn, 120)  # default window is 30s
    res = reflect_db.record_recall_search(
        "q two", [b], session_id="s1", conn=conn,
    )
    assert res["followup"] is False


def test_followup_requires_same_session(conn):
    a, b = _mk_learnings(conn, 2)
    reflect_db.record_recall_search("q one", [a], session_id="s1", conn=conn)
    res = reflect_db.record_recall_search(
        "q two", [b], session_id="s2", conn=conn,
    )
    assert res["followup"] is False


def test_window_seconds_param_overrides_default(conn):
    a, b = _mk_learnings(conn, 2)
    reflect_db.record_recall_search("q one", [a], session_id="s1", conn=conn)
    _backdate_recall_events(conn, 120)
    res = reflect_db.record_recall_search(
        "q two", [b], session_id="s1", window_seconds=300, conn=conn,
    )
    assert res["followup"] is True


def test_no_session_id_recorded_but_never_counted(conn):
    a, b = _mk_learnings(conn, 2)
    first = reflect_db.record_recall_search("q one", [a], conn=conn)
    second = reflect_db.record_recall_search("q two", [b], conn=conn)
    assert first["counted"] is False and second["counted"] is False
    assert second["followup"] is False
    # Rows still land (recall telemetry), anchored to no session.
    rows = conn.execute("SELECT * FROM recall_events").fetchall()
    assert len(rows) == 2
    assert all(r["session_id"] == "" for r in rows)
    stats = reflect_db.get_followup_stats(conn=conn)
    assert stats == {"searches": 0, "followups": 0, "rate": 0.0}


def test_empty_result_set_records_nothing(conn):
    res = reflect_db.record_recall_search("empty ask", [], session_id="s1", conn=conn)
    assert res == {"followup": False, "counted": False, "recall_event_ids": []}
    assert conn.execute("SELECT COUNT(*) FROM recall_events").fetchone()[0] == 0


def test_metrics_counters_and_followup_stats(conn):
    a, b, c = _mk_learnings(conn, 3)
    reflect_db.record_recall_search("q one", [a], session_id="s1", conn=conn)
    reflect_db.record_recall_search("q two", [b], session_id="s1", conn=conn)
    # Third search overlaps the second — counted, not a followup.
    reflect_db.record_recall_search("q three", [b, c], session_id="s1", conn=conn)
    stats = reflect_db.get_followup_stats(conn=conn)
    assert stats["searches"] == 3
    assert stats["followups"] == 1
    assert stats["rate"] == pytest.approx(1 / 3)
    assert reflect_db.get_metric("recall_searches_total", conn=conn) == 3
    assert reflect_db.get_metric("recall_followups_total", conn=conn) == 1


def test_migration_adds_followup_columns_to_legacy_table(tmp_path):
    """A pre-A4 recall_events table (no session_id/followup) gains both."""
    db_file = tmp_path / "legacy.db"
    raw = sqlite3.connect(str(db_file))
    raw.executescript(
        """CREATE TABLE recall_events (
               id              TEXT PRIMARY KEY,
               learning_id     TEXT NOT NULL,
               query           TEXT NOT NULL,
               query_hash      TEXT NOT NULL DEFAULT '',
               source_context  TEXT NOT NULL DEFAULT '',
               rank            INTEGER,
               feedback        TEXT NOT NULL DEFAULT '',
               created_at      TEXT NOT NULL
           );"""
    )
    raw.close()
    try:
        connection = reflect_db.init_db(db_file)
        cols = {
            row[1]
            for row in connection.execute(
                "PRAGMA table_info(recall_events)"
            ).fetchall()
        }
        assert {"session_id", "followup"} <= cols
    finally:
        reflect_db.close_all()


def test_db_window_env_override_and_floor(monkeypatch):
    monkeypatch.delenv("RECALL_FOLLOWUP_WINDOW_SECONDS", raising=False)
    assert reflect_db.followup_window_seconds() == 30.0
    monkeypatch.setenv("RECALL_FOLLOWUP_WINDOW_SECONDS", "120")
    assert reflect_db.followup_window_seconds() == 120.0
    monkeypatch.setenv("RECALL_FOLLOWUP_WINDOW_SECONDS", "0")
    assert reflect_db.followup_window_seconds() == 1.0  # floor — never off
    monkeypatch.setenv("RECALL_FOLLOWUP_WINDOW_SECONDS", "junk")
    assert reflect_db.followup_window_seconds() == 30.0


# =========================================================================
# Acceptance 1 — session-side state machine (recall.py track_followup)
# =========================================================================


def test_track_followup_disjoint_within_window(state):
    t0 = 1_000_000.0
    assert recall_mod.track_followup("q1", ["a", "b"], "s", now=t0) is False
    assert recall_mod.track_followup("q2", ["c", "d"], "s", now=t0 + 5) is True


def test_track_followup_overlap_is_false(state):
    t0 = 1_000_000.0
    recall_mod.track_followup("q1", ["a", "b"], "s", now=t0)
    assert recall_mod.track_followup("q2", ["b", "c"], "s", now=t0 + 5) is False


def test_track_followup_same_query_is_retry(state):
    t0 = 1_000_000.0
    recall_mod.track_followup("q1", ["a"], "s", now=t0)
    assert recall_mod.track_followup("q1", ["b"], "s", now=t0 + 5) is False


def test_track_followup_outside_window_is_false(state):
    t0 = 1_000_000.0
    recall_mod.track_followup("q1", ["a"], "s", now=t0)
    assert recall_mod.track_followup("q2", ["b"], "s", now=t0 + 45) is False


def test_track_followup_window_env_tunable(state, monkeypatch):
    monkeypatch.setenv("RECALL_FOLLOWUP_WINDOW_SECONDS", "60")
    t0 = 1_000_000.0
    recall_mod.track_followup("q1", ["a"], "s", now=t0)
    assert recall_mod.track_followup("q2", ["b"], "s", now=t0 + 45) is True


def test_track_followup_sessions_are_independent(state):
    t0 = 1_000_000.0
    recall_mod.track_followup("q1", ["a"], "s1", now=t0)
    assert recall_mod.track_followup("q2", ["b"], "s2", now=t0 + 5) is False


def test_track_followup_corrupt_state_file_is_no_prior(state):
    path = recall_mod._recent_searches_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("{not json", encoding="utf-8")
    assert recall_mod.track_followup("q", ["a"], "s", now=1_000_000.0) is False


def test_stale_sessions_pruned_from_state_file(state):
    t0 = 1_000_000.0
    recall_mod.track_followup("q-old", ["a"], "old-session", now=t0)
    # 2h later another session writes — the hourly-sweep analog prunes.
    recall_mod.track_followup("q-new", ["b"], "new-session", now=t0 + 7200)
    data = json.loads(recall_mod._recent_searches_path().read_text())
    assert "old-session" not in data
    assert "new-session" in data


# =========================================================================
# Acceptance 3 — metrics.jsonl carries the counter
# =========================================================================


def _metric_lines(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line]


def test_metrics_jsonl_carries_the_followup_counter(state):
    recall_mod.record_followup_diagnostic(
        "how does auth work", [_lrn("lrn-1"), _lrn("lrn-2")], "sess-a", True,
    )
    recall_mod.record_followup_diagnostic(
        "auth token refresh", [_lrn("lrn-3")], "sess-a", True,
    )
    lines = _metric_lines(state["metrics"])
    assert len(lines) == 2
    assert all(line["op"] == "recall_search" for line in lines)
    assert lines[0]["followup"] is False
    assert lines[1]["followup"] is True
    assert lines[1]["session_id"] == "sess-a"
    assert lines[1]["result_count"] == 1
    assert lines[1]["window_seconds"] == 30.0
    assert "ts" in lines[1]


def test_no_session_anchor_skips_diagnostic(state):
    recall_mod.record_followup_diagnostic("q", [_lrn("lrn-1")], None, True)
    assert not state["metrics"].exists()


def test_env_session_id_is_a_valid_anchor(state, monkeypatch):
    monkeypatch.setenv("CLAUDE_SESSION_ID", "env-sess")
    recall_mod.record_followup_diagnostic("q", [_lrn("lrn-1")], None, True)
    lines = _metric_lines(state["metrics"])
    assert lines and lines[0]["session_id"] == "env-sess"


def test_disabled_flag_skips_diagnostic(state):
    recall_mod.record_followup_diagnostic("q", [_lrn("lrn-1")], "sess", False)
    assert not state["metrics"].exists()


def test_empty_results_skip_diagnostic(state):
    recall_mod.record_followup_diagnostic("q", [], "sess", True)
    assert not state["metrics"].exists()


def test_cli_exposes_no_followup_flag():
    text = RECALL.read_text(encoding="utf-8")
    assert "--no-followup" in text
    # SessionStart's synthetic boot queries must opt out.
    assert "--no-followup" in SESSION_START_HOOK.read_text(encoding="utf-8")


# =========================================================================
# Acceptance 2 — cost skill shows followup rate
# =========================================================================


def _write_metrics(path: Path, followups: int, total: int) -> None:
    now = datetime.now(timezone.utc).isoformat()
    with path.open("w", encoding="utf-8") as f:
        for i in range(total):
            f.write(
                json.dumps(
                    {
                        "ts": now,
                        "op": "recall_search",
                        "session_id": "s",
                        "followup": i < followups,
                        "window_seconds": 30.0,
                        "result_count": 3,
                        "query": f"q{i}",
                    }
                )
                + "\n"
            )
        # Unrelated engine ops must not count as searches.
        f.write(json.dumps({"ts": now, "op": "search", "harness": "claude"}) + "\n")


def test_followup_stats_aggregation():
    events = [
        {"op": "recall_search", "followup": True, "window_seconds": 30.0},
        {"op": "recall_search", "followup": False, "window_seconds": 30.0},
        {"op": "search"},  # engine op, ignored
    ]
    stats = reflect_cost.followup_stats(events)
    assert stats["searches"] == 2
    assert stats["followups"] == 1
    assert stats["rate"] == pytest.approx(0.5)


def test_cost_cli_followup_report(tmp_path):
    metrics = tmp_path / "metrics.jsonl"
    _write_metrics(metrics, followups=1, total=4)
    proc = subprocess.run(
        [sys.executable, str(COST), "--followup",
         "--metrics-path", str(metrics), "--since", "1d"],
        capture_output=True, text=True, timeout=30, check=False,
    )
    assert proc.returncode == 0
    out = proc.stdout
    assert "followup rate" in out
    assert "searches tracked : 4" in out
    assert "followups        : 1" in out
    assert "25%" in out


def test_cost_cli_followup_json(tmp_path):
    metrics = tmp_path / "metrics.jsonl"
    _write_metrics(metrics, followups=2, total=4)
    proc = subprocess.run(
        [sys.executable, str(COST), "--followup", "--json",
         "--metrics-path", str(metrics), "--since", "1d"],
        capture_output=True, text=True, timeout=30, check=False,
    )
    assert proc.returncode == 0
    stats = json.loads(proc.stdout)
    assert stats["searches"] == 4
    assert stats["followups"] == 2
    assert stats["rate"] == pytest.approx(0.5)


def test_cost_cli_followup_window_filters_old_events(tmp_path):
    metrics = tmp_path / "metrics.jsonl"
    old = (datetime.now(timezone.utc) - timedelta(days=10)).isoformat()
    metrics.write_text(
        json.dumps({"ts": old, "op": "recall_search", "followup": True}) + "\n",
        encoding="utf-8",
    )
    proc = subprocess.run(
        [sys.executable, str(COST), "--followup",
         "--metrics-path", str(metrics), "--since", "1d"],
        capture_output=True, text=True, timeout=30, check=False,
    )
    assert proc.returncode == 0
    assert "No tracked recall searches" in proc.stdout


def test_cost_cli_followup_missing_file(tmp_path):
    proc = subprocess.run(
        [sys.executable, str(COST), "--followup",
         "--metrics-path", str(tmp_path / "absent.jsonl")],
        capture_output=True, text=True, timeout=30, check=False,
    )
    assert proc.returncode == 0
    assert "No tracked recall searches" in proc.stdout


def test_cost_skill_md_surfaces_followup_rate():
    text = COST_SKILL_MD.read_text(encoding="utf-8")
    assert "--followup" in text
    assert "followup rate" in text.lower()


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
