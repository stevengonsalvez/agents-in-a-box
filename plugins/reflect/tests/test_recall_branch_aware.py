# ABOUTME: Regression tests for port A6 — branch-aware capture & isolation.
# ABOUTME: Pins the acceptance bullets: worktrees A and B get separate shard
# ABOUTME: dirs, default recall scope is the current branch only, and the
# ABOUTME: --all-branches flag (or RECALL_ALL_BRANCHES) widens to the
# ABOUTME: project-level shard pooling every branch.
"""Port A6: branch-aware capture & isolation.

Extends R15 per-project sharding with a branch dimension:
``~/.learnings/shards/<project>/branches/<branch>/``. Two worktrees of the
same repo (agents-in-a-box LITERALLY runs ``/.agents-in-a-box/worktrees/...``)
no longer collapse into one bucket — each branch keeps its own sub-shard so
recall in worktree A never serves worktree B's learnings.

Acceptance bullets pinned here:
  1. worktrees A and B get separate shard dirs
  2. recall in worktree A returns only its learnings by default (current
     branch scope)
  3. --all-branches / RECALL_ALL_BRANCHES widens scope to the project shard
And the layering invariants A6 must preserve from R15:
  4. trunk (main/master) and a detached HEAD use the project-level shard, so
     the no-worktree case is byte-identical to R15
  5. branch names with slashes/whitespace are sanitized to one flat dir
  6. an explicit pre-set $GLOBAL_LEARNINGS_PATH override still always wins
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
    """Sandbox the shard tree + clear project/branch memoization per test."""
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(tmp_path / "learnings"))
    for var in (
        "GLOBAL_LEARNINGS_PATH", "RECALL_GLOBAL", "RECALL_ALL_BRANCHES",
        "RECALL_BRANCH", "CLAUDE_PROJECT_DIR",
    ):
        monkeypatch.delenv(var, raising=False)
    monkeypatch.setattr(recall_mod, "_CURRENT_PROJECT_CACHE", None, raising=False)
    monkeypatch.setattr(recall_mod, "_CURRENT_BRANCH_CACHE", None, raising=False)
    yield
    monkeypatch.setattr(recall_mod, "_CURRENT_PROJECT_CACHE", None, raising=False)
    monkeypatch.setattr(recall_mod, "_CURRENT_BRANCH_CACHE", None, raising=False)


# --- bullet 1: worktrees A and B get separate shard dirs -----------------

def test_branch_shard_under_branches_subdir(monkeypatch, tmp_path):
    """shard_kb_path('proj', 'feat__a') lives under <proj>/branches/feat__a."""
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    shard = recall_mod.shard_kb_path("proj", "feat__a")
    assert shard == root / "shards" / "proj" / "branches" / "feat__a"


def test_two_branches_get_distinct_dirs(monkeypatch, tmp_path):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    a = recall_mod.shard_kb_path("proj", "feat__auth")
    b = recall_mod.shard_kb_path("proj", "feat__payment")
    assert a != b
    assert a == root / "shards" / "proj" / "branches" / "feat__auth"
    assert b == root / "shards" / "proj" / "branches" / "feat__payment"


# --- bullet 2: default scope = current branch only -----------------------

def test_default_scope_resolves_to_current_branch_shard(monkeypatch, tmp_path):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj")
    monkeypatch.setattr(recall_mod, "detect_current_branch", lambda: "feat__auth")
    kb = recall_mod.resolve_kb_root(scope_global=False)
    assert kb == root / "shards" / "proj" / "branches" / "feat__auth"


def test_recall_env_points_subprocess_at_branch_shard(monkeypatch, tmp_path):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj")
    monkeypatch.setattr(recall_mod, "detect_current_branch", lambda: "feat__b")
    env, kb_root = recall_mod.recall_env(scope_global=False)
    expected = root / "shards" / "proj" / "branches" / "feat__b"
    assert kb_root == expected
    assert env["GLOBAL_LEARNINGS_PATH"] == str(expected)


def test_worktree_a_and_b_resolve_to_different_kb_roots(monkeypatch, tmp_path):
    """The crux: same project, different branch => DIFFERENT KB root, so a
    recall in worktree A can never read worktree B's index."""
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj")

    monkeypatch.setattr(recall_mod, "detect_current_branch", lambda: "feat__a")
    kb_a = recall_mod.resolve_kb_root(scope_global=False)

    monkeypatch.setattr(recall_mod, "detect_current_branch", lambda: "feat__b")
    kb_b = recall_mod.resolve_kb_root(scope_global=False)

    assert kb_a != kb_b


# --- bullet 3: --all-branches widens scope -------------------------------

def test_all_branches_widens_to_project_shard(monkeypatch, tmp_path):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj")
    monkeypatch.setattr(recall_mod, "detect_current_branch", lambda: "feat__a")
    # default (current branch) vs all-branches (project level)
    narrow = recall_mod.resolve_kb_root(scope_global=False)
    wide = recall_mod.resolve_kb_root(scope_global=False, all_branches=True)
    assert narrow == root / "shards" / "proj" / "branches" / "feat__a"
    assert wide == root / "shards" / "proj"  # project shard, no /branches/


