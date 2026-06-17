# ABOUTME: Regression tests for port SG6 — negative recall as knowledge-gap signal.
# ABOUTME: Pins gap persistence on 0-result recalls, >=2-session repeat surfacing, normalized dedup.
"""SG6 in recall.py + skills/reflect-status/scripts/knowledge_gaps.py.

Acceptance criteria pinned here:
  1. 0-result queries persist — recall.py appends {ts, query, normalized,
     session_id} to $REFLECT_STATE_DIR/knowledge-gaps.jsonl.
  2. Repeat detection >=2 — the aggregator surfaces only gaps seen in >=2
     DISTINCT sessions ("users keep asking about X with no learnings").
  3. Aggregator dedups normalized queries — word-order/stopword variants of
     one ask collapse into a single gap whose sessions are counted together.
"""

from __future__ import annotations

import importlib
import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL = PLUGIN_ROOT / "skills" / "recall" / "scripts" / "recall.py"
GAPS_SCRIPT = (
    PLUGIN_ROOT / "skills" / "reflect-status" / "scripts" / "knowledge_gaps.py"
)
sys.path.insert(0, str(RECALL.parent))
sys.path.insert(0, str(GAPS_SCRIPT.parent))

recall_mod = importlib.import_module("recall")
gaps_mod = importlib.import_module("knowledge_gaps")


# --- helpers ---------------------------------------------------------------

def _read_gaps(state_dir: Path) -> list[dict]:
    p = state_dir / "knowledge-gaps.jsonl"
    if not p.exists():
        return []
    return [json.loads(line) for line in p.read_text().splitlines() if line.strip()]


def _write_gaps(state_dir: Path, entries: list[dict]) -> Path:
    state_dir.mkdir(parents=True, exist_ok=True)
    p = state_dir / "knowledge-gaps.jsonl"
    p.write_text("".join(json.dumps(e) + "\n" for e in entries))
    return p


def _entry(query: str, session: str, ts: str = "2026-06-09T10:00:00") -> dict:
    return {
        "ts": ts,
        "query": query,
        "normalized": recall_mod.normalize_gap_query(query),
        "session_id": session,
    }


# --- normalization (acceptance 3 groundwork) --------------------------------

def test_normalize_is_word_order_insensitive():
    a = recall_mod.normalize_gap_query("tmux kill server")
    b = recall_mod.normalize_gap_query("kill server tmux")
    assert a == b and a, "word-order variants must share one dedup key"


def test_normalize_drops_stopwords_and_case():
    a = recall_mod.normalize_gap_query("How do I use the Redis pool")
    b = recall_mod.normalize_gap_query("redis pool")
    assert a == b == "pool redis"


def test_normalize_vacuous_query_is_empty():
    assert recall_mod.normalize_gap_query("how do I do it") == ""


# --- log_knowledge_gap unit (acceptance 1) -----------------------------------

