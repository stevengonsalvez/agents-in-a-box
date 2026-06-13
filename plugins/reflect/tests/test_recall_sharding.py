# ABOUTME: Regression tests for port R15 — per-project sharding in recall.py.
# ABOUTME: Pins the acceptance bullets: shard dir lives under
# ABOUTME: ~/.learnings/shards/<project>/, default scope = current project's
# ABOUTME: shard, --global / RECALL_GLOBAL searches the pooled KB, and an
# ABOUTME: explicit $GLOBAL_LEARNINGS_PATH override always wins.
"""Port R15: per-project sharding.

Each project keeps its OWN nano-graphrag index under
``~/.learnings/shards/<project>/`` (Hindsight bank_id partitioning shape).
recall.py defaults to the CURRENT project's shard; ``--global`` / RECALL_GLOBAL
searches the pooled ``~/.learnings`` KB across all projects.

Acceptance bullets pinned here:
  1. shard dir resolves to ~/.learnings/shards/<project>/
  2. default scope → the current project's shard
  3. --global / RECALL_GLOBAL → the pooled global KB
  4. an explicit pre-set $GLOBAL_LEARNINGS_PATH override always wins (the
     eval/behavioral harness contract — sharding must never clobber it)
  5. unknown current project → fall back to the pooled global KB
  6. shard scope neutralizes the R16 affinity boost; global scope keeps it
"""

from __future__ import annotations

import importlib
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
RECALL_SCRIPTS = PLUGIN_ROOT / "skills" / "recall" / "scripts"
SCRIPTS = PLUGIN_ROOT / "scripts"
sys.path.insert(0, str(RECALL_SCRIPTS))
sys.path.insert(0, str(SCRIPTS))

import recall as recall_mod  # noqa: E402


@pytest.fixture(autouse=True)
def _isolate(monkeypatch, tmp_path):
    """Sandbox the shard tree + clear project memoization for every test."""
    # Point the pooled root at a tmp dir so no test touches ~/.learnings.
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(tmp_path / "learnings"))
    # Clear all R15/R16 scope env so the default path is deterministic.
    for var in ("GLOBAL_LEARNINGS_PATH", "RECALL_GLOBAL", "CLAUDE_PROJECT_DIR"):
        monkeypatch.delenv(var, raising=False)
    # detect_current_project memoizes per process.
    monkeypatch.setattr(recall_mod, "_CURRENT_PROJECT_CACHE", None, raising=False)
    yield
    monkeypatch.setattr(recall_mod, "_CURRENT_PROJECT_CACHE", None, raising=False)


# --- bullet 1: shard dir layout ------------------------------------------

def test_shard_dir_under_shards_subdir(monkeypatch, tmp_path):
    """shard_kb_path('myproj') == <root>/shards/myproj."""
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    shard = recall_mod.shard_kb_path("myproj")
    assert shard == root / "shards" / "myproj"


def test_shard_dir_none_for_empty_project():
    """Empty project id => None (caller falls back to the pooled KB), never a
    'shards//' path."""
    assert recall_mod.shard_kb_path("") is None


# --- bullet 2: default scope = current project's shard -------------------

def test_default_scope_resolves_to_current_project_shard(monkeypatch, tmp_path):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "shotclubhouse")
    kb = recall_mod.resolve_kb_root(scope_global=False)
    assert kb == root / "shards" / "shotclubhouse"


def test_recall_env_points_subprocess_at_shard(monkeypatch, tmp_path):
    """recall_env() returns os.environ + GLOBAL_LEARNINGS_PATH set to the shard
    so the `reflect` subprocess reads that shard's index."""
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj-a")
    env, kb_root = recall_mod.recall_env(scope_global=False)
    assert kb_root == root / "shards" / "proj-a"
    assert env["GLOBAL_LEARNINGS_PATH"] == str(root / "shards" / "proj-a")


# --- bullet 3: --global searches the pooled KB ---------------------------

def test_global_flag_resolves_to_pooled_root(monkeypatch, tmp_path):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj-a")
    kb = recall_mod.resolve_kb_root(scope_global=True)
    assert kb == root  # pooled global, NOT the shard


def test_recall_global_env_resolves_to_pooled_root(monkeypatch, tmp_path):
    """RECALL_GLOBAL=1 is the env analog of --global (read at import time, so
    re-import to pick it up)."""
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setenv("RECALL_GLOBAL", "1")
    mod = importlib.reload(recall_mod)
    try:
        monkeypatch.setattr(mod, "detect_current_project", lambda: "proj-a")
        kb = mod.resolve_kb_root(scope_global=False)
        assert kb == root  # env forces global even without the flag
    finally:
        monkeypatch.delenv("RECALL_GLOBAL", raising=False)
        importlib.reload(mod)


