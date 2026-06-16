# ABOUTME: Regression tests for port M1 — enforced 3-layer staged recall (index → timeline → hydrate).
# ABOUTME: Hermetic: a tmp GLOBAL_LEARNINGS_PATH KB + a stubbed-out reflect CLI pin every acceptance bullet.
"""Port M1 (claude-mem __IMPORTANT + step-numbered tools) in recall_stages.py.

Acceptance bullets pinned here:
  - reflect_index returns ID-only rows ≤100 tokens/result
  - reflect_timeline accepts an anchor ID or a free-text query and yields
    chronological neighbours bounded by depth_before/depth_after
  - reflect_hydrate(ids[]) returns full learning bodies + entity sidecars
  - reflect_workflow returns a markdown contract that explicitly numbers steps
  - tool descriptions carry literal 'Step 1:'/'Step 2:'/'Step 3:' prefixes
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
STAGES = SCRIPTS / "recall_stages.py"
sys.path.insert(0, str(SCRIPTS))

import recall as recall_mod  # noqa: E402
import recall_stages  # noqa: E402


# --- Fixtures --------------------------------------------------------------

DOCS = [
    # (id, created, title, body, has_sidecar)
    ("lrn-001", "2026-01-01T10:00:00Z", "Tmux socket safety",
     "Never kill the tmux server.\n\n**How to apply:** kill by session name.", True),
    ("lrn-002", "2026-01-05T10:00:00Z", "Pytest caching pitfalls",
     "Pytest cache dir collisions break parallel runs.", False),
    ("lrn-003", "2026-02-01T10:00:00Z", "Zanzibar quota drift",
     "Quota drift in zanzibar replicas needs reconciliation jobs.", True),
    ("lrn-004", "2026-02-10T10:00:00Z", "Zanzibar shard rebalance",
     "Shard rebalance in zanzibar must drain before move.", False),
    ("lrn-005", "2026-03-01T10:00:00Z", "Uv tool installs",
     "uv tool install puts binaries in ~/.local/bin.", False),
    ("lrn-006", "2026-03-15T10:00:00Z", "Graph reindex cost",
     "Reindexing the graph is O(docs); batch it.", False),
]


@pytest.fixture()
def kb(tmp_path, monkeypatch):
    """A tmp learnings repo + reflect CLI stubbed out (forces local paths)."""
    docs_dir = tmp_path / "documents"
    docs_dir.mkdir()
    for doc_id, created, title, body, sidecar in DOCS:
        (docs_dir / f"{doc_id}.md").write_text(
            f"---\nid: {doc_id}\ntitle: \"{title}\"\ncreated: {created}\n"
            f"category: testing\nconfidence: HIGH\n---\n\n{body}\n"
        )
        if sidecar:
            (docs_dir / f"{doc_id}.entities.yaml").write_text(
                f"document_id: {doc_id}\nentities:\n"
                f"  - name: ENTITY_{doc_id.upper().replace('-', '_')}\n"
                f"    type: concept\n    description: \"test entity\"\n"
            )
    monkeypatch.setenv("GLOBAL_LEARNINGS_PATH", str(tmp_path))
    # Hermetic: no engine CLI → reflect_index uses the lexical fallback.
    monkeypatch.setattr(recall_mod, "find_learnings_cli", lambda: None)
    return tmp_path


# --- Step 1: reflect_index ---------------------------------------------------

def test_index_rows_are_id_only_and_under_100_tokens(kb):
    out = reflect_index_rows("zanzibar quota drift")
    assert out, "expected at least one index row"
    for row in out:
        assert set(row) == {"id", "title", "score", "project", "date"}, (
            "index rows must be ID-only — no bodies, no chunks"
        )
        assert recall_stages.recall_mod._est_tokens(json.dumps(row)) <= 100


def reflect_index_rows(query, **kw):
    return recall_stages.reflect_index(query, **kw)["results"]


def test_index_ranks_matching_docs_first(kb):
    rows = reflect_index_rows("zanzibar quota drift")
    assert rows[0]["id"] == "lrn-003"
    assert rows[0]["score"] > 0


def test_index_respects_limit(kb):
    rows = reflect_index_rows("zanzibar", limit=1)
    assert len(rows) == 1


def test_index_row_carries_project_and_date(kb):
    row = reflect_index_rows("zanzibar quota drift")[0]
    assert row["project"] == "testing"
    assert row["date"].startswith("2026-02-01")


def test_index_caps_pathological_titles(kb, tmp_path):
    (tmp_path / "documents" / "lrn-big.md").write_text(
        f"---\nid: lrn-big\ntitle: \"{'wombat ' * 300}\"\ncreated: 2026-04-01T00:00:00Z\n---\n\nwombat body\n"
    )
    row = reflect_index_rows("wombat")[0]
    assert recall_stages.recall_mod._est_tokens(json.dumps(row)) <= 100


def test_index_uses_engine_scores_when_recall_succeeds(kb, monkeypatch):
    lrn = recall_mod.Learning(
        chunk_text="engine chunk about zanzibar",
        frontmatter={"id": "lrn-003", "title": "Zanzibar quota drift",
                     "category": "testing", "created": "2026-02-01T10:00:00Z"},
    )
    fake = recall_mod.RecallResult(
        [lrn], "zanzibar", "naive", scores={"lrn-003": 0.87}
    )
    monkeypatch.setattr(recall_stages.recall_mod, "recall", lambda q, **kw: fake)
    rows = reflect_index_rows("zanzibar")
    assert rows == [{
        "id": "lrn-003", "title": "Zanzibar quota drift", "score": 0.87,
        "project": "testing", "date": "2026-02-01T10:00:00Z",
    }]


# --- Step 2: reflect_timeline -------------------------------------------------

def test_timeline_by_anchor_id_bounded_by_depths(kb):
    out = recall_stages.reflect_timeline(
        anchor_id="lrn-003", depth_before=2, depth_after=1
    )
    assert out["anchor"] == "lrn-003"
    ids = [r["id"] for r in out["results"]]
    assert ids == ["lrn-001", "lrn-002", "lrn-003", "lrn-004"]
    flags = [r["anchor"] for r in out["results"]]
    assert flags == [False, False, True, False]


def test_timeline_results_are_chronological(kb):
    out = recall_stages.reflect_timeline(anchor_id="lrn-003")
    dates = [r["date"] for r in out["results"]]
    assert dates == sorted(dates)


def test_timeline_by_free_text_query(kb):
    out = recall_stages.reflect_timeline(
        query="zanzibar quota drift", depth_before=1, depth_after=1
    )
    assert out["anchor"] == "lrn-003"
    assert [r["id"] for r in out["results"]] == ["lrn-002", "lrn-003", "lrn-004"]


def test_timeline_clamps_at_kb_edges(kb):
    out = recall_stages.reflect_timeline(
        anchor_id="lrn-001", depth_before=5, depth_after=1
    )
    assert [r["id"] for r in out["results"]] == ["lrn-001", "lrn-002"]


def test_timeline_zero_depths_yield_anchor_only(kb):
    out = recall_stages.reflect_timeline(
        anchor_id="lrn-004", depth_before=0, depth_after=0
    )
    assert [r["id"] for r in out["results"]] == ["lrn-004"]


def test_timeline_unknown_anchor_reports_error(kb):
    out = recall_stages.reflect_timeline(anchor_id="lrn-nope")
    assert out["results"] == []
    assert "not found" in out["error"]


# --- Step 3: reflect_hydrate ---------------------------------------------------

def test_hydrate_returns_full_bodies_and_sidecars(kb):
    out = recall_stages.reflect_hydrate(["lrn-001", "lrn-003"])
    assert out["count"] == 2
    one, three = out["results"]
    assert one["found"] and three["found"]
    assert "Never kill the tmux server." in one["body"]
    assert "**How to apply:**" in one["body"], "hydrate must return the FULL body"
    assert one["entities"]["document_id"] == "lrn-001"
    assert one["entities"]["entities"][0]["name"] == "ENTITY_LRN_001"
    assert three["entities"]["document_id"] == "lrn-003"


def test_hydrate_without_sidecar_yields_none_entities(kb):
    out = recall_stages.reflect_hydrate(["lrn-002"])
    rec = out["results"][0]
    assert rec["found"] is True
    assert rec["entities"] is None
    assert "Pytest cache dir collisions" in rec["body"]


def test_hydrate_unknown_id_is_partial_not_fatal(kb):
    out = recall_stages.reflect_hydrate(["lrn-nope", "lrn-005"])
    assert out["results"][0] == {"id": "lrn-nope", "found": False}
    assert out["results"][1]["found"] is True


# --- Workflow bootstrap + step-numbered descriptions ----------------------------

def test_workflow_contract_numbers_the_steps():
    contract = recall_stages.reflect_workflow()
    for marker in ("1.", "2.", "3."):
        assert marker in contract, f"workflow contract must number step {marker}"
    for stage in ("index", "timeline", "hydrate"):
        assert stage in contract.lower()
    assert "token" in contract.lower(), "contract must state the token rationale"
    assert contract.lstrip().startswith("#"), "contract must be markdown"


def test_tool_descriptions_carry_literal_step_prefixes():
    d = recall_stages.TOOL_DESCRIPTIONS
    assert d["reflect_index"].startswith("Step 1:")
    assert d["reflect_timeline"].startswith("Step 2:")
    assert d["reflect_hydrate"].startswith("Step 3:")
    assert "ALWAYS FOLLOW" in d["reflect_workflow"]


# --- CLI surface -----------------------------------------------------------------

def _run_cli(*argv, env=None):
    return subprocess.run(
        [sys.executable, str(STAGES), *argv],
        capture_output=True, text=True, timeout=60, env=env,
    )


def test_cli_workflow_prints_contract():
    r = _run_cli("workflow")
    assert r.returncode == 0
    assert "3-Layer" in r.stdout or "3-layer" in r.stdout.lower()
    assert "hydrate" in r.stdout.lower()


def test_cli_help_surfaces_step_order():
    r = _run_cli("--help")
    assert r.returncode == 0
    assert "Step 1:" in r.stdout
    assert "Step 2:" in r.stdout
    assert "Step 3:" in r.stdout


def test_cli_timeline_requires_anchor_or_query():
    r = _run_cli("timeline")
    assert r.returncode == 2


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
