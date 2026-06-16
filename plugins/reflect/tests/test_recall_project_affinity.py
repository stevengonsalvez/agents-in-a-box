# ABOUTME: Regression tests for port R16 — project-affinity multiplicative
# ABOUTME: boost in recall.py rerank. Pins the soft-affinity contract: same-
# ABOUTME: project hits get the bounded 1 + α/2 boost, cross-project hits are
# ABOUTME: EXACTLY unchanged, α=0 disables, and current_project="" (the future
# ABOUTME: R15 shard-scoped path) neutralizes the boost entirely.
"""Port R16: project-affinity multiplicative boost.

combined = CE × confidence × recency × tags × proof × project_affinity,
where project_affinity = bounded_boost(project_norm, α) with norm 1.0 on a
same-project match and the neutral 0.5 otherwise. Default α=0.2 → matched
hits get +10%; non-matches multiply by exactly 1.0 (soft affinity, never
hard isolation — cross-project gems still surface, just down-ranked).

Acceptance bullets pinned here:
  1. same-project hits rank above otherwise-identical cross-project hits
  2. α=0 disables the boost (cross-project rankings unchanged)
  3. interacts cleanly with R15 — current_project="" (the shard-scoped
     path's hook) neutralizes affinity so it only applies on global scope
"""

from __future__ import annotations

import importlib
import subprocess
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
from recall import (  # noqa: E402
    PROJECT_AFFINITY_ALPHA,
    Learning,
    _normalize_project,
    bounded_boost,
    detect_current_project,
    project_norm,
    rerank,
    rerank_with_scores,
)


def _lrn(name: str, project: str | None = None, **fm_extra) -> Learning:
    fm: dict = {"name": name, "confidence": "medium"}
    if project is not None:
        fm["project_id"] = project
    fm.update(fm_extra)
    return Learning(chunk_text=f"learning body for {name}", frontmatter=fm)


@pytest.fixture(autouse=True)
def _reset_project_cache():
    """detect_current_project memoizes per process — isolate every test."""
    recall_mod._CURRENT_PROJECT_CACHE = None
    yield
    recall_mod._CURRENT_PROJECT_CACHE = None


# ---------- the boost shape: bounded, one-sided, neutral on non-match ----------

def test_default_alpha_gives_ten_percent_match_boost():
    assert PROJECT_AFFINITY_ALPHA == 0.2
    match = bounded_boost(project_norm("my-app", "my-app"), PROJECT_AFFINITY_ALPHA)
    assert match == pytest.approx(1.1)


def test_cross_project_multiplier_is_exactly_one():
    """Soft affinity: non-matching hits are unchanged, never penalised."""
    for current, hit in [
        ("my-app", "other-app"),   # genuine cross-project
        ("my-app", ""),            # learning has no project
        ("", "other-app"),         # current project unknown
        ("", ""),                  # neither side known
    ]:
        boost = bounded_boost(project_norm(current, hit), PROJECT_AFFINITY_ALPHA)
        assert boost == pytest.approx(1.0), (current, hit)


def test_project_boost_stays_within_declared_range():
    lo, hi = 1.0, 1.0 + PROJECT_AFFINITY_ALPHA / 2
    for current, hit in [("a", "a"), ("a", "b"), ("a", ""), ("", "")]:
        boost = bounded_boost(project_norm(current, hit), PROJECT_AFFINITY_ALPHA)
        assert lo <= boost <= hi


# ---------- acceptance 1: same-project ranks above identical cross-project ----------

def test_same_project_hit_ranks_above_identical_cross_project_hit():
    cross = _lrn("cross", project="other-app")
    same = _lrn("same", project="my-app")
    ordered = rerank([cross, same], current_project="my-app")
    assert [lrn.id for lrn in ordered] == ["same", "cross"]


def test_same_project_boost_is_soft_not_hard_isolation():
    """A clearly better cross-project hit still outranks a same-project one —
    the +10% affinity nudge cannot overcome a HIGH-vs-LOW confidence gap."""
    gem = _lrn("gem", project="other-app", confidence="high")
    weak = _lrn("weak", project="my-app", confidence="low")
    ordered = rerank([weak, gem], current_project="my-app")
    assert [lrn.id for lrn in ordered] == ["gem", "weak"]


def test_affinity_matches_project_key_fallback():
    """`project` frontmatter (no explicit project_id) still matches."""
    cross = _lrn("cross")
    cross.frontmatter["project"] = "other-app"
    same = _lrn("same")
    same.frontmatter["project"] = "my-app"
    ordered = rerank([cross, same], current_project="my-app")
    assert [lrn.id for lrn in ordered] == ["same", "cross"]


def test_scores_reflect_exact_boost_ratio():
    cross = _lrn("cross", project="other-app")
    same = _lrn("same", project="my-app")
    _, scores = rerank_with_scores([cross, same], current_project="my-app")
    assert scores["same"] / scores["cross"] == pytest.approx(
        1.0 + PROJECT_AFFINITY_ALPHA / 2
    )


# ---------- acceptance 2: α=0 disables the boost ----------

