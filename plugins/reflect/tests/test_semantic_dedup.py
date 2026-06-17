# ABOUTME: Regression tests for port C1 — per-ingest semantic-dedup
# ABOUTME: adjudication. Pins the cosine probe on the revise CREATE path
# ABOUTME: (merge call observed at/above threshold, UPDATE path used instead
# ABOUTME: of CREATE on merge, threshold config-tunable via env + TOML),
# ABOUTME: the keep verdict, fail-open degradation, and the CLI surface.
"""Port C1: per-ingest semantic-dedup adjudication (Hindsight consolidator).

Acceptance criteria pinned here:
  1. per-ingest merge call observed when threshold met — a CREATE whose text
     is >= threshold cosine to an existing learning is held with a focused
     'merge?' adjudication instead of landing as a duplicate row
  2. UPDATE path used instead of CREATE on merge — answering the adjudication
     with the UPDATE it names folds the evidence into the existing learning
  3. threshold config-tunable — REFLECT_DEDUP_THRESHOLD env var and the
     [cascade].dedup_threshold TOML key both move the floor; >= 1.0 disables
"""

from __future__ import annotations

import json
import math
import os
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "scripts"
CASCADE = SCRIPTS / "reflect_cascade.py"
sys.path.insert(0, str(SCRIPTS))

import reflect_cascade  # noqa: E402
import reflect_db  # noqa: E402

# Fake `reflect` CLI: answers `embed` with unit vectors identical to the
# query, so every candidate sits at cosine 1.0 — deterministically above any
# enabled threshold without loading a real embedding model.
_FAKE_REFLECT = """#!/usr/bin/env python3
import json, sys
if len(sys.argv) < 2 or sys.argv[1] != "embed":
    sys.exit(2)
payload = json.loads(sys.stdin.read() or "{}")
cands = payload.get("candidates") or []
print(json.dumps({
    "available": True, "model": "fake",
    "query_embedding": [1.0, 0.0],
    "embeddings": {c["id"]: [1.0, 0.0] for c in cands},
}))
"""


@pytest.fixture(autouse=True)
def pinned_threshold(monkeypatch):
    """Pin the threshold to the documented default so a developer's local
    ~/.reflect/reflect.toml override can never flip a test outcome."""
    monkeypatch.setenv("REFLECT_DEDUP_THRESHOLD", "0.97")


@pytest.fixture
def conn(tmp_path, monkeypatch):
    """Fresh isolated DB per test, wired as the module default connection."""
    db_file = tmp_path / "reflect.db"
    connection = reflect_db.init_db(db_file)
    monkeypatch.setattr(reflect_db, "get_conn", lambda path=None: connection)
    yield connection
    reflect_db.close_all()


def _vec_for(similarity: float) -> list[float]:
    """A unit vector at exactly *similarity* cosine to the query [1, 0]."""
    return [similarity, math.sqrt(max(0.0, 1.0 - similarity * similarity))]


def _patch_probe(monkeypatch, similarity: float) -> None:
    """Route the embed subprocess to synthetic vectors at a fixed cosine."""
    monkeypatch.setattr(reflect_cascade, "_find_reflect_cli",
                        lambda: "/fake/reflect")
    monkeypatch.setattr(
        reflect_cascade, "_fetch_dedup_embeddings",
        lambda cli, text, cands, timeout=0: (
            [1.0, 0.0], {c["id"]: _vec_for(similarity) for c in cands}
        ),
    )


def _forbid_probe(monkeypatch) -> None:
    """Any embed attempt is a test failure — pins paths that must skip it."""
    def _boom(*args, **kwargs):
        raise AssertionError("dedup embed probe must not run on this path")
    monkeypatch.setattr(reflect_cascade, "_fetch_dedup_embeddings", _boom)


def _learning_count(conn) -> int:
    return conn.execute("SELECT COUNT(*) FROM learnings").fetchone()[0]


# ── acceptance 1: merge call observed when threshold met ────────────────────

def test_merge_call_observed_when_threshold_met(conn, monkeypatch):
    lid = reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    _patch_probe(monkeypatch, similarity=1.0)
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Pin uv tool versions, always"}],
        source_memory_id="t1",
    )
    assert summary["needs_adjudication"] == 1
    assert summary["created"] == 0 and summary["errors"] == []
    assert _learning_count(conn) == 1  # the near-duplicate row never landed
    adj = summary["adjudications"][0]
    assert adj["existing_id"] == lid
    assert adj["existing_title"] == "Always pin uv tool versions"
    assert adj["new_text"] == "Pin uv tool versions, always"
    assert adj["similarity"] >= 0.97
    assert adj["threshold"] == pytest.approx(0.97)
    # The focused 1-by-1 merge question carries both verdict paths.
    assert adj["question"].startswith("merge?")
    assert lid in adj["question"]
    assert "dedup_adjudicated" in adj["question"]


