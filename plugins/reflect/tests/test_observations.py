# ABOUTME: Regression tests for port O1 — consolidated observations layer
# ABOUTME: (persona/conventions aggregate). Pins the observations table +
# ABOUTME: history in reflect.db, the cascade's second-pass observation
# ABOUTME: executor (CREATE/UPDATE/DELETE), the observe CLI, the drain-slice
# ABOUTME: block, and the observation-first retrieval tier for open-domain
# ABOUTME: queries.
"""Port O1: drain emits a second, aggregated observation stream.

Acceptance criteria pinned here:
  1. 50 'team prefers X' corrections collapse into 1 observation with
     proof_count=50
  2. UPDATE adds source_correction_ids without losing history
  3. observation tier surfaces FIRST for open-domain queries
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
SKILL = PLUGIN_ROOT / "skills" / "reflect" / "SKILL.md"
TEMPLATE = PLUGIN_ROOT / "assets" / "observation_template.md"
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


def _observation_count(conn) -> int:
    return conn.execute("SELECT COUNT(*) FROM observations").fetchone()[0]


def _correction_ids(row) -> list[str]:
    return json.loads(row["source_correction_ids"])


def _write_transcript(path: Path, text: str) -> Path:
    path.write_text(
        json.dumps({"message": {"role": "user", "content": text}}) + "\n"
    )
    return path


# ── schema ───────────────────────────────────────────────────────────────────

def test_fresh_db_has_observation_tables(conn):
    tables = {
        r[0]
        for r in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table'"
        ).fetchall()
    }
    assert "observations" in tables
    assert "observation_history" in tables


def test_reinit_recreates_dropped_observation_tables(tmp_path):
    """A pre-O1 DB (no observations tables) gains them on the next init."""
    db_file = tmp_path / "old.db"
    connection = reflect_db.init_db(db_file)
    with connection:
        connection.execute("DROP TABLE observation_history")
        connection.execute("DROP TABLE observations")
    reflect_db.close_all()
    connection = reflect_db.init_db(db_file)
    try:
        oid = reflect_db.add_observation("team prefers X", conn=connection)
        assert reflect_db.get_observation(oid, conn=connection) is not None
    finally:
        reflect_db.close_all()


# ── acceptance 1: 50 corrections collapse into 1 observation, proof 50 ──────

def test_fifty_corrections_collapse_into_one_observation(conn):
    corrections = [
        reflect_db.add_learning(f"Team prefers conventional commits (case {i})", conn=conn)
        for i in range(50)
    ]
    create = reflect_cascade.execute_observation_actions(
        [{"action": "CREATE",
          "content": "Team prefers conventional commit messages",
          "category": "Process",
          "source_correction_ids": [corrections[0]],
          "reason": "first sighting of the convention"}],
    )
    assert create["created"] == 1 and create["errors"] == []
    oid = reflect_db.get_observations(conn=conn)[0]["id"]

    updates = reflect_cascade.execute_observation_actions(
        [{"action": "UPDATE", "target_id": oid,
          "source_correction_ids": [cid], "reason": "more evidence"}
         for cid in corrections[1:]],
    )
    assert updates["updated"] == 49 and updates["errors"] == []

    assert _observation_count(conn) == 1  # ONE aggregate, not 50 siblings
    row = reflect_db.get_observation(oid, conn=conn)
    assert row["proof_count"] == 50
    assert _correction_ids(row) == corrections


def test_create_starts_proof_at_cited_correction_count(conn):
    oid = reflect_db.add_observation(
        "Codebase generally uses dataclasses",
        source_correction_ids=["c1", "c2", "c3"],
        conn=conn,
    )
    row = reflect_db.get_observation(oid, conn=conn)
    assert row["proof_count"] == 3
    assert _correction_ids(row) == ["c1", "c2", "c3"]


def test_create_without_corrections_starts_at_proof_one(conn):
    oid = reflect_db.add_observation("Team prefers tabs", conn=conn)
    assert reflect_db.get_observation(oid, conn=conn)["proof_count"] == 1


# ── acceptance 2: UPDATE adds source_correction_ids without losing history ──

def test_update_appends_ids_and_keeps_history(conn):
    oid = reflect_db.add_observation(
        "Team prefers strict typing", source_correction_ids=["c1"], conn=conn,
    )
    assert reflect_db.add_observation_evidence(oid, ["c2"], conn=conn)

    row = reflect_db.get_observation(oid, conn=conn)
    assert row["proof_count"] == 2
    assert _correction_ids(row) == ["c1", "c2"]

    history = reflect_db.get_observation_history(oid, conn=conn)
    assert [h["change_type"] for h in history] == ["evidence_added"]
    snap = json.loads(history[0]["snapshot_json"])
    assert snap["proof_count"] == 1  # pre-mutation form archived
    assert json.loads(snap["source_correction_ids"]) == ["c1"]


def test_update_same_correction_ids_is_idempotent(conn):
    oid = reflect_db.add_observation(
        "rule", source_correction_ids=["c1"], conn=conn,
    )
    assert not reflect_db.add_observation_evidence(oid, ["c1"], conn=conn)
    row = reflect_db.get_observation(oid, conn=conn)
    assert row["proof_count"] == 1
    assert reflect_db.get_observation_history(oid, conn=conn) == []


def test_update_anonymous_evidence_bumps_proof(conn):
    oid = reflect_db.add_observation("rule", conn=conn)
    assert reflect_db.add_observation_evidence(oid, conn=conn)
    assert reflect_db.get_observation(oid, conn=conn)["proof_count"] == 2


def test_update_content_rewrite_keeps_old_wording_in_history(conn):
    oid = reflect_db.add_observation(
        "Team prefers tabs", source_correction_ids=["c1"], conn=conn,
    )
    assert reflect_db.add_observation_evidence(
        oid, ["c2"], content="Team prefers tabs (except YAML)", conn=conn,
    )
    row = reflect_db.get_observation(oid, conn=conn)
    assert row["content"] == "Team prefers tabs (except YAML)"
    history = reflect_db.get_observation_history(oid, conn=conn)
    snap = json.loads(history[0]["snapshot_json"])
    assert snap["content"] == "Team prefers tabs"  # prior wording survives


def test_update_missing_observation_returns_false(conn):
    assert not reflect_db.add_observation_evidence("ghost", ["c1"], conn=conn)


# ── DELETE: non-destructive retire ───────────────────────────────────────────

def test_delete_retires_non_destructively(conn):
    oid = reflect_db.add_observation("Team prefers fab deploy", conn=conn)
    summary = reflect_cascade.execute_observation_actions(
        [{"action": "DELETE", "target_id": oid,
          "reason": "deploys moved to GitHub Actions"}],
    )
    assert summary["deleted"] == 1 and summary["errors"] == []
    row = reflect_db.get_observation(oid, conn=conn)
    assert row["status"] == "retired"
    assert "GitHub Actions" in row["retired_reason"]
    history = reflect_db.get_observation_history(oid, conn=conn)
    assert [h["change_type"] for h in history] == ["retired"]


def test_delete_already_retired_is_idempotent(conn):
    oid = reflect_db.add_observation("stale convention", conn=conn)
    assert reflect_db.retire_observation(oid, reason="dropped", conn=conn)
    second = reflect_cascade.execute_observation_actions(
        [{"action": "DELETE", "target_id": oid, "reason": "again"}],
    )
    assert second["deleted"] == 0 and second["skipped"] == 1
    assert second["errors"] == []
    assert reflect_db.get_observation(oid, conn=conn)["retired_reason"] == "dropped"


def test_retired_observation_excluded_from_reads(conn):
    keep = reflect_db.add_observation("live convention", conn=conn)
    gone = reflect_db.add_observation("dead convention", conn=conn)
    reflect_db.retire_observation(gone, conn=conn)
    ids = [o["id"] for o in reflect_db.get_observations(conn=conn)]
    assert ids == [keep]
    all_ids = [
        o["id"] for o in reflect_db.get_observations(include_retired=True, conn=conn)
    ]
    assert set(all_ids) == {keep, gone}


def test_scope_filter_includes_global(conn):
    proj = reflect_db.add_observation("project convention", scope="project", conn=conn)
    glob = reflect_db.add_observation("global convention", scope="global", conn=conn)
    ids = {o["id"] for o in reflect_db.get_observations(scope="project", conn=conn)}
    assert ids == {proj, glob}  # global conventions apply everywhere


# ── acceptance 3: observation tier surfaces FIRST for open-domain queries ───

def test_is_open_domain_query_shapes():
    assert reflect_db.is_open_domain_query("what conventions does this codebase use?")
    assert reflect_db.is_open_domain_query("what does this team prefer?")
    assert reflect_db.is_open_domain_query("how do we usually handle migrations")
    assert not reflect_db.is_open_domain_query("how do I fix the playwright retry flake?")
    assert not reflect_db.is_open_domain_query("error: cannot find module reflect_db")
    assert not reflect_db.is_open_domain_query("")
    assert not reflect_db.is_open_domain_query(None)


def test_observation_tier_first_for_open_domain_query(conn):
    reflect_db.add_learning(
        "codebase conventions: use conventional commits", conn=conn,
    )
    reflect_db.add_observation(
        "Team prefers conventional commits across the codebase",
        source_correction_ids=["c1", "c2", "c3"],
        conn=conn,
    )
    results = reflect_cascade.recall_tiered(
        "what conventions does this codebase use?"
    )
    assert results, "open-domain query must surface results"
    assert results[0]["tier"] == "observation"  # the aggregate leads
    assert results[0]["proof_count"] == 3
    tiers = [r["tier"] for r in results]
    assert "learning" in tiers  # raw corrections still present, after
    # every observation entry precedes every learning entry
    assert tiers.index("learning") > max(
        i for i, t in enumerate(tiers) if t == "observation"
    )


def test_closed_domain_query_has_no_observation_tier(conn):
    reflect_db.add_observation("Team prefers tabs", conn=conn)
    reflect_db.add_learning("playwright retry flake fix: bump timeout", conn=conn)
    results = reflect_cascade.recall_tiered(
        "playwright retry flake fix timeout"
    )
    assert all(r["tier"] == "learning" for r in results)


def test_observation_tier_is_proof_ranked(conn):
    weak = reflect_db.add_observation("Team prefers X", conn=conn)
    strong = reflect_db.add_observation(
        "Team prefers Y", source_correction_ids=["a", "b", "c"], conn=conn,
    )
    tier = reflect_db.recall_observation_tier(
        "what does this team prefer?", conn=conn,
    )
    assert [o["id"] for o in tier] == [strong, weak]


def test_recall_tiered_fails_open_without_db(monkeypatch):
    monkeypatch.setattr(
        reflect_db, "get_conn",
        lambda path=None: (_ for _ in ()).throw(RuntimeError("no db")),
    )
    assert reflect_cascade.recall_tiered("what conventions do we use?") == []


# ── executor: malformed actions + revise created_ids handoff ────────────────

def test_malformed_observation_actions_are_collected_not_fatal(conn):
    oid = reflect_db.add_observation("good convention", conn=conn)
    summary = reflect_cascade.execute_observation_actions(
        [
            {"action": "EXPLODE", "target_id": oid},
            {"action": "UPDATE"},                       # missing target_id
            {"action": "UPDATE", "target_id": "ghost"},  # not found
            {"action": "CREATE"},                       # missing content
            "not-an-object",
            {"action": "UPDATE", "target_id": oid,
             "source_correction_ids": ["c9"]},          # the one valid action
        ],
    )
    assert summary["updated"] == 1
    assert summary["skipped"] == 5
    assert len(summary["errors"]) == 5
    assert reflect_db.get_observation(oid, conn=conn)["proof_count"] == 2


def test_revise_summary_carries_created_ids_for_second_pass(conn):
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Always pin uv tool versions",
          "dedup_adjudicated": True, "reason": "new"}],
        source_memory_id="t1",
    )
    assert summary["created"] == 1
    assert len(summary["created_ids"]) == 1
    assert reflect_db.get_learning(summary["created_ids"][0], conn=conn) is not None


# ── prepare: observation block embedded in the slice ─────────────────────────

def test_prepare_embeds_observation_block(conn, tmp_path):
    oid = reflect_db.add_observation(
        "Team prefers strict typing", source_correction_ids=["c1"], conn=conn,
    )
    transcript = _write_transcript(
        tmp_path / "t.jsonl",
        "No, never use var in TypeScript. The root cause was a missing index.",
    )
    out = tmp_path / "slice.txt"
    prep = reflect_cascade.prepare(transcript, out_path=str(out))
    assert prep.action == "reflect"
    assert prep.observation_count == 1
    body = out.read_text()
    assert "Consolidated observations" in body
    assert oid in body
    assert "PREFER UPDATE OVER CREATE" in body
    assert "source_correction_ids" in body
    assert f"python3 {CASCADE} observe --actions" in body


def test_prepare_block_present_even_with_no_observations(conn, tmp_path):
    transcript = _write_transcript(
        tmp_path / "t.jsonl",
        "No, never use var in TypeScript. The root cause was a missing index.",
    )
    out = tmp_path / "slice.txt"
    prep = reflect_cascade.prepare(transcript, out_path=str(out))
    assert prep.action == "reflect"
    assert prep.observation_count == 0
    # The contract still rides the slice so the layer can bootstrap.
    assert "Consolidated observations" in out.read_text()


# ── observe CLI (subprocess, isolated via REFLECT_DB_PATH) ───────────────────

def test_cli_observe_update_bumps_proof(tmp_path):
    db_file = tmp_path / "cli.db"
    connection = reflect_db.init_db(db_file)
    oid = reflect_db.add_observation(
        "cli convention", source_correction_ids=["c1"], conn=connection,
    )
    reflect_db.close_all()

    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(db_file)
    actions = json.dumps(
        [{"action": "UPDATE", "target_id": oid,
          "source_correction_ids": ["c2"], "reason": "more evidence"}]
    )
    result = subprocess.run(
        [sys.executable, str(CASCADE), "observe", "--actions", actions],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    summary = json.loads(result.stdout)
    assert summary["updated"] == 1 and summary["errors"] == []

    connection = reflect_db.init_db(db_file)
    try:
        row = reflect_db.get_observation(oid, conn=connection)
        assert row["proof_count"] == 2
        assert json.loads(row["source_correction_ids"]) == ["c1", "c2"]
    finally:
        reflect_db.close_all()


def test_cli_observe_create_lands_in_scope(tmp_path):
    db_file = tmp_path / "cli.db"
    reflect_db.init_db(db_file)
    reflect_db.close_all()

    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(db_file)
    actions = json.dumps(
        [{"action": "CREATE", "content": "Team prefers conventional commits",
          "source_correction_ids": ["c1", "c2"], "reason": "aggregate"}]
    )
    result = subprocess.run(
        [sys.executable, str(CASCADE), "observe",
         "--scope", "global", "--actions", actions],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["created"] == 1

    connection = reflect_db.init_db(db_file)
    try:
        rows = reflect_db.get_observations(conn=connection)
        assert len(rows) == 1
        assert rows[0]["scope"] == "global"
        assert rows[0]["proof_count"] == 2
    finally:
        reflect_db.close_all()


def test_cli_observe_invalid_json_exits_nonzero(tmp_path):
    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(tmp_path / "unused.db")
    result = subprocess.run(
        [sys.executable, str(CASCADE), "observe", "--actions", "{not json"],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 1
    summary = json.loads(result.stdout)
    assert summary["executed"] == 0
    assert any("invalid actions JSON" in e for e in summary["errors"])


# ── plumbing pins: skill doc + template carry the contract ───────────────────

def test_skill_documents_observation_contract():
    skill = SKILL.read_text()
    assert "Consolidated Observations" in skill
    assert "type: observation" in skill or "`observation`" in skill
    for token in ("observe", "proof_count", "source_correction_ids",
                  "observation_history", "PREFER UPDATE OVER CREATE"):
        assert token in skill


def test_observation_template_shape():
    template = TEMPLATE.read_text()
    assert "type: observation" in template
    assert "proof_count" in template
    assert "source_correction_ids" in template
    assert "statement" in template
    assert "obs-" in template


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
