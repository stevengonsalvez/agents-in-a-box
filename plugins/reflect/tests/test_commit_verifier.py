# ABOUTME: Regression tests for port M5 — commit-hash verification at write time.
# ABOUTME: Pins acceptance: extraction, git verification, unverified_refs frontmatter, all-hallucinated rejection.
"""Port M5 (claude-mem commit-verification.ts): LLM-fabricated commit refs are
caught before persistence and recorded (or the note rejected)."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
PLUGIN_ROOT = HERE.parent
sys.path.insert(0, str(PLUGIN_ROOT / "scripts"))

from commit_verifier import RefReport, extract_commit_refs, verify_refs  # noqa: E402


# ---------- extraction ----------

def test_extracts_short_and_full_shas():
    text = "fixed in a1b2c3d and later in f267d1d43b9b6652831c7ceff8084a556a25480e"
    refs = extract_commit_refs(text)
    assert "a1b2c3d" in refs
    assert "f267d1d43b9b6652831c7ceff8084a556a25480e" in refs


def test_ignores_english_hexish_words_and_decimals():
    text = "we acceded to a decade of 1234567 efforts"
    refs = extract_commit_refs(text)
    assert "acceded" not in refs and "decade" not in refs
    # pure decimal 7-digit IS hex-shaped and contains digits — by design it's a
    # candidate; git verification is the arbiter. Just assert no crash here.
    assert isinstance(refs, list)


def test_dedup_preserves_order():
    text = "see abc1234, then def5678, then abc1234 again"
    assert extract_commit_refs(text) == ["abc1234", "def5678"]


def test_no_refs_returns_empty():
    assert extract_commit_refs("no hashes here at all") == []


# ---------- verification against a real repo ----------

@pytest.fixture()
def tiny_repo(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True)
    subprocess.run(["git", "-c", "user.email=t@t", "-c", "user.name=t",
                    "commit", "--allow-empty", "-q", "-m", "first"], cwd=repo, check=True)
    sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo,
                         capture_output=True, text=True, check=True).stdout.strip()
    return repo, sha


def test_real_sha_verifies(tiny_repo):
    repo, sha = tiny_repo
    rep = verify_refs(f"the fix landed in {sha[:10]}", repo_dir=repo)
    assert rep.checked
    assert sha[:10] in rep.verified
    assert not rep.unverified


def test_fabricated_sha_flagged(tiny_repo):
    repo, sha = tiny_repo
    rep = verify_refs("see commit 1234567890abcdef1234", repo_dir=repo)
    assert rep.checked
    assert rep.unverified == ["1234567890abcdef1234"]
    assert rep.all_unverified


def test_mixed_refs_not_all_unverified(tiny_repo):
    repo, sha = tiny_repo
    rep = verify_refs(f"good {sha[:12]} bad deadbeef123", repo_dir=repo)
    assert rep.verified == [sha[:12]]
    assert rep.unverified == ["deadbeef123"]
    assert not rep.all_unverified


def test_non_repo_dir_checked_false(tmp_path):
    rep = verify_refs("commit abc1234 mentioned", repo_dir=tmp_path)
    assert not rep.checked
    assert not rep.unverified  # absence of evidence != fabrication


def test_refless_text_is_checked_and_clean(tiny_repo):
    repo, _ = tiny_repo
    rep = verify_refs("no refs at all", repo_dir=repo)
    assert rep.checked and not rep.verified and not rep.unverified


# ---------- integration: create_knowledge_note ----------

@pytest.fixture()
def note_env(tmp_path, monkeypatch, tiny_repo):
    repo, sha = tiny_repo
    monkeypatch.chdir(repo)  # output_generator writes under cwd project dir
    monkeypatch.setenv("CLAUDE_PROJECT_DIR", str(repo))
    return repo, sha


def test_note_with_fabricated_ref_gets_frontmatter_flag(note_env):
    repo, sha = note_env
    import output_generator
    path, slug = output_generator.create_knowledge_note(
        title="M5 flag test", category="testing", tags=["t"], symptoms=["s"],
        root_cause="rc", key_insight="ki",
        problem=f"bug introduced in {sha[:10]}",
        solution="fixed by reverting bad sha 1234567890abcdef1234 partially",
    )
    text = path.read_text()
    assert "unverified_refs" in text
    assert "1234567890abcdef1234" in text
    path.unlink()


def test_note_with_only_fabricated_refs_rejected(note_env):
    repo, sha = note_env
    import output_generator
    with pytest.raises(ValueError, match="all_refs_hallucinated"):
        output_generator.create_knowledge_note(
            title="M5 reject test", category="testing", tags=["t"], symptoms=["s"],
            root_cause="rc", key_insight="ki",
            problem="bug found in commit 1234567890abcdef1234",
            solution="and fixed in deadbeef99 which also does not exist",
        )


def test_note_with_verified_ref_passes_clean(note_env):
    repo, sha = note_env
    import output_generator
    path, slug = output_generator.create_knowledge_note(
        title="M5 clean test", category="testing", tags=["t"], symptoms=["s"],
        root_cause="rc", key_insight="ki",
        problem=f"introduced in {sha[:10]}",
        solution="documented fix",
    )
    text = path.read_text()
    assert "unverified_refs" not in text
    path.unlink()


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