def test_below_threshold_creates_normally(conn, monkeypatch):
    reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    _patch_probe(monkeypatch, similarity=0.5)
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Use ast-grep for code search"}],
    )
    assert summary["created"] == 1
    assert summary["needs_adjudication"] == 0
    assert summary["adjudications"] == []
    assert _learning_count(conn) == 2


def test_retired_learnings_are_not_dedup_candidates(conn, monkeypatch):
    lid = reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    reflect_db.update_learning_status(lid, "reverted",
                                      revert_reason="stale", conn=conn)
    _forbid_probe(monkeypatch)  # empty live corpus -> probe never starts
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Always pin uv tool versions"}],
    )
    assert summary["created"] == 1
    assert summary["needs_adjudication"] == 0


# ── acceptance 2: UPDATE path used instead of CREATE on merge ───────────────

def test_update_path_used_instead_of_create_on_merge(conn, monkeypatch):
    lid = reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    _patch_probe(monkeypatch, similarity=1.0)

    # Step 1: the CREATE is held with the merge question.
    held = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Pin uv tool versions, always"}],
        source_memory_id="transcript-1",
    )
    assert held["needs_adjudication"] == 1 and held["created"] == 0

    # Step 2: the drain answers "merge" with the UPDATE the question names.
    merged = reflect_cascade.execute_revision_actions(
        [{"action": "UPDATE", "target_id": held["adjudications"][0]["existing_id"],
          "reason": "same rule, different wording"}],
        source_memory_id="transcript-1",
    )
    assert merged["updated"] == 1 and merged["errors"] == []
    assert _learning_count(conn) == 1  # merged, not duplicated
    row = reflect_db.get_learning(lid, conn=conn)
    assert row["proof_count"] == 2  # the new evidence folded into the twin
    assert "transcript-1" in json.loads(row["source_memory_ids"])


def test_keep_verdict_creates_with_adjudicated_flag(conn, monkeypatch):
    reflect_db.add_learning("Cache TTL is 5 minutes", conn=conn)
    _forbid_probe(monkeypatch)  # the keep verdict must not re-probe
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Cache TTL is 15 minutes",
          "dedup_adjudicated": True}],
    )
    assert summary["created"] == 1
    assert summary["needs_adjudication"] == 0
    assert _learning_count(conn) == 2  # genuinely distinct detail kept


# ── acceptance 3: threshold config-tunable ──────────────────────────────────

def test_threshold_tunable_via_env(conn, monkeypatch):
    reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    _patch_probe(monkeypatch, similarity=0.6)

    monkeypatch.setenv("REFLECT_DEDUP_THRESHOLD", "0.5")
    held = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Pin tool versions"}],
    )
    assert held["needs_adjudication"] == 1 and held["created"] == 0
    assert held["adjudications"][0]["threshold"] == pytest.approx(0.5)

    monkeypatch.setenv("REFLECT_DEDUP_THRESHOLD", "0.9")
    created = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Pin tool versions"}],
    )
    assert created["created"] == 1 and created["needs_adjudication"] == 0


def test_threshold_at_or_above_one_disables_probe(conn, monkeypatch):
    reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    _forbid_probe(monkeypatch)
    monkeypatch.setenv("REFLECT_DEDUP_THRESHOLD", "1.0")
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Always pin uv tool versions"}],
    )
    assert summary["created"] == 1 and summary["needs_adjudication"] == 0


def test_threshold_falls_back_to_config_layer(monkeypatch):
    import reflect_config
    monkeypatch.delenv("REFLECT_DEDUP_THRESHOLD", raising=False)
    monkeypatch.setattr(reflect_config, "get_config",
                        lambda: {"cascade": {"dedup_threshold": 0.5}})
    assert reflect_cascade.dedup_threshold() == pytest.approx(0.5)


def test_threshold_default_and_junk_values(monkeypatch):
    import reflect_config
    monkeypatch.delenv("REFLECT_DEDUP_THRESHOLD", raising=False)
    monkeypatch.setattr(reflect_config, "get_config", lambda: {})
    assert reflect_cascade.dedup_threshold() == pytest.approx(0.97)
    monkeypatch.setenv("REFLECT_DEDUP_THRESHOLD", "not-a-number")
    assert reflect_cascade.dedup_threshold() == pytest.approx(0.97)