def test_gap_entry_persists_with_normalized_and_session(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    recall_mod.log_knowledge_gap("istio sidecar injection", session_id="sess-a")
    entries = _read_gaps(tmp_path)
    assert len(entries) == 1
    e = entries[0]
    assert e["query"] == "istio sidecar injection"
    assert e["normalized"] == "injection istio sidecar"
    assert e["session_id"] == "sess-a"
    assert e["ts"]


def test_gap_log_is_append_only(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    recall_mod.log_knowledge_gap("istio sidecar", session_id="s1")
    recall_mod.log_knowledge_gap("istio sidecar", session_id="s2")
    assert len(_read_gaps(tmp_path)) == 2


def test_vacuous_query_never_logged(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    recall_mod.log_knowledge_gap("how do I do it", session_id="s1")
    assert _read_gaps(tmp_path) == []


def test_session_id_falls_back_to_env_then_per_day(monkeypatch):
    monkeypatch.setenv("CLAUDE_SESSION_ID", "env-sid")
    assert recall_mod._gap_session_id(None) == "env-sid"
    assert recall_mod._gap_session_id("explicit") == "explicit"
    monkeypatch.delenv("CLAUDE_SESSION_ID")
    anon = recall_mod._gap_session_id(None)
    assert anon.startswith("unknown-"), "anonymous asks get a per-day pseudo-id"


def test_gap_log_env_kill_switch(tmp_path, monkeypatch):
    monkeypatch.setenv("REFLECT_STATE_DIR", str(tmp_path))
    monkeypatch.setattr(recall_mod, "GAP_LOG_ENABLED", False)
    recall_mod.log_knowledge_gap("istio sidecar", session_id="s1")
    assert _read_gaps(tmp_path) == []


def test_gap_log_silent_on_unwritable_dir(tmp_path, monkeypatch):
    blocker = tmp_path / "file-not-dir"
    blocker.write_text("x")
    monkeypatch.setenv("REFLECT_STATE_DIR", str(blocker / "nested"))
    # mkdir under a file raises OSError — must be swallowed, never raised.
    recall_mod.log_knowledge_gap("istio sidecar", session_id="s1")


# --- recall.py end-to-end via fake CLI (acceptance 1) ------------------------

@pytest.fixture()
def fake_reflect_empty(tmp_path):
    """Fake `reflect` CLI returning ZERO chunks for every search."""
    script = tmp_path / "bin" / "reflect"
    script.parent.mkdir()
    script.write_text(
        "#!/usr/bin/env python3\n"
        "import json\n"
        'print(json.dumps({"context": ""}))\n'
    )
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return script.parent


@pytest.fixture()
def fake_reflect_hit(tmp_path):
    """Fake `reflect` CLI returning one chunk that mentions the query terms."""
    script = tmp_path / "bin" / "reflect"
    script.parent.mkdir()
    script.write_text(
        "#!/usr/bin/env python3\n"
        "import json\n"
        'chunk = "---\\nname: lrn-redis\\nconfidence: high\\n---\\n'
        'redis pool exhaustion fix"\n'
        'print(json.dumps({"context": chunk}))\n'
    )
    script.chmod(script.stat().st_mode | stat.S_IEXEC)
    return script.parent


def _run_recall(bin_dir: Path, state_dir: Path, query: str, *args, env_extra=None):
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        "REFLECT_STATE_DIR": str(state_dir),
        "RECALL_CROSS_ENCODER": "0",
        "RECALL_MMR": "0",
        "RECALL_GRAPH_ARM": "0",
        **(env_extra or {}),
    }
    env.pop("CLAUDE_SESSION_ID", None)
    return subprocess.run(
        [sys.executable, str(RECALL), query, "--format", "json", "--no-cache", *args],
        capture_output=True, text=True, timeout=60, env=env,
    )


def test_zero_result_recall_persists_gap(fake_reflect_empty, tmp_path):
    state = tmp_path / "state"
    r = _run_recall(fake_reflect_empty, state, "redis pool exhaustion",
                    "--session-id", "sess-42")
    assert r.returncode == 0, r.stderr
    entries = _read_gaps(state)
    assert len(entries) == 1
    assert entries[0]["normalized"] == "exhaustion pool redis"
    assert entries[0]["session_id"] == "sess-42"


def test_nonempty_recall_logs_no_gap(fake_reflect_hit, tmp_path):
    state = tmp_path / "state"
    r = _run_recall(fake_reflect_hit, state, "redis pool exhaustion")
    assert r.returncode == 0, r.stderr
    assert json.loads(r.stdout)["count"] >= 1
    assert _read_gaps(state) == []


def test_ood_gated_empty_counts_as_gap(fake_reflect_hit, tmp_path):
    """Nearest-junk-only IS a gap: the KB has nothing about the ask."""
    state = tmp_path / "state"
    r = _run_recall(fake_reflect_hit, state, "istio sidecar injection",
                    "--min-overlap", "0.99", "--session-id", "sess-1")
    assert r.returncode == 0, r.stderr
    assert json.loads(r.stdout)["ood_gated"] is True
    entries = _read_gaps(state)
    assert len(entries) == 1 and entries[0]["session_id"] == "sess-1"


def test_no_gap_log_flag_suppresses(fake_reflect_empty, tmp_path):
    state = tmp_path / "state"
    r = _run_recall(fake_reflect_empty, state, "redis pool exhaustion",
                    "--no-gap-log")
    assert r.returncode == 0, r.stderr
    assert _read_gaps(state) == []


def test_gap_log_env_gate_suppresses(fake_reflect_empty, tmp_path):
    state = tmp_path / "state"
    r = _run_recall(fake_reflect_empty, state, "redis pool exhaustion",
                    env_extra={"RECALL_GAP_LOG": "0"})
    assert r.returncode == 0, r.stderr
    assert _read_gaps(state) == []


def test_cli_missing_is_error_not_gap(tmp_path):
    """Infra failure (no reflect CLI anywhere) must not pollute the gap log."""
    empty_bin = tmp_path / "bin"
    empty_bin.mkdir()
    state = tmp_path / "state"
    env = {
        "PATH": str(empty_bin),  # no reflect, no qmd, no git
        "HOME": str(tmp_path),   # keep legacy ~/.learnings fallback away
        "REFLECT_STATE_DIR": str(state),
    }
    r = subprocess.run(
        [sys.executable, str(RECALL), "redis pool exhaustion", "--no-cache"],
        capture_output=True, text=True, timeout=60, env=env,
    )
    assert r.returncode == 0, r.stderr
    assert _read_gaps(state) == []


def test_session_start_hook_opts_out():
    """The SessionStart hook's query is synthetic — it must pass --no-gap-log."""
    hook = PLUGIN_ROOT / "skills" / "recall" / "hooks" / "session_start_recall.py"
    assert '"--no-gap-log"' in hook.read_text()


def test_user_prompt_hook_forwards_session_id():
    """The UserPromptSubmit hook is the genuine-ask path — it must forward
    the session id so cross-session repeat detection works."""
    hook = (
        PLUGIN_ROOT / "skills" / "recall" / "hooks" / "user_prompt_submit_recall.py"
    )
    text = hook.read_text()
    assert '"--session-id"' in text
    assert "query_recall(prompt, session_id)" in text


# --- aggregator: repeat detection (acceptance 2) ------------------------------

def test_two_distinct_sessions_surfaces(tmp_path):
    entries = [
        _entry("istio sidecar injection", "s1", "2026-06-08T10:00:00"),
        _entry("istio sidecar injection", "s2", "2026-06-09T10:00:00"),
    ]
    gaps = gaps_mod.repeat_gaps(gaps_mod.aggregate(entries))
    assert len(gaps) == 1
    assert gaps[0].session_count == 2
    assert gaps[0].asks == 2
    assert gaps[0].query == "istio sidecar injection"


def test_single_session_does_not_surface(tmp_path):
    entries = [
        _entry("istio sidecar injection", "s1"),
        _entry("istio sidecar injection", "s1"),  # same session asked twice
    ]
    gaps = gaps_mod.repeat_gaps(gaps_mod.aggregate(entries))
    assert gaps == [], "repeats within ONE session are not a cross-session gap"


def test_min_sessions_threshold_is_tunable():
    entries = [_entry("bun workspace hoisting", "s1")]
    assert gaps_mod.repeat_gaps(gaps_mod.aggregate(entries), min_sessions=1)


# --- aggregator: normalized dedup (acceptance 3) ------------------------------

def test_word_order_variants_dedup_into_one_gap():
    entries = [
        _entry("tmux kill server", "s1", "2026-06-08T10:00:00"),
        _entry("kill server in tmux", "s2", "2026-06-09T10:00:00"),
    ]
    gaps = gaps_mod.aggregate(entries)
    assert len(gaps) == 1, "variants of one ask must collapse to one gap"
    assert gaps[0].session_count == 2
    # Surfaces as a repeat precisely BECAUSE the variants were dedup'd.
    assert gaps_mod.repeat_gaps(gaps)


def test_distinct_queries_stay_distinct():
    entries = [
        _entry("tmux kill server", "s1"),
        _entry("redis pool exhaustion", "s2"),
    ]
    assert len(gaps_mod.aggregate(entries)) == 2


def test_aggregate_orders_hottest_first():
    entries = [
        _entry("bun workspace hoisting", "s1"),
        _entry("istio sidecar injection", "s1", "2026-06-07T10:00:00"),
        _entry("istio sidecar injection", "s2", "2026-06-08T10:00:00"),
        _entry("istio sidecar injection", "s3", "2026-06-09T10:00:00"),
    ]
    gaps = gaps_mod.aggregate(entries)
    assert gaps[0].normalized == recall_mod.normalize_gap_query("istio sidecar injection")
    assert gaps[0].session_count == 3
    assert gaps[0].last_ts == "2026-06-09T10:00:00"
    assert gaps[0].first_ts == "2026-06-07T10:00:00"


def test_malformed_lines_are_skipped(tmp_path):
    state = tmp_path / "state"
    p = _write_gaps(state, [_entry("istio sidecar", "s1")])
    with p.open("a") as f:
        f.write("{torn line\n\n[]\n")
    entries = gaps_mod.load_entries(p)
    assert len(entries) == 1


def test_missing_log_is_empty_report(tmp_path):
    assert gaps_mod.load_entries(tmp_path / "absent.jsonl") == []


# --- aggregator CLI surface ---------------------------------------------------

def _run_gaps(state_dir: Path, *args):
    env = {**os.environ, "REFLECT_STATE_DIR": str(state_dir)}
    return subprocess.run(
        [sys.executable, str(GAPS_SCRIPT), *args],
        capture_output=True, text=True, timeout=30, env=env,
    )


def test_cli_markdown_surfaces_repeat_gap(tmp_path):
    state = tmp_path / "state"
    _write_gaps(state, [
        _entry("istio sidecar injection", "s1", "2026-06-08T10:00:00"),
        _entry("istio sidecar injection", "s2", "2026-06-09T10:00:00"),
        _entry("bun workspace hoisting", "s1"),  # one session — stays hidden
    ])
    r = _run_gaps(state)
    assert r.returncode == 0, r.stderr
    assert "users keep asking about" in r.stdout
    assert "istio sidecar injection" in r.stdout
    assert "2 sessions" in r.stdout
    assert "bun workspace hoisting" not in r.stdout


def test_cli_json_shape(tmp_path):
    state = tmp_path / "state"
    _write_gaps(state, [
        _entry("istio sidecar injection", "s1"),
        _entry("istio sidecar injection", "s2"),
    ])
    r = _run_gaps(state, "--format", "json")
    assert r.returncode == 0, r.stderr
    payload = json.loads(r.stdout)
    assert payload["total_gaps"] == 1
    assert payload["repeat_gaps"][0]["sessions"] == 2
    assert payload["repeat_gaps"][0]["normalized"] == "injection istio sidecar"


def test_cli_empty_state_exits_zero(tmp_path):
    r = _run_gaps(tmp_path / "nowhere")
    assert r.returncode == 0, r.stderr
    assert "No repeat knowledge gaps" in r.stdout


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
