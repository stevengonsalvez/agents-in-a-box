# ABOUTME: Regression tests for port S5 — belief revision on ingest
# ABOUTME: (CREATE/UPDATE/DELETE). Pins related-learnings recall into the
# ABOUTME: drain slice, the structured-action executor (UPDATE = evidence
# ABOUTME: merge, DELETE = non-destructive retire), the revise CLI, and the
# ABOUTME: drain-prompt + SKILL.md plumbing.
"""Port S5: drain emits structured actions over related learnings.

Acceptance criteria pinned here:
  1. second drain on same correction increments proof_count rather than
     creating a duplicate
  2. DELETE retires a learning marked stale
  3. history snapshot recorded (S6)
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
DRAIN = PLUGIN_ROOT / "hooks" / "reflect-drain-bg.sh"
SKILL = PLUGIN_ROOT / "skills" / "reflect" / "SKILL.md"
CASCADE = SCRIPTS / "reflect_cascade.py"
sys.path.insert(0, str(SCRIPTS))

import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB per test, wired as the module default connection."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


class _Sig:
    def __init__(self, signal: str, source_quote: str = ""):
        self.signal = signal
        self.source_quote = source_quote
        self.line_number = 1


def _learning_count(conn) -> int:
    return conn.execute("SELECT COUNT(*) FROM learnings").fetchone()[0]


def _row(conn, lid):
    return reflect_db.get_learning(lid, conn=conn)


def _write_transcript(path: Path, text: str) -> Path:
    path.write_text(
        json.dumps({"message": {"role": "user", "content": text}}) + "\n"
    )
    return path


# ── related-learnings recall ─────────────────────────────────────────────────

def test_related_recall_matches_title_overlap(conn):
    hit = reflect_db.add_learning("Never use var in TypeScript", conn=conn)
    reflect_db.add_learning("Use uv instead of pip", conn=conn)
    related = reflect_cascade.recall_related_learnings(
        [_Sig("No, never use var in TypeScript files")]
    )
    assert [r["id"] for r in related] == [hit]
    assert related[0]["title"] == "Never use var in TypeScript"
    assert related[0]["proof_count"] == 1


def test_related_recall_matches_on_source_quote_too(conn):
    hit = reflect_db.add_learning("tmux kill-server is forbidden", conn=conn)
    related = reflect_cascade.recall_related_learnings(
        [_Sig("", source_quote="stop — never run tmux kill-server again")]
    )
    assert [r["id"] for r in related] == [hit]


def test_related_recall_excludes_retired(conn):
    lid = reflect_db.add_learning("Never use var in TypeScript", conn=conn)
    reflect_db.update_learning_status(lid, "reverted", revert_reason="stale", conn=conn)
    related = reflect_cascade.recall_related_learnings(
        [_Sig("never use var in TypeScript")]
    )
    assert related == []


def test_related_recall_respects_limit(conn):
    for i in range(reflect_cascade._RELATED_LIMIT + 3):
        reflect_db.add_learning(f"never use var pattern {i}", conn=conn)
    related = reflect_cascade.recall_related_learnings(
        [_Sig("never use var pattern 1")],
    )
    assert len(related) <= reflect_cascade._RELATED_LIMIT


def test_related_recall_empty_signals_is_empty(conn):
    assert reflect_cascade.recall_related_learnings([]) == []


def test_related_recall_fails_open_without_db(monkeypatch):
    """DB unavailable → no candidates, never an exception (silent-fail)."""
    monkeypatch.setattr(
        reflect_db, "get_conn",
        lambda path=None: (_ for _ in ()).throw(RuntimeError("no db")),
    )
    assert reflect_cascade.recall_related_learnings([_Sig("never use var")]) == []


# ── prepare: revision block embedded in the slice ───────────────────────────

def test_prepare_embeds_revision_block(conn, tmp_path):
    lid = reflect_db.add_learning("Never use var in TypeScript", conn=conn)
    transcript = _write_transcript(
        tmp_path / "t.jsonl",
        "No, never use var in TypeScript. The root cause was a missing index.",
    )
    out = tmp_path / "slice.txt"
    prep = reflect_cascade.prepare(transcript, out_path=str(out))
    assert prep.action == "reflect"
    assert prep.related_count >= 1
    body = out.read_text()
    assert "Related existing learnings" in body
    assert lid in body
    assert "PREFER UPDATE OVER CREATE" in body
    # The exact executable command (cascade path + transcript as --source).
    assert "revise --source" in body
    assert str(transcript) in body
    assert str(CASCADE) in body


def test_prepare_without_related_has_no_block(conn, tmp_path):
    transcript = _write_transcript(
        tmp_path / "t.jsonl",
        "No, never use var in TypeScript. The root cause was a missing index.",
    )
    out = tmp_path / "slice.txt"
    prep = reflect_cascade.prepare(transcript, out_path=str(out))
    assert prep.action == "reflect"
    assert prep.related_count == 0
    assert "Related existing learnings" not in out.read_text()


# ── acceptance 1: second drain increments proof_count, no duplicate ─────────

def test_second_drain_update_increments_proof_not_duplicate(conn):
    lid = reflect_db.add_learning(
        "Never use var in TypeScript", source_memory_ids=["transcript-1"], conn=conn,
    )
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid, "reason": "restates the var rule"}],
        source_memory_id="transcript-2",
    )
    assert summary["updated"] == 1 and summary["executed"] == 1
    assert summary["errors"] == []
    assert _learning_count(conn) == 1  # no duplicate sibling created
    row = _row(conn, lid)
    assert row["proof_count"] == 2
    assert json.loads(row["source_memory_ids"]) == ["transcript-1", "transcript-2"]


def test_update_same_source_is_idempotent(conn):
    lid = reflect_db.add_learning("rule", source_memory_ids=["t1"], conn=conn)
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid}], source_memory_id="t1",
    )
    assert summary["updated"] == 0 and summary["skipped"] == 1
    assert summary["errors"] == []
    assert _row(conn, lid)["proof_count"] == 1


def test_update_missing_learning_is_collected_not_fatal(conn):
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": "nope"}], source_memory_id="t1",
    )
    assert summary["updated"] == 0 and summary["skipped"] == 1
    assert any("not found" in e for e in summary["errors"])


# ── acceptance 2: DELETE retires a stale learning ────────────────────────────

def test_delete_retires_stale_learning(conn):
    lid = reflect_db.add_learning("We deploy with fab deploy", conn=conn)
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "DELETE", "target_id": lid,
          "reason": "superseded: deploys moved to GitHub Actions"}],
    )
    assert summary["deleted"] == 1 and summary["errors"] == []
    row = _row(conn, lid)
    assert row["status"] == "reverted"
    assert row["reverted_at"]
    assert "GitHub Actions" in row["revert_reason"]
    # Retired learning never resurfaces as a revision candidate.
    assert reflect_cascade.recall_related_learnings(
        [_Sig("we deploy with fab deploy")]
    ) == []


def test_delete_already_retired_is_idempotent(conn):
    lid = reflect_db.add_learning("stale rule", conn=conn)
    first = reflect_cascade.execute_revision_actions(
        [{"action": "DELETE", "target_id": lid, "reason": "stale"}],
    )
    second = reflect_cascade.execute_revision_actions(
        [{"action": "DELETE", "target_id": lid, "reason": "stale again"}],
    )
    assert first["deleted"] == 1
    assert second["deleted"] == 0 and second["skipped"] == 1
    assert second["errors"] == []
    assert _row(conn, lid)["revert_reason"] == "stale"  # first reason kept


def test_delete_missing_learning_is_collected_not_fatal(conn):
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "DELETE", "target_id": "ghost"}],
    )
    assert summary["deleted"] == 0 and summary["skipped"] == 1
    assert any("not found" in e for e in summary["errors"])


# ── acceptance 3: history snapshot recorded (S6) ─────────────────────────────

def test_update_records_history_snapshot(conn):
    lid = reflect_db.add_learning("rule", conn=conn)
    reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": lid}], source_memory_id="t2",
    )
    history = reflect_db.get_learning_history(lid, conn=conn)
    assert [h["change_type"] for h in history] == ["proof_added"]
    snap = json.loads(history[0]["snapshot_json"])
    assert snap["proof_count"] == 1  # pre-mutation form archived


def test_delete_records_history_snapshot(conn):
    lid = reflect_db.add_learning("stale rule", conn=conn)
    reflect_cascade.execute_revision_actions(
        [{"action": "DELETE", "target_id": lid, "reason": "contradicted"}],
    )
    history = reflect_db.get_learning_history(lid, conn=conn)
    assert [h["change_type"] for h in history] == ["status_change"]
    snap = json.loads(history[0]["snapshot_json"])
    assert snap["status"] == "pending"  # pre-retire form archived


# ── CREATE + malformed actions ───────────────────────────────────────────────

def test_create_action_inserts_with_source_evidence(conn):
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Always pin uv tool versions",
          "category": "Tools", "reason": "no existing match"}],
        source_memory_id="transcript-9",
    )
    assert summary["created"] == 1 and summary["errors"] == []
    rows = conn.execute("SELECT * FROM learnings").fetchall()
    assert len(rows) == 1
    assert rows[0]["title"] == "Always pin uv tool versions"
    assert rows[0]["category"] == "Tools"
    assert rows[0]["proof_count"] == 1
    assert json.loads(rows[0]["source_memory_ids"]) == ["transcript-9"]


def test_malformed_actions_are_collected_not_fatal(conn):
    lid = reflect_db.add_learning("good rule", conn=conn)
    summary = reflect_cascade.execute_revision_actions(
        [
            {"action": "EXPLODE", "target_id": lid},
            {"action": "UPDATE"},                      # missing target_id
            {"action": "CREATE"},                      # missing content
            "not-an-object",
            {"action": "UPDATE", "target_id": lid},    # the one valid action
        ],
        source_memory_id="t2",
    )
    assert summary["updated"] == 1
    assert summary["skipped"] == 4
    assert len(summary["errors"]) == 4
    assert _row(conn, lid)["proof_count"] == 2


# ── revise CLI (subprocess, isolated via REFLECT_DB_PATH) ────────────────────

def test_cli_revise_update_bumps_proof(tmp_path):
    db_file = tmp_path / "cli.db"
    connection = reflect_db.init_db(db_file)
    lid = reflect_db.add_learning("cli rule", conn=connection)
    reflect_db.close_all()

    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(db_file)
    actions = json.dumps([{"action": "UPDATE", "target_id": lid, "reason": "dup"}])
    result = subprocess.run(
        [sys.executable, str(CASCADE), "revise",
         "--source", "transcript-7", "--actions", actions],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    summary = json.loads(result.stdout)
    assert summary["updated"] == 1 and summary["errors"] == []

    connection = reflect_db.init_db(db_file)
    try:
        row = reflect_db.get_learning(lid, conn=connection)
        assert row["proof_count"] == 2
        assert json.loads(row["source_memory_ids"]) == ["transcript-7"]
    finally:
        reflect_db.close_all()


def test_cli_revise_reads_actions_from_file(tmp_path):
    db_file = tmp_path / "cli.db"
    connection = reflect_db.init_db(db_file)
    lid = reflect_db.add_learning("stale cli rule", conn=connection)
    reflect_db.close_all()

    actions_file = tmp_path / "actions.json"
    actions_file.write_text(json.dumps(
        [{"action": "DELETE", "target_id": lid, "reason": "stale"}]
    ))
    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(db_file)
    result = subprocess.run(
        [sys.executable, str(CASCADE), "revise", "--actions", str(actions_file)],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["deleted"] == 1

    connection = reflect_db.init_db(db_file)
    try:
        assert reflect_db.get_learning(lid, conn=connection)["status"] == "reverted"
    finally:
        reflect_db.close_all()


def test_cli_revise_invalid_json_exits_nonzero(tmp_path):
    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(tmp_path / "unused.db")
    result = subprocess.run(
        [sys.executable, str(CASCADE), "revise", "--actions", "{not json"],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 1
    summary = json.loads(result.stdout)
    assert summary["executed"] == 0
    assert any("invalid actions JSON" in e for e in summary["errors"])


# ── plumbing pins: drain prompt + skill doc carry the contract ───────────────

def test_drain_prompt_mentions_belief_revision():
    script = DRAIN.read_text()
    assert "Related existing learnings" in script
    assert "UPDATE over CREATE" in script
    assert "revise" in script


def test_skill_documents_action_contract():
    skill = SKILL.read_text()
    assert "Belief Revision" in skill
    assert "PREFER UPDATE OVER CREATE" in skill
    for token in ("CREATE", "UPDATE", "DELETE", "proof_count", "reverted", "revise"):
        assert token in skill


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
