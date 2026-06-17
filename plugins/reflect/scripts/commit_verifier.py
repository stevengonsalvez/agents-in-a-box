#!/usr/bin/env python3
# ABOUTME: Commit-hash verification at write time (port M5, from claude-mem commit-verification.ts).
# ABOUTME: Extracts hex refs from learning text and checks them against the local repo; flags hallucinations.
"""Commit-reference verifier.

Port M5. LLMs fabricate commit hashes; a learning citing a sha that doesn't
exist poisons trust in the whole note. Before persistence:

1. extract candidate hashes (7-40 hex chars, word-bounded),
2. verify each via ``git cat-file -e <hash>^{commit}`` in the project repo,
3. return ``(verified, unverified)`` so the writer can record
   ``unverified_refs`` in frontmatter — or reject the note outright when
   *every* cited ref is fabricated.

Design notes:
- A generic ``RefVerifier`` protocol keeps the door open for file-path /
  symbol verifiers later (same hook, different extractor).
- Common false positives are excluded: pure-decimal strings, and hex words
  that are common English words ("deadbeef" passes, "added" must not —
  handled by requiring at least one digit).
- ``git`` failures (not a repo, git missing) verify NOTHING but also flag
  nothing — absence of evidence is not fabrication evidence. The caller
  distinguishes ``checked=False``.
"""
from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

__all__ = ["extract_commit_refs", "verify_refs", "RefReport"]

# 7-40 hex chars, word-bounded, not part of a longer token.
_HEX_RE = re.compile(r"(?<![0-9a-fA-F/])([0-9a-f]{7,40})(?![0-9a-fA-F])")


def extract_commit_refs(text: str) -> list[str]:
    """Candidate commit hashes in ``text`` (deduped, order-preserving).

    Requires at least one digit — filters all-letter hex-ish English words
    ("accede", "decade") while keeping real shas (vanishingly unlikely to be
    digit-free at >=7 chars).
    """
    seen: dict[str, None] = {}
    for m in _HEX_RE.finditer(text):
        cand = m.group(1)
        if any(c.isdigit() for c in cand):
            seen.setdefault(cand, None)
    return list(seen)


@dataclass
class RefReport:
    checked: bool = False            # False => git unavailable / not a repo
    verified: list[str] = field(default_factory=list)
    unverified: list[str] = field(default_factory=list)

    @property
    def all_unverified(self) -> bool:
        return self.checked and bool(self.unverified) and not self.verified


def _git_has_commit(sha: str, cwd: Path) -> bool:
    try:
        r = subprocess.run(
            ["git", "cat-file", "-e", f"{sha}^{{commit}}"],
            cwd=cwd, capture_output=True, timeout=5,
        )
        return r.returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def _is_git_repo(cwd: Path) -> bool:
    try:
        r = subprocess.run(
            ["git", "rev-parse", "--git-dir"],
            cwd=cwd, capture_output=True, timeout=5,
        )
        return r.returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def verify_refs(text: str, repo_dir: str | Path | None = None) -> RefReport:
    """Verify every commit-like ref in ``text`` against ``repo_dir``.

    ``repo_dir`` defaults to the cwd. When the dir isn't a git repo (or git
    is missing) the report has ``checked=False`` and nothing is flagged.
    """
    refs = extract_commit_refs(text)
    if not refs:
        return RefReport(checked=True)
    cwd = Path(repo_dir) if repo_dir else Path.cwd()
    if not _is_git_repo(cwd):
        return RefReport(checked=False)
    report = RefReport(checked=True)
    for sha in refs:
        (report.verified if _git_has_commit(sha, cwd) else report.unverified).append(sha)
    return report