def test_plugin_toml_documents_the_threshold():
    text = (PLUGIN_ROOT / "reflect.toml").read_text(encoding="utf-8")
    assert "[cascade]" in text
    assert "dedup_threshold = 0.97" in text


# ── fail-open: the probe is a guard, never a blocker ────────────────────────

def test_fail_open_when_cli_missing(conn, monkeypatch):
    reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    monkeypatch.setattr(reflect_cascade, "_find_reflect_cli", lambda: None)
    _forbid_probe(monkeypatch)  # no CLI -> embed must never be attempted
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Always pin uv tool versions"}],
    )
    assert summary["created"] == 1 and summary["errors"] == []


def test_fail_open_when_embeddings_unavailable(conn, monkeypatch):
    reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    monkeypatch.setattr(reflect_cascade, "_find_reflect_cli",
                        lambda: "/fake/reflect")
    monkeypatch.setattr(reflect_cascade, "_fetch_dedup_embeddings",
                        lambda *a, **kw: None)  # slim build / timeout / junk
    summary = reflect_cascade.execute_revision_actions(
        [{"action": "CREATE", "content": "Always pin uv tool versions"}],
    )
    assert summary["created"] == 1 and summary["errors"] == []


# ── subprocess contract: `reflect embed` payload in, vectors out ────────────

@pytest.fixture
def fake_reflect(tmp_path):
    """An executable fake `reflect` honouring the embed I/O contract."""
    bin_dir = tmp_path / "fakebin"
    bin_dir.mkdir()
    script = bin_dir / "reflect"
    script.write_text(_FAKE_REFLECT, encoding="utf-8")
    script.chmod(0o755)
    return script


def test_probe_via_real_subprocess_contract(conn, monkeypatch, fake_reflect):
    lid = reflect_db.add_learning("Always pin uv tool versions", conn=conn)
    monkeypatch.setattr(reflect_cascade, "_find_reflect_cli",
                        lambda: str(fake_reflect))
    twin = reflect_cascade.find_semantic_twin("Pin uv tool versions, always")
    assert twin is not None
    assert twin["id"] == lid
    assert twin["similarity"] == pytest.approx(1.0)


def test_cli_revise_holds_near_dup_then_update_merges(tmp_path, fake_reflect):
    db_file = tmp_path / "cli.db"
    connection = reflect_db.init_db(db_file)
    lid = reflect_db.add_learning("Always pin uv tool versions",
                                  conn=connection)
    reflect_db.close_all()

    env = dict(os.environ)
    env["REFLECT_DB_PATH"] = str(db_file)
    env["REFLECT_DEDUP_THRESHOLD"] = "0.97"
    env["PATH"] = f"{fake_reflect.parent}{os.pathsep}{env.get('PATH', '')}"

    # Step 1: the near-duplicate CREATE is held with the merge question.
    create = json.dumps([{"action": "CREATE",
                          "content": "Pin uv tool versions, always"}])
    result = subprocess.run(
        [sys.executable, str(CASCADE), "revise",
         "--source", "transcript-3", "--actions", create],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    summary = json.loads(result.stdout)
    assert summary["needs_adjudication"] == 1 and summary["created"] == 0
    assert summary["adjudications"][0]["existing_id"] == lid
    assert summary["adjudications"][0]["question"].startswith("merge?")

    # Step 2: the merge verdict lands as an UPDATE — no duplicate row.
    update = json.dumps([{"action": "UPDATE", "target_id": lid,
                          "reason": "same rule"}])
    result = subprocess.run(
        [sys.executable, str(CASCADE), "revise",
         "--source", "transcript-3", "--actions", update],
        env=env, capture_output=True, text=True, timeout=60,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["updated"] == 1

    connection = reflect_db.init_db(db_file)
    try:
        assert connection.execute(
            "SELECT COUNT(*) FROM learnings").fetchone()[0] == 1
        row = reflect_db.get_learning(lid, conn=connection)
        assert row["proof_count"] == 2
    finally:
        reflect_db.close_all()


# ── prompt plumbing: the drain is forewarned about the final step ───────────

def test_revision_block_explains_adjudication_step():
    block = reflect_cascade._build_revision_block([], "/tmp/t.jsonl")
    assert "adjudications" in block
    assert '"dedup_adjudicated": true' in block
    assert "merge?" in block


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
