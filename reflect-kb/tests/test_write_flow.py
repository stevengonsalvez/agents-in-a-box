"""Tests for the confidence-gated write flow (v4 §Phase 4).

We build a tiny loopback "origin -> team_root" git topology per-test so the
HIGH/MED paths exercise real git plumbing without hitting the network. The
``gh`` CLI is faked via dependency injection — we never shell out to it.
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest
import yaml

from reflect_kb import write_flow


def _git(cmd: list[str], cwd: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *cmd], cwd=cwd, check=True, capture_output=True, text=True
    )


def _init_origin_and_clone(tmp_path: Path) -> tuple[Path, Path]:
    """Create a bare ``origin.git`` + a ``team_root`` clone seeded with main."""
    origin = tmp_path / "origin.git"
    _git(["init", "--bare", "--initial-branch=main", str(origin)], tmp_path)

    # Seed the bare repo by pushing an initial commit from a scratch workspace.
    seed = tmp_path / "seed"
    seed.mkdir()
    _git(["init", "-q", "--initial-branch=main"], seed)
    _git(["config", "user.email", "t@t"], seed)
    _git(["config", "user.name", "t"], seed)
    (seed / "README.md").write_text("team kb\n", encoding="utf-8")
    _git(["add", "."], seed)
    _git(["commit", "-q", "-m", "init"], seed)
    _git(["push", "-q", str(origin), "HEAD:main"], seed)

    team_root = tmp_path / "team"
    _git(["clone", "-q", str(origin), str(team_root)], tmp_path)
    _git(["config", "user.email", "t@t"], team_root)
    _git(["config", "user.name", "t"], team_root)
    return origin, team_root


def _write_doc(path: Path, *, title: str, confidence: str | None,
               extra: dict | None = None) -> Path:
    fm: dict = {
        "title": title,
        "created": "2026-04-24",
        "category": "testing",
        "key_insight": "dummy",
    }
    if confidence is not None:
        fm["confidence"] = confidence
    if extra:
        fm.update(extra)
    body = f"---\n{yaml.safe_dump(fm, sort_keys=True)}---\n\nBody for {title}.\n"
    path.write_text(body, encoding="utf-8")
    return path


# ----- classify / slugify / parse ---------------------------------------------


@pytest.mark.parametrize(
    "given,expected",
    [
        ({"confidence": "high"}, write_flow.ROUTE_HIGH),
        ({"confidence": "HIGH"}, write_flow.ROUTE_HIGH),
        ({"confidence": "H"}, write_flow.ROUTE_HIGH),
        ({"confidence": "medium"}, write_flow.ROUTE_MED),
        ({"confidence": "MED"}, write_flow.ROUTE_MED),
        ({"confidence": "low"}, write_flow.ROUTE_LOW),
        ({"confidence": "garbage"}, write_flow.ROUTE_MED),  # unknown -> default
        ({}, write_flow.ROUTE_MED),  # missing -> default
    ],
)
def test_classify_confidence(given, expected):
    assert write_flow.classify_confidence(given) == expected


def test_slugify_handles_unicode_and_length():
    slug = write_flow.slugify("Tokio runtime nested panic — diagnosis")
    assert slug.startswith("tokio-runtime-nested-panic")
    assert len(slug) <= 60
    assert write_flow.slugify("") == "untitled"


# ----- HIGH path --------------------------------------------------------------


def test_route_high_commits_and_pushes_to_main(tmp_path):
    origin, team_root = _init_origin_and_clone(tmp_path)
    doc = _write_doc(
        tmp_path / "learning-high.md",
        title="Cache headers prevent double-write",
        confidence="high",
    )
    sidecar = doc.with_suffix(".entities.yaml")
    sidecar.write_text("entities: []\n", encoding="utf-8")

    result = write_flow.route_document(doc, team_root=team_root)

    assert result.route == write_flow.ROUTE_HIGH
    assert result.confidence == write_flow.ROUTE_HIGH
    assert result.pushed is True, result.notes
    assert result.commit_sha

    # Staged files landed under documents/ in the clone
    doc_rel = team_root / "documents" / doc.name
    sidecar_rel = team_root / "documents" / sidecar.name
    assert doc_rel.exists()
    assert sidecar_rel.exists()

    # Last commit message carries the [HIGH] marker
    msg = _git(["log", "-1", "--pretty=%s"], team_root).stdout.strip()
    assert "[HIGH]" in msg
    assert "Cache headers prevent double-write" in msg

    # Origin received the commit on main
    origin_sha = _git(["rev-parse", "main"], origin).stdout.strip()
    assert origin_sha == result.commit_sha


# ----- MED path ---------------------------------------------------------------


def test_route_medium_pushes_branch_and_invokes_gh(tmp_path):
    origin, team_root = _init_origin_and_clone(tmp_path)
    doc = _write_doc(
        tmp_path / "learning-med.md",
        title="Unsure mitigation for flaky test",
        confidence="medium",
    )

    fake_pr_url = "https://github.com/org/team-kb/pull/42"
    gh_calls: list[list[str]] = []

    def fake_gh(cmd, cwd=None, check=True):
        gh_calls.append(cmd)
        return subprocess.CompletedProcess(
            args=cmd, returncode=0, stdout=f"{fake_pr_url}\n", stderr="",
        )

    result = write_flow.route_document(
        doc, team_root=team_root, gh=fake_gh, gh_available=lambda: True,
    )

    assert result.route == write_flow.ROUTE_MED
    assert result.branch == f"knowledge/{result.slug}"
    assert result.pushed is True, result.notes
    assert result.pr_url == fake_pr_url, result.notes

    # The gh call was a draft PR with the right head branch
    assert gh_calls, "expected one gh pr create invocation"
    cmd = gh_calls[0]
    assert "gh" in cmd[0] and "pr" in cmd and "create" in cmd
    assert "--draft" in cmd
    assert "--head" in cmd
    head_idx = cmd.index("--head") + 1
    assert cmd[head_idx] == result.branch

    # Branch made it to origin
    branches = _git(["branch", "-r"], team_root).stdout
    assert f"origin/{result.branch}" in branches


def test_route_medium_degrades_gracefully_when_gh_missing(tmp_path):
    _, team_root = _init_origin_and_clone(tmp_path)
    doc = _write_doc(
        tmp_path / "learning-med-no-gh.md",
        title="Needs review",
        confidence=None,  # unset -> default to MED
    )

    result = write_flow.route_document(
        doc, team_root=team_root, gh_available=lambda: False,
    )

    assert result.route == write_flow.ROUTE_MED
    assert result.pushed is True
    assert result.pr_url is None
    assert any("gh not available" in n for n in result.notes), result.notes


# ----- LOW path ---------------------------------------------------------------


def test_route_low_writes_review_queue_yaml(tmp_path):
    doc = _write_doc(
        tmp_path / "learning-low.md",
        title="Hunch about connection pool timeout",
        confidence="low",
        extra={"tags": ["speculative", "db"]},
    )
    queue_dir = tmp_path / "queue"

    result = write_flow.route_document(doc, team_root=None, queue_dir=queue_dir)

    assert result.route == write_flow.ROUTE_LOW
    assert result.queue_path is not None
    assert result.queue_path.exists()
    payload = yaml.safe_load(result.queue_path.read_text())
    assert payload["slug"] == result.slug
    assert payload["title"] == "Hunch about connection pool timeout"
    assert payload["source"] == str(doc)
    assert payload["confidence"] == "low"
    assert payload["tags"] == ["speculative", "db"]
    assert "queued_at" in payload


def test_route_without_team_falls_back_to_queue(tmp_path):
    """MED/HIGH docs should fall back to LOW queue when no team is configured."""
    doc = _write_doc(
        tmp_path / "orphan.md",
        title="No team configured",
        confidence="high",
    )
    queue_dir = tmp_path / "queue"
    result = write_flow.route_document(doc, team_root=None, queue_dir=queue_dir)

    assert result.queue_path is not None
    assert result.queue_path.exists()
    assert any("no team KB configured" in n for n in result.notes)
