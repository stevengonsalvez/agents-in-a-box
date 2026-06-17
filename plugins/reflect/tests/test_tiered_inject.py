# ABOUTME: Regression tests for port R10 — 3-tier hierarchical inject at
# ABOUTME: SessionStart. Pins: skills (curated) tier beats raw learnings when
# ABOUTME: it has a strong hit; lower tier runs when the skills tier is empty,
# ABOUTME: weak, or stale; everything sits behind the REFLECT_TIERED_INJECT flag.
"""Port R10: 3-tier hierarchical inject (hindsight forced tool-order shape).

Acceptance criteria pinned here:
  1. query covered by a skill injects skill name + 1-line summary,
     not 3 raw learnings
  2. behind config flag for opt-in rollout (off by default)

Plus the design invariants:
  - no/weak skill hit falls through to the learnings tier (recall.py)
  - deleted (stale) skills are pruned before matching, so a stale top
    tier falls through instead of winning
  - skills-tier failures are silent: the hook still exits 0 and degrades
    to the flat learnings inject

The hook is exercised as a subprocess (the way the harness runs it) with
a fully synthetic environment: tmp HOME, tmp reflect.db, tmp skills dir,
and a fake ``uv`` on PATH that stands in for the recall.py learnings
tier — so no test ever touches the real ~/.reflect or knowledge base.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
HOOK = PLUGIN_ROOT / "skills" / "recall" / "hooks" / "session_start_recall.py"
SKILL_INDEX = PLUGIN_ROOT / "scripts" / "skill_index.py"

FAKE_LEARNING = "- prior learning about playwright flake retries [lrn-fake-1]"


# --- Environment scaffolding -------------------------------------------------


def _write_skill(
    base: Path,
    dirname: str,
    *,
    name: str | None = None,
    description: str = "Does a useful thing.",
    tags: list[str] | None = None,
) -> Path:
    """Write a minimal SKILL.md the R20 frontmatter parser understands."""
    skill_dir = base / dirname
    skill_dir.mkdir(parents=True)
    lines = ["---"]
    if name is not None:
        lines.append(f"name: {name}")
    lines.append("description: |")
    lines.extend(f"  {ln}" for ln in description.splitlines())
    if tags:
        lines.append("tags:")
        lines.extend(f"  - {t}" for t in tags)
    lines.extend(["---", "", f"# {dirname}", "body"])
    path = skill_dir / "SKILL.md"
    path.write_text("\n".join(lines), encoding="utf-8")
    return path


def _fake_uv(bin_dir: Path, output: str = FAKE_LEARNING) -> Path:
    """A stand-in ``uv`` that prints canned learnings markdown.

    The hook shells out ``uv run --quiet recall.py <query> ...`` for the
    learnings tier; this fake makes 'the lower tier produced results'
    observable without a knowledge base.
    """
    bin_dir.mkdir(parents=True, exist_ok=True)
    uv = bin_dir / "uv"
    uv.write_text(f"#!/bin/sh\necho '{output}'\n", encoding="utf-8")
    uv.chmod(0o755)
    return uv


@pytest.fixture
def sandbox(tmp_path):
    """Isolated world: project dir, skills dir, state dir, empty PATH dir."""
    (tmp_path / "home").mkdir()
    (tmp_path / "playwright").mkdir()       # CLAUDE_PROJECT_DIR; name = query
    (tmp_path / "skills").mkdir()
    (tmp_path / "emptybin").mkdir()         # PATH without uv/git
    return tmp_path


def _env(sandbox: Path, *, flag: str | None = "1", uv_bin: Path | None = None,
         extra: dict[str, str] | None = None) -> dict[str, str]:
    """Minimal hook environment. Deliberately NOT a copy of os.environ —
    no real uv, git, or ~/.reflect can leak into the test."""
    path = str(uv_bin) if uv_bin else str(sandbox / "emptybin")
    env = {
        "PATH": path,
        "HOME": str(sandbox / "home"),
        "REFLECT_STATE_DIR": str(sandbox / "state"),
        "REFLECT_DB_PATH": str(sandbox / "reflect.db"),
        "REFLECT_SKILLS_DIR": str(sandbox / "skills"),
        "CLAUDE_PROJECT_DIR": str(sandbox / "playwright"),
    }
    if flag is not None:
        env["REFLECT_TIERED_INJECT"] = flag
    if extra:
        env.update(extra)
    return env


def _run_hook(env: dict[str, str]) -> str:
    """Run the hook, assert the silent-fail contract, return the context."""
    result = subprocess.run(
        [sys.executable, str(HOOK)],
        input="{}",
        capture_output=True,
        text=True,
        env=env,
        timeout=30,
    )
    assert result.returncode == 0, (
        f"hook exited non-zero:\nstdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    parsed = json.loads(result.stdout)
    out = parsed["hookSpecificOutput"]
    assert out["hookEventName"] == "SessionStart"
    return out["additionalContext"]


# --- Acceptance 1: skill hit injects skill, not raw learnings ----------------


def test_skill_hit_injects_name_and_summary_not_learnings(sandbox):
    """Query covered by a skill → skill name + 1-line summary injected;
    the raw-learnings tier (available via fake uv) is NOT injected."""
    _write_skill(
        sandbox / "skills",
        "webapp-testing",
        name="webapp-testing",
        description="Drive Playwright browser tests for webapps.",
        tags=["playwright", "browser"],
    )
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(_env(sandbox, uv_bin=uv_dir))

    assert "webapp-testing" in ctx, f"skill name missing from inject: {ctx!r}"
    assert "Drive Playwright browser tests for webapps." in ctx, (
        f"1-line summary missing from inject: {ctx!r}"
    )
    assert "lrn-fake-1" not in ctx, (
        f"raw learnings leaked into a skill-tier inject: {ctx!r}"
    )


def test_skill_tier_block_is_compact(sandbox):
    """The skill tier injects a router block (name + summary), not the
    skill body — and stays inside the SessionStart char budget."""
    _write_skill(
        sandbox / "skills",
        "webapp-testing",
        name="webapp-testing",
        description="Drive Playwright browser tests for webapps.",
        tags=["playwright"],
    )
    ctx = _run_hook(_env(sandbox))
    assert ctx
    assert len(ctx) <= 1500  # SESSION_START_MAX_CHARS
    assert "body" not in ctx  # SKILL.md body never injected


# --- Acceptance 2: behind a config flag (opt-in, off by default) -------------


def test_flag_off_by_default_uses_learnings_tier(sandbox):
    """Without REFLECT_TIERED_INJECT the hook behaves exactly as before:
    flat learnings inject, even when a matching skill is installed."""
    _write_skill(
        sandbox / "skills",
        "webapp-testing",
        name="webapp-testing",
        description="Drive Playwright browser tests for webapps.",
        tags=["playwright"],
    )
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(_env(sandbox, flag=None, uv_bin=uv_dir))

    assert "lrn-fake-1" in ctx, f"learnings tier expected with flag off: {ctx!r}"
    assert "webapp-testing" not in ctx


def test_flag_explicit_zero_disables(sandbox):
    """REFLECT_TIERED_INJECT=0 keeps the skills tier off."""
    _write_skill(
        sandbox / "skills",
        "webapp-testing",
        name="webapp-testing",
        tags=["playwright"],
    )
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)
    ctx = _run_hook(_env(sandbox, flag="0", uv_bin=uv_dir))
    assert "lrn-fake-1" in ctx
    assert "webapp-testing" not in ctx


# --- Tier fall-through: empty / weak / stale top tier -------------------------


def test_no_skill_hit_falls_back_to_learnings(sandbox):
    """Flag on but no skill covers the query → lower tier injects."""
    _write_skill(
        sandbox / "skills",
        "deploy-helper",
        name="deploy-helper",
        description="Roll out helm releases safely.",
        tags=["helm", "kubernetes"],
    )
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(_env(sandbox, uv_bin=uv_dir))

    assert "lrn-fake-1" in ctx, f"expected learnings fallback: {ctx!r}"
    assert "deploy-helper" not in ctx


def test_weak_summary_only_hit_falls_back(sandbox):
    """A summary-only overlap (score 1.0 < default 2.0) is not a 'strong
    hit' — the hook must NOT prefer it over the learnings tier."""
    _write_skill(
        sandbox / "skills",
        "deploy-helper",
        name="deploy-helper",
        description="Mentions playwright once but is about helm rollouts.",
        tags=["helm"],
    )
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(_env(sandbox, uv_bin=uv_dir))

    assert "lrn-fake-1" in ctx
    assert "deploy-helper" not in ctx


