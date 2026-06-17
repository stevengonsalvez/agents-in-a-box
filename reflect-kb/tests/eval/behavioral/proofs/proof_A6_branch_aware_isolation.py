# ABOUTME: Behavioral proof for A6 — branch-aware capture & isolation. Two
# ABOUTME: worktrees (branches) of ONE project each keep their own sub-shard
# ABOUTME: under shards/<project>/branches/<branch>/; default recall in
# ABOUTME: worktree A sees ONLY A's learning, --all-branches sees the union.
"""A6 branch-aware capture & isolation proof.

Invariant: with two worktrees of the SAME project — branch ``feat/auth`` and
branch ``feat/payment`` — each branch's learnings live in its own sub-shard
(``<root>/shards/<project>/branches/<branch>/``). A default recall run from
worktree A (``RECALL_BRANCH=feat/auth``) returns A's learning and NOT B's
(clean per-worktree isolation — the symptom A6 fixes is "working on feat/A and
getting injected learnings from feat/B"), while the SAME query run with
``--all-branches`` surfaces B's learning too (cross-branch union pooled at the
project-level shard). Shard layout + the current branch + the --all-branches
flag fully determine inclusion/exclusion; no LLM participates in the assertion.

Like the R15 proof, this deliberately does NOT use the shared `behavioral_kb`
fixture: that fixture pins $GLOBAL_LEARNINGS_PATH to one KB, which (by the
explicit-override precedence) bypasses shard resolution entirely. Instead it
builds real branch sub-shards under a sandboxed RECALL_LEARNINGS_ROOT and
drives recall.py via CLAUDE_PROJECT_DIR (project detection) + RECALL_BRANCH
(branch detection) with $GLOBAL_LEARNINGS_PATH UNSET — exactly the runtime
shape the SessionStart hook hits inside a worktree.

Falsifiability: if branch-awareness were absent (A6 reverted to plain R15, one
shard per project), BOTH branches' learnings would pool into the single project
shard, so the default scope-A recall would return B's learning and the
isolation assertion would FAIL. If --all-branches were ignored (stuck on the
branch sub-shard), the union recall would miss B and the union assertion would
FAIL.

PORT: A6
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

# Reuse the conftest's doc renderer so seeds index identically to every other
# proof. conftest.py sits one dir up (tests/eval/behavioral/).
_CONFTEST_DIR = Path(__file__).resolve().parents[1]
if str(_CONFTEST_DIR) not in sys.path:
    sys.path.insert(0, str(_CONFTEST_DIR))
from conftest import RECALL_PY, _doc_md  # noqa: E402

# Both learnings answer the SAME query (shared topic terms) so neither is
# excluded by the OOD gate — only the BRANCH shard decides which is visible.
SHARED_QUERY = "auth token refresh race condition under concurrent requests"

PROJECT = "checkout-service"
# Raw git branch names — recall.py sanitizes feat/auth -> feat__auth for the
# on-disk shard dir; the proof seeds the sanitized dirs to match.
BRANCH_A_RAW = "feat/auth"
BRANCH_B_RAW = "feat/payment"
BRANCH_A_DIR = "feat__auth"
BRANCH_B_DIR = "feat__payment"

SEED_A = dict(
    name="a6-auth-token-refresh",
    title="feat/auth worktree: serialize the auth token refresh to kill the race",
    category="auth",
    tags=["auth", "token-refresh", "concurrency"],
    confidence="high",
    created="2026-03-01",
    key_insight="Guard the auth token refresh with a single-flight lock so concurrent "
                "requests don't each refresh and race.",
    body="On the feat/auth worktree concurrent requests each triggered an auth token "
         "refresh, racing to overwrite the token; a single-flight lock around the refresh "
         "removed the race.",
)

SEED_B = dict(
    name="a6-payment-idempotency",
    title="feat/payment worktree: idempotency key stops double-charge on retry",
    category="payments",
    tags=["payment", "idempotency", "concurrency"],
    confidence="high",
    created="2026-03-02",
    key_insight="Attach an idempotency key to each payment so a concurrent retry can't "
                "double-charge the customer.",
    body="On the feat/payment worktree a concurrent retry of an in-flight payment created a "
         "duplicate charge; an idempotency key keyed on the order id made the retry a no-op.",
)


def _base_env(root: Path) -> dict:
    """os.environ + the real HF caches + the full-stack venv on PATH, with the
    shard root sandboxed and any inherited scope env stripped."""
    env = dict(os.environ)
    env.pop("GLOBAL_LEARNINGS_PATH", None)  # let shard resolution run
    env.pop("RECALL_GLOBAL", None)
    env.pop("RECALL_ALL_BRANCHES", None)
    env.pop("RECALL_BRANCH", None)
    env["RECALL_LEARNINGS_ROOT"] = str(root)
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


def _recall(
    base_env: dict, project_dir: Path, branch_raw: str, *, all_branches: bool
) -> dict:
    """Run recall.py the way the SessionStart hook does INSIDE a worktree:
    scoped to ``project_dir`` (CLAUDE_PROJECT_DIR) on branch ``branch_raw``
    (RECALL_BRANCH — the env the hook now pins). $GLOBAL_LEARNINGS_PATH stays
    UNSET so the branch sub-shard (or the project shard under --all-branches)
    is what resolves."""
    env = dict(base_env)
    env["CLAUDE_PROJECT_DIR"] = str(project_dir)
    env["RECALL_BRANCH"] = branch_raw
    cmd = [
        "python3", str(RECALL_PY), SHARED_QUERY,
        "--limit", "5", "--format", "json", "--no-cache",
        "--min-overlap", "0.0",
    ]
    if all_branches:
        cmd.append("--all-branches")
    r = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=300)
    assert r.returncode == 0, f"recall.py exited {r.returncode}\nSTDERR:\n{r.stderr[-1200:]}"
    return json.loads(r.stdout or "{}")


@pytest.mark.skipif(
    not shutil.which("reflect", path=(os.environ.get("RECALL_EVAL_BIN_DIR", "") + ":" + os.environ.get("PATH", ""))),
    reason="full-stack `reflect` not resolvable; set RECALL_EVAL_BIN_DIR",
)
def test_A6_branch_aware_isolation(tmp_path):
    root = tmp_path / "learnings"                       # sandboxed ~/.learnings
    proj_shard = root / "shards" / PROJECT              # all-branches pooled scope
    branch_a = proj_shard / "branches" / BRANCH_A_DIR
    branch_b = proj_shard / "branches" / BRANCH_B_DIR

    base_env = _base_env(root)
    if not shutil.which("reflect", path=base_env["PATH"]):
        pytest.skip("`reflect` CLI not resolvable in the proof env")

    # One project dir, shared by both worktrees — only the branch differs (the
    # real worktree layout: same repo, two checkouts, two branches). The
    # basename must normalize to PROJECT so detect_current_project() maps to
    # the right shard root.
    proj_dir = tmp_path / "work" / PROJECT
    proj_dir.mkdir(parents=True)

    # ---- Seed two ISOLATED branch sub-shards: A knows only SEED_A, B only B.
    _seed_shard(branch_a, [SEED_A], base_env)
    _seed_shard(branch_b, [SEED_B], base_env)
    # The project-level shard pools BOTH (the --all-branches union scope; in
    # production the capture path writes a branch's learning into both its
    # branch sub-shard and the project roll-up).
    _seed_shard(proj_shard, [SEED_A, SEED_B], base_env)

    # ---- Default scope from worktree A (feat/auth): sees A, NOT B. ----
    scoped = _recall(base_env, proj_dir, BRANCH_A_RAW, all_branches=False)
    scoped_ids = [r.get("id") for r in scoped.get("results", [])]
    assert SEED_A["name"] in scoped_ids, (
        f"expected the feat/auth worktree's sub-shard to serve its own learning "
        f"{SEED_A['name']!r}, got {scoped_ids}"
    )
    assert SEED_B["name"] not in scoped_ids, (
        f"branch isolation broken: the feat/auth worktree surfaced feat/payment's "
        f"learning {SEED_B['name']!r} (got {scoped_ids}) — the branch sub-shard is "
        f"not isolating, which is exactly the cross-worktree pollution A6 fixes."
    )

    # ---- Symmetry: worktree B (feat/payment) sees B, NOT A. ----
    scoped_b = _recall(base_env, proj_dir, BRANCH_B_RAW, all_branches=False)
    scoped_b_ids = [r.get("id") for r in scoped_b.get("results", [])]
    assert SEED_B["name"] in scoped_b_ids, (
        f"expected the feat/payment worktree to serve its own learning "
        f"{SEED_B['name']!r}, got {scoped_b_ids}"
    )
    assert SEED_A["name"] not in scoped_b_ids, (
        f"branch isolation broken: the feat/payment worktree surfaced feat/auth's "
        f"learning {SEED_A['name']!r} (got {scoped_b_ids})."
    )

    # ---- --all-branches from worktree A: surfaces B's learning too (union). ----
    pooled_res = _recall(base_env, proj_dir, BRANCH_A_RAW, all_branches=True)
    pooled_ids = [r.get("id") for r in pooled_res.get("results", [])]
    assert SEED_B["name"] in pooled_ids, (
        f"--all-branches must widen to the project shard and pool every branch, "
        f"but feat/payment's learning {SEED_B['name']!r} did not appear "
        f"(got {pooled_ids})."
    )