# --- bullet 4: explicit override always wins -----------------------------

def test_explicit_override_wins_over_shard(monkeypatch, tmp_path):
    """A pre-set $GLOBAL_LEARNINGS_PATH (the harness contract) => resolve_kb_root
    returns None: the env is left untouched and every arm reads that path."""
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(tmp_path / "learnings"))
    monkeypatch.setenv("GLOBAL_LEARNINGS_PATH", str(tmp_path / "sandbox-kb"))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj-a")
    assert recall_mod.resolve_kb_root(scope_global=False) is None
    assert recall_mod.resolve_kb_root(scope_global=True) is None
    env, kb_root = recall_mod.recall_env(scope_global=False)
    assert kb_root is None
    # env keeps the caller's override unchanged.
    assert env["GLOBAL_LEARNINGS_PATH"] == str(tmp_path / "sandbox-kb")


# --- bullet 5: unknown project falls back to pooled KB -------------------

def test_unknown_project_falls_back_to_pooled(monkeypatch, tmp_path):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "")
    assert recall_mod.resolve_kb_root(scope_global=False) == root


# --- docs root threading -------------------------------------------------

def test_docs_root_for_shard(monkeypatch, tmp_path):
    """The corpus-scan arms (QMD/temporal) read <shard>/documents so they agree
    with the `reflect` subprocess on which shard is in scope."""
    shard = tmp_path / "learnings" / "shards" / "proj-a"
    assert recall_mod._docs_root_for(shard) == shard / "documents"


def test_docs_root_for_override(monkeypatch, tmp_path):
    """kb_root None => honour the existing $GLOBAL_LEARNINGS_PATH override's
    documents dir (pre-R15 behaviour)."""
    monkeypatch.setenv("GLOBAL_LEARNINGS_PATH", str(tmp_path / "ov"))
    assert recall_mod._docs_root_for(None) == tmp_path / "ov" / "documents"


# --- cache key is shard-specific -----------------------------------------

def test_cache_token_distinguishes_shards(tmp_path):
    """Two different shards must produce DIFFERENT cache mode tokens (else a
    query against project A would serve project B's cached result)."""
    a = recall_mod._cache_scope_token("naive", tmp_path / "shards" / "a")
    b = recall_mod._cache_scope_token("naive", tmp_path / "shards" / "b")
    assert a != b
    # None (override in force) reuses the bare mode — pre-R15 key shape.
    assert recall_mod._cache_scope_token("naive", None) == "naive"


# --- bullet 6: shard scope neutralizes the R16 affinity boost ------------

def test_shard_scope_neutralizes_affinity_boost(monkeypatch, tmp_path):
    """When the corpus is a single-project shard, the R16 project-affinity
    boost is redundant — the rerank must run with current_project="" so a
    learning carrying a DIFFERENT project_id is not penalised relative to the
    detected project. Asserted via the resolved rerank scope, then the boost
    parity it implies.
    """
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj-a")

    # Two otherwise-identical learnings differing only in project_id. Under
    # the shard scope (current_project="") their formula scores must be EQUAL
    # — affinity contributes nothing. Under the global scope (auto-detect,
    # which resolves to proj-a here) the proj-a learning out-scores proj-b.
    same = recall_mod.Learning(
        chunk_text="x", frontmatter={"name": "s", "confidence": "medium",
                                     "project_id": "proj-a"})
    other = recall_mod.Learning(
        chunk_text="x", frontmatter={"name": "o", "confidence": "medium",
                                     "project_id": "proj-b"})

    # shard scope: explicit "" => boost neutral => equal scores
    _, shard_scores = recall_mod.rerank_with_scores(
        [same, other], current_project="")
    assert shard_scores[recall_mod._learning_key(same)] == pytest.approx(
        shard_scores[recall_mod._learning_key(other)]
    )

    # global scope: auto-detect proj-a => same-project hit scores strictly
    # higher (only when the affinity α is live).
    if recall_mod.PROJECT_AFFINITY_ALPHA > 0:
        _, global_scores = recall_mod.rerank_with_scores(
            [same, other], current_project="proj-a")
        assert (
            global_scores[recall_mod._learning_key(same)]
            > global_scores[recall_mod._learning_key(other)]
        )
