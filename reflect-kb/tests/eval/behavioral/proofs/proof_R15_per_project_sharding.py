# ABOUTME: Behavioral proof for R15 — per-project sharding. Two projects each
# ABOUTME: keep their own index under ~/.learnings/shards/<project>/; default
# ABOUTME: recall sees ONLY the current project's shard, --global sees the union.
"""R15 per-project sharding proof.

Invariant: with two separate per-project shards seeded under
``<root>/shards/<project>/``, a default recall scoped to project A returns A's
learning and NOT B's (clean isolation, less cross-project noise), while the
SAME query run with ``--global`` surfaces B's learning too (cross-project
union). Shard layout + the --global flag fully determine inclusion/exclusion;
no LLM participates in the assertion.

This proof deliberately does NOT use the shared `behavioral_kb` fixture: that
fixture pins $GLOBAL_LEARNINGS_PATH to one KB, which (by R15's explicit-override
precedence) bypasses shard resolution entirely. Instead it builds two real
shard KBs under a sandboxed RECALL_LEARNINGS_ROOT and drives recall.py via
CLAUDE_PROJECT_DIR (current-project detection) with $GLOBAL_LEARNINGS_PATH
UNSET — exactly the runtime shape SessionStart hits in production.

If sharding were absent (one pooled KB), the default scope-A recall would
return B's learning too and the isolation assertion would FAIL; if --global
were ignored, the global recall would miss B and the union assertion would
FAIL.

PORT: R15
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

# Reuse the conftest's doc renderer so seeds index identically to every other
# proof. conftest.py sits one dir up (tests/eval/behavioral/).
import sys

_CONFTEST_DIR = Path(__file__).resolve().parents[1]
if str(_CONFTEST_DIR) not in sys.path:
    sys.path.insert(0, str(_CONFTEST_DIR))
from conftest import RECALL_PY, _doc_md  # noqa: E402

# Both learnings answer the SAME query (shared topic terms) so neither is
# excluded by the OOD gate — only the SHARD decides which one is visible.
SHARED_QUERY = "redis connection pool exhaustion under load"

PROJECT_A = "alpha-service"
PROJECT_B = "beta-service"

SEED_A = dict(
    name="r15-alpha-redis-pool",
    title="Alpha service: cap the redis connection pool to avoid exhaustion",
    category="database",
    tags=["redis", "connection-pool", "alpha"],
    confidence="high",
    created="2026-03-01",
    key_insight="Bound the redis connection pool size so load spikes can't exhaust it.",
    body="Under load the alpha service opened unbounded redis connections and exhausted "
         "the pool; capping max connections and reusing them fixed the redis exhaustion.",
)

SEED_B = dict(
    name="r15-beta-redis-pool",
    title="Beta service: redis pool exhaustion traced to leaked connections",
    category="database",
    tags=["redis", "connection-pool", "beta"],
    confidence="high",
    created="2026-03-02",
    key_insight="Close redis connections on the error path so the pool isn't exhausted under load.",
    body="The beta service leaked redis connections on its error path, exhausting the connection "
         "pool under load; closing them in a finally block resolved the redis exhaustion.",
)


def _base_env(root: Path) -> dict:
    """os.environ + the real HF caches + the full-stack venv on PATH, with the
    shard root sandboxed and any inherited $GLOBAL_LEARNINGS_PATH stripped."""
    env = dict(os.environ)
    env.pop("GLOBAL_LEARNINGS_PATH", None)  # let shard resolution run
    env["RECALL_LEARNINGS_ROOT"] = str(root)
    # A6 added a branch dimension under shards/<project>/branches/<branch>/.
    # This proof seeds the R15 project-level shard (shards/<project>/), which A6
    # defines as TRUNK parity (empty branch). Pin RECALL_BRANCH="" so recall.py's
    # branch detection resolves to that project shard rather than the runner's
    # current worktree branch (which would point at a non-existent sub-shard).
    env["RECALL_BRANCH"] = ""
    env.setdefault("HF_HOME", str(Path.home() / ".cache" / "huggingface"))
    env.setdefault(
        "SENTENCE_TRANSFORMERS_HOME",
        str(Path.home() / ".cache" / "torch" / "sentence_transformers"),
    )
    bin_dir = os.environ.get("RECALL_EVAL_BIN_DIR")
    if bin_dir:
        env["PATH"] = f"{bin_dir}:{env.get('PATH', '')}"
    return env


def _seed_shard(shard_dir: Path, learnings: list[dict], base_env: dict) -> None:
    """Build one shard KB at ``shard_dir`` (reflect init + write docs + reindex).

    The engine resolves its KB from $GLOBAL_LEARNINGS_PATH, so we set it to the
    SHARD dir for the duration of seeding only — recall.py later runs with it
    UNSET so its own shard resolution is exercised.
    """
    env = dict(base_env)
    env["GLOBAL_LEARNINGS_PATH"] = str(shard_dir)
    shard_dir.mkdir(parents=True, exist_ok=True)
    r = subprocess.run(["reflect", "init"], capture_output=True, text=True, env=env)
    assert r.returncode == 0, f"reflect init failed for {shard_dir}: {r.stderr[-600:]}"
    docs = shard_dir / "documents"
    docs.mkdir(exist_ok=True)
    for d in learnings:
        (docs / f"{d['name']}.md").write_text(_doc_md(d))
    r = subprocess.run(
        ["reflect", "reindex", "--force"],
        capture_output=True, text=True, env=env, timeout=1800,
    )
    assert r.returncode == 0, f"reflect reindex failed for {shard_dir}: {r.stderr[-800:]}"


def _recall(base_env: dict, project_dir: Path, *, scope_global: bool) -> dict:
    """Run recall.py the way SessionStart does, scoped to ``project_dir`` via
    CLAUDE_PROJECT_DIR. $GLOBAL_LEARNINGS_PATH stays UNSET so the shard path
    (or --global pooled path) is what resolves."""
    env = dict(base_env)
    env["CLAUDE_PROJECT_DIR"] = str(project_dir)
    cmd = [
        "python3", str(RECALL_PY), SHARED_QUERY,
        "--limit", "5", "--format", "json", "--no-cache",
        "--min-overlap", "0.0",
    ]
    if scope_global:
        cmd.append("--global")
    r = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=300)
    assert r.returncode == 0, f"recall.py exited {r.returncode}\nSTDERR:\n{r.stderr[-1200:]}"
    return json.loads(r.stdout or "{}")


@pytest.mark.skipif(
    not shutil.which("reflect", path=(os.environ.get("RECALL_EVAL_BIN_DIR", "") + ":" + os.environ.get("PATH", ""))),
    reason="full-stack `reflect` not resolvable; set RECALL_EVAL_BIN_DIR",
)
def test_R15_per_project_sharding(tmp_path):
    root = tmp_path / "learnings"          # sandboxed ~/.learnings analog
    pooled = root                          # --global searches the pooled root
    shard_a = root / "shards" / PROJECT_A
    shard_b = root / "shards" / PROJECT_B

    base_env = _base_env(root)
    if not shutil.which("reflect", path=base_env["PATH"]):
        pytest.skip("`reflect` CLI not resolvable in the proof env")

    # Project A's CLAUDE_PROJECT_DIR basename must normalize to PROJECT_A so
    # detect_current_project() maps cwd → the right shard.
    proj_a_dir = tmp_path / "work" / PROJECT_A
    proj_b_dir = tmp_path / "work" / PROJECT_B
    proj_a_dir.mkdir(parents=True)
    proj_b_dir.mkdir(parents=True)

    # ---- Seed two ISOLATED shards: A knows only SEED_A, B only SEED_B ----
    _seed_shard(shard_a, [SEED_A], base_env)
    _seed_shard(shard_b, [SEED_B], base_env)
    # Also pool BOTH learnings into the global root so --global has a union to
    # find (production keeps the pooled KB alongside the shards).
    _seed_shard(pooled, [SEED_A, SEED_B], base_env)

    # ---- Default scope from project A: sees A, NOT B (isolation) ----
    scoped = _recall(base_env, proj_a_dir, scope_global=False)
    scoped_ids = [r.get("id") for r in scoped.get("results", [])]
    assert SEED_A["name"] in scoped_ids, (
        f"expected project-A shard to serve its own learning {SEED_A['name']!r}, "
        f"got {scoped_ids}"
    )
    assert SEED_B["name"] not in scoped_ids, (
        f"per-project isolation broken: project-A recall surfaced project-B's "
        f"learning {SEED_B['name']!r} (got {scoped_ids}) — the shard is not isolating."
    )

    # ---- --global from project A: surfaces B's learning too (union) ----
    pooled_res = _recall(base_env, proj_a_dir, scope_global=True)
    pooled_ids = [r.get("id") for r in pooled_res.get("results", [])]
    assert SEED_B["name"] in pooled_ids, (
        f"--global must search across shards, but project-B's learning "
        f"{SEED_B['name']!r} did not appear (got {pooled_ids})."
    )