def test_min_score_threshold_is_env_tunable(sandbox):
    """REFLECT_SKILL_TIER_MIN_SCORE=1.0 lets the same summary-only hit win."""
    _write_skill(
        sandbox / "skills",
        "deploy-helper",
        name="deploy-helper",
        description="Mentions playwright once but is about helm rollouts.",
        tags=["helm"],
    )
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(
        _env(
            sandbox,
            uv_bin=uv_dir,
            extra={"REFLECT_SKILL_TIER_MIN_SCORE": "1.0"},
        )
    )

    assert "deploy-helper" in ctx
    assert "lrn-fake-1" not in ctx


def test_deleted_skill_is_pruned_then_falls_back(sandbox):
    """Stale top tier: a skill that won the tier, then got uninstalled,
    must be pruned by refresh_if_stale() — the next session falls through
    to the learnings tier instead of injecting a ghost skill."""
    _write_skill(
        sandbox / "skills",
        "webapp-testing",
        name="webapp-testing",
        description="Drive Playwright browser tests for webapps.",
        tags=["playwright"],
    )
    env = _env(sandbox)
    ctx = _run_hook(env)
    assert "webapp-testing" in ctx  # indexed + won the tier

    shutil.rmtree(sandbox / "skills" / "webapp-testing")
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(_env(sandbox, uv_bin=uv_dir))
    assert "webapp-testing" not in ctx, f"ghost skill injected: {ctx!r}"
    assert "lrn-fake-1" in ctx


# --- Silent-fail invariants ----------------------------------------------------


def test_skills_tier_db_failure_degrades_to_learnings(sandbox):
    """An unusable reflect.db path must not break the hook — it exits 0
    and degrades to the flat learnings inject."""
    _write_skill(
        sandbox / "skills",
        "webapp-testing",
        name="webapp-testing",
        tags=["playwright"],
    )
    uv_dir = sandbox / "uvbin"
    _fake_uv(uv_dir)

    ctx = _run_hook(
        _env(
            sandbox,
            uv_bin=uv_dir,
            extra={"REFLECT_DB_PATH": os.devnull + "/nope/reflect.db"},
        )
    )

    assert "lrn-fake-1" in ctx, f"expected learnings fallback on DB error: {ctx!r}"


def test_flag_on_with_nothing_anywhere_emits_empty(sandbox):
    """Flag on, no skills installed, no uv on PATH → empty context, exit 0."""
    ctx = _run_hook(_env(sandbox))
    assert ctx == ""


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