def test_alpha_zero_disables_boost(monkeypatch):
    monkeypatch.setenv("RECALL_PROJECT_ALPHA", "0")
    importlib.reload(recall_mod)
    try:
        assert recall_mod.PROJECT_AFFINITY_ALPHA == 0.0
        cross = _lrn("cross", project="other-app")
        same = _lrn("same", project="my-app")
        _, scores = recall_mod.rerank_with_scores(
            [cross, same], current_project="my-app"
        )
        assert scores["same"] == pytest.approx(scores["cross"])
        # Stable order preserved — cross was listed first and nothing reranks it.
        ordered = recall_mod.rerank(
            [_lrn("cross", project="other-app"), _lrn("same", project="my-app")],
            current_project="my-app",
        )
        assert [lrn.id for lrn in ordered] == ["cross", "same"]
    finally:
        monkeypatch.undo()
        importlib.reload(recall_mod)
    assert recall_mod.PROJECT_AFFINITY_ALPHA == PROJECT_AFFINITY_ALPHA


def test_env_alpha_is_tunable_and_clamped(monkeypatch):
    monkeypatch.setenv("RECALL_PROJECT_ALPHA", "0.5")
    importlib.reload(recall_mod)
    try:
        assert recall_mod.PROJECT_AFFINITY_ALPHA == 0.5
    finally:
        monkeypatch.undo()
        importlib.reload(recall_mod)
    monkeypatch.setenv("RECALL_PROJECT_ALPHA", "-3")
    importlib.reload(recall_mod)
    try:
        assert recall_mod.PROJECT_AFFINITY_ALPHA == 0.0  # clamped, can't flip sign
    finally:
        monkeypatch.undo()
        importlib.reload(recall_mod)


# ---------- acceptance 3: R15 interaction — global scope only ----------

def test_empty_current_project_neutralizes_affinity():
    """The future R15 shard-scoped path passes current_project="" — affinity
    must then be neutral for every hit, matching or not."""
    cross = _lrn("cross", project="other-app")
    same = _lrn("same", project="my-app")
    _, scores = rerank_with_scores([cross, same], current_project="")
    assert scores["same"] == pytest.approx(scores["cross"])


def test_auto_detection_skipped_when_no_candidate_has_a_project(monkeypatch):
    """Project-less candidate sets must not pay the git-subprocess cost."""
    calls: list[str] = []
    monkeypatch.setattr(
        recall_mod, "detect_current_project",
        lambda: calls.append("hit") or "my-app",
    )
    rerank([_lrn("a"), _lrn("b")])
    assert calls == []
    rerank([_lrn("a", project="my-app"), _lrn("b")])
    assert calls == ["hit"]


def test_explicit_current_project_skips_detection(monkeypatch):
    monkeypatch.setattr(
        recall_mod, "detect_current_project",
        lambda: pytest.fail("detection must not run when project is explicit"),
    )
    rerank([_lrn("a", project="my-app")], current_project="my-app")
    rerank([_lrn("a", project="my-app")], current_project="")


# ---------- project identity: normalization + detection ----------

def test_normalize_project_handles_names_paths_and_junk():
    assert _normalize_project("my-app") == "my-app"
    assert _normalize_project("My-App") == "my-app"  # case-insensitive match key
    assert _normalize_project("/Users/x/dev/my-app") == "my-app"
    assert _normalize_project("/Users/x/dev/my-app/") == "my-app"
    assert _normalize_project("  my-app  ") == "my-app"
    assert _normalize_project("") == ""
    assert _normalize_project(None) == ""
    assert _normalize_project(True) == ""  # bool frontmatter junk
    assert _normalize_project("/") == ""


def test_learning_project_id_prefers_project_id_over_project():
    lrn = _lrn("a", project="canonical")
    lrn.frontmatter["project"] = "legacy"
    assert lrn.project_id == "canonical"
    lrn2 = _lrn("b")
    lrn2.frontmatter["project"] = "/Users/x/dev/Legacy-App"
    assert lrn2.project_id == "legacy-app"
    assert _lrn("c").project_id == ""


def test_detect_current_project_from_claude_project_dir(monkeypatch):
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", "/Users/x/dev/My-App")
    assert detect_current_project() == "my-app"


def test_detect_current_project_from_git_toplevel(monkeypatch, tmp_path):
    repo = tmp_path / "git-proj"
    repo.mkdir()
    subprocess.run(
        ["git", "init", "-q", str(repo)], check=True, capture_output=True
    )
    monkeypatch.delenv("CLAUDE_PROJECT_DIR", raising=False)
    monkeypatch.chdir(repo)
    assert detect_current_project() == "git-proj"


def test_detect_current_project_memoizes(monkeypatch):
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", "/dev/first")
    assert detect_current_project() == "first"
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", "/dev/second")
    assert detect_current_project() == "first"  # cached for the process


# ---------- config surface ----------

def test_reflect_config_declares_project_affinity_alpha(monkeypatch, tmp_path):
    import reflect_config

    monkeypatch.setattr(
        reflect_config, "_user_override_path", lambda: tmp_path / "absent-user.toml"
    )
    monkeypatch.setattr(
        reflect_config, "_project_override_path", lambda: tmp_path / "absent-proj.toml"
    )
    monkeypatch.delenv("REFLECT_RECALL_PROJECT_ALPHA", raising=False)
    cfg = reflect_config.load_config(force_reload=True)
    try:
        assert cfg["recall"]["boost"]["project_affinity_alpha"] == 0.2
        monkeypatch.setenv("REFLECT_RECALL_PROJECT_ALPHA", "0")
        cfg = reflect_config.load_config(force_reload=True)
        assert cfg["recall"]["boost"]["project_affinity_alpha"] == 0.0
    finally:
        monkeypatch.undo()
        reflect_config.load_config(force_reload=True)


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-v"]))