def test_recall_all_branches_env_widens_scope(monkeypatch, tmp_path):
    """RECALL_ALL_BRANCHES=1 is the env analog of --all-branches (read at
    import time, so re-import to pick it up)."""
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setenv("RECALL_ALL_BRANCHES", "1")
    mod = importlib.reload(recall_mod)
    try:
        monkeypatch.setattr(mod, "detect_current_project", lambda: "proj")
        monkeypatch.setattr(mod, "detect_current_branch", lambda: "feat__a")
        kb = mod.resolve_kb_root(scope_global=False)
        assert kb == root / "shards" / "proj"  # widened even without the flag
    finally:
        monkeypatch.delenv("RECALL_ALL_BRANCHES", raising=False)
        importlib.reload(mod)


# --- bullet 4: trunk/detached use the project shard (R15 parity) ---------

@pytest.mark.parametrize("trunk", ["main", "master", "", "HEAD"])
def test_trunk_and_detached_collapse_to_project_shard(monkeypatch, tmp_path, trunk):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj")
    monkeypatch.setenv("RECALL_BRANCH", trunk)
    monkeypatch.setattr(recall_mod, "_CURRENT_BRANCH_CACHE", None, raising=False)
    kb = recall_mod.resolve_kb_root(scope_global=False)
    # No /branches/ — byte-identical to R15's project-level shard.
    assert kb == root / "shards" / "proj"


# --- bullet 5: branch-name sanitization ----------------------------------

@pytest.mark.parametrize(
    "raw,expected",
    [
        ("feat/auth", "feat__auth"),
        ("feat/sub/thing", "feat__sub__thing"),
        ("Feat/Auth", "feat__auth"),  # lowercased
        ("hotfix\\win", "hotfix__win"),
        ("with space", "with__space"),
        ("main", ""),  # trunk
        ("master", ""),  # trunk
        ("HEAD", ""),  # detached
        ("", ""),
        (None, ""),
    ],
)
def test_sanitize_branch(raw, expected):
    assert recall_mod._sanitize_branch(raw) == expected


def test_slash_branch_is_one_flat_dir(monkeypatch, tmp_path):
    """A feat/auth branch maps to ONE dir (branches/feat__auth), never a
    nested branches/feat/auth that could collide with another branch."""
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj")
    monkeypatch.setenv("RECALL_BRANCH", "feat/auth")
    monkeypatch.setattr(recall_mod, "_CURRENT_BRANCH_CACHE", None, raising=False)
    kb = recall_mod.resolve_kb_root(scope_global=False)
    assert kb == root / "shards" / "proj" / "branches" / "feat__auth"


# --- detection via RECALL_BRANCH -----------------------------------------

def test_detect_current_branch_honours_recall_branch_env(monkeypatch):
    monkeypatch.setenv("RECALL_BRANCH", "feat/x")
    monkeypatch.setattr(recall_mod, "_CURRENT_BRANCH_CACHE", None, raising=False)
    assert recall_mod.detect_current_branch() == "feat__x"


# --- bullet 6: explicit override still wins ------------------------------

def test_explicit_override_wins_over_branch_shard(monkeypatch, tmp_path):
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(tmp_path / "learnings"))
    monkeypatch.setenv("GLOBAL_LEARNINGS_PATH", str(tmp_path / "sandbox-kb"))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj")
    monkeypatch.setattr(recall_mod, "detect_current_branch", lambda: "feat__a")
    assert recall_mod.resolve_kb_root(scope_global=False) is None
    env, kb_root = recall_mod.recall_env(scope_global=False)
    assert kb_root is None
    assert env["GLOBAL_LEARNINGS_PATH"] == str(tmp_path / "sandbox-kb")


# --- global scope still pools every project, ignoring branch -------------

def test_global_scope_ignores_branch(monkeypatch, tmp_path):
    root = tmp_path / "learnings"
    monkeypatch.setenv("RECALL_LEARNINGS_ROOT", str(root))
    monkeypatch.setattr(recall_mod, "detect_current_project", lambda: "proj")
    monkeypatch.setattr(recall_mod, "detect_current_branch", lambda: "feat__a")
    assert recall_mod.resolve_kb_root(scope_global=True) == root


# --- cache key segments branches -----------------------------------------

def test_cache_token_distinguishes_branches(tmp_path):
    """Two branches of one project must produce DIFFERENT cache tokens (else a
    recall on branch A would serve branch B's cached result)."""
    base = tmp_path / "shards" / "proj" / "branches"
    a = recall_mod._cache_scope_token("naive", base / "feat__a")
    b = recall_mod._cache_scope_token("naive", base / "feat__b")
    assert a != b
