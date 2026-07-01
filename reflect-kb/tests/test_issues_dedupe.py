"""Tests for issue fingerprinting + 3-layer deduplication."""

from __future__ import annotations

import subprocess

from reflect_kb.issues.dedupe import (
    CandidateIssue,
    fetch_existing_titles,
    fingerprint,
    load_ledger,
    partition_candidates,
    record_filed,
    save_ledger,
)


def _cand(title: str) -> CandidateIssue:
    return CandidateIssue(title=title, body="body", labels=["bug"])


def test_fingerprint_is_slug_and_stable():
    assert fingerprint("CLI crashes on missing config!") == "cli-crashes-on-missing-config"
    assert fingerprint("CLI crashes on missing config") == fingerprint(
        "cli crashes on missing config"
    )
    assert fingerprint("") == "untitled"
    assert len(fingerprint("x " * 100)) <= 60


def test_in_batch_duplicates_collapse():
    cands = [_cand("Timeout in drain loop"), _cand("Timeout in drain loop!")]
    decisions = partition_candidates(cands)
    keeps = [d for d in decisions if d.keep]
    drops = [d for d in decisions if not d.keep]
    assert len(keeps) == 1
    assert len(drops) == 1
    assert drops[0].reason == "dup-in-batch"


def test_ledger_duplicate_is_skipped():
    ledger = {"version": 1, "filed_issues": []}
    record_filed(ledger, _cand("Race in plugin spawn"), gh_issue_number=5)
    decisions = partition_candidates([_cand("Race in plugin spawn")], ledger=ledger)
    assert not decisions[0].keep
    assert decisions[0].reason == "dup-in-ledger"


def test_existing_github_exact_title_skipped():
    decisions = partition_candidates(
        [_cand("Reflect drain double-files issues")],
        existing_titles=["Reflect drain double-files issues"],
    )
    assert not decisions[0].keep
    assert decisions[0].reason == "dup-on-github"


def test_existing_github_token_overlap_skipped():
    # High symmetric-Jaccard overlap (near-identical titles) is still caught,
    # but surfaced distinctly as a fuzzy overlap match, not an exact one.
    decisions = partition_candidates(
        [_cand("sanitizer misses telegram bot tokens")],
        existing_titles=["sanitizer misses telegram bot tokens entirely"],
    )
    assert not decisions[0].keep
    assert decisions[0].reason == "dup-on-github-overlap"


def test_genuinely_new_candidate_is_kept():
    decisions = partition_candidates(
        [_cand("Completely unrelated new finding about csv export")],
        existing_titles=["something about authentication flow"],
    )
    assert decisions[0].keep
    assert decisions[0].reason == "new"


def test_short_new_title_not_suppressed_by_long_unrelated_existing():
    # Regression for the asymmetric-overlap false-positive: a short genuine new
    # issue sharing one incidental word with a long unrelated existing issue
    # must NOT be suppressed. 'Drain loop hangs' vs 'loop counter wrong in
    # metrics drain' share {loop, drain} but describe different problems.
    decisions = partition_candidates(
        [_cand("Drain loop hangs")],
        existing_titles=["loop counter wrong in metrics drain reporting code"],
    )
    assert decisions[0].keep, decisions[0].reason
    assert decisions[0].reason == "new"


def test_ledger_roundtrip_atomic(tmp_path):
    path = tmp_path / "filed_issues.json"
    ledger = load_ledger(path)
    assert ledger["filed_issues"] == []
    record_filed(
        ledger, _cand("First issue"), gh_issue_number=1, gh_url="https://github.com/o/r/issues/1"
    )
    save_ledger(ledger, path)

    reloaded = load_ledger(path)
    assert len(reloaded["filed_issues"]) == 1
    assert reloaded["filed_issues"][0]["fingerprint"] == fingerprint("First issue")
    assert reloaded["filed_issues"][0]["gh_issue_number"] == 1
    # No stray .tmp left behind.
    assert not (tmp_path / "filed_issues.json.tmp").exists()


def test_corrupt_ledger_recovers_to_empty(tmp_path):
    path = tmp_path / "filed_issues.json"
    path.write_text("{ not valid json", encoding="utf-8")
    assert load_ledger(path) == {"version": 1, "filed_issues": []}


def test_fetch_existing_titles_parses_gh_json():
    def fake_runner(cmd):
        assert cmd[:3] == ["gh", "issue", "list"]
        return subprocess.CompletedProcess(
            cmd, 0, stdout='[{"title": "a bug"}, {"title": "b bug"}]', stderr=""
        )

    titles = fetch_existing_titles("o/r", runner=fake_runner)
    assert titles == ["a bug", "b bug"]


def test_fetch_existing_titles_degrades_when_gh_missing():
    def boom(cmd):
        raise FileNotFoundError("gh not installed")

    assert fetch_existing_titles("o/r", runner=boom) == []
