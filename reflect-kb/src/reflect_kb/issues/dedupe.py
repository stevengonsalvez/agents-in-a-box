"""Issue candidate model + deduplication.

Ported from agent-deck's ``list_candidates.py`` / ``file_issue.py`` dedupe
logic, but keyed off reflect-kb's own state dir instead of agent-deck's
per-conductor manifest.

Three dedup layers, cheapest first:

1. **In-batch** — two candidates from the same run that slugify to the same
   fingerprint collapse to one.
2. **Local ledger** — ``filed_issues.json`` under ``REFLECT_STATE_DIR`` records
   every issue this tool has filed. A fingerprint present there is skipped. This
   is what makes a second run idempotent even offline.
3. **Remote** — ``gh issue list`` titles (open + closed) are fetched once;
   an exact-fingerprint match (``dup-on-github``) OR a high symmetric-Jaccard
   token overlap with a minimum shared-token floor (``dup-on-github-overlap``)
   marks a candidate as already-on-GitHub. The two are surfaced distinctly so a
   reviewer can tell an exact suppression from a softer fuzzy one.

The fingerprint is ``slugify(title)`` (lowercase, non-alphanumerics → ``-``,
truncated at 60 chars) — identical to agent-deck so a port of an existing
ledger stays compatible.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Optional

Runner = Callable[..., subprocess.CompletedProcess]

# Token overlap above which a candidate is treated as a duplicate of an
# existing GitHub issue. agent-deck used an asymmetric 50%-of-candidate-tokens
# heuristic, which false-positive-suppresses genuinely-new issues with short
# titles against long unrelated existing ones (a 2-word candidate sharing one
# generic word with a 10-word issue scored 50% and was wrongly dropped). We use
# SYMMETRIC Jaccard (shared / union) at a higher 0.7 threshold AND require a
# minimum shared-token count, so a single incidental shared word can never
# suppress a new issue. The exact-fingerprint fast path still matches identical
# titles regardless of overlap maths.
_TOKEN_OVERLAP = 0.7
_MIN_SHARED_TOKENS = 2

_LEDGER_VERSION = 1

_STOP = {
    "the",
    "a",
    "an",
    "to",
    "of",
    "in",
    "on",
    "for",
    "and",
    "or",
    "is",
    "via",
    "with",
    "when",
    "not",
    "use",
    "vs",
    "but",
    "from",
    "into",
}


@dataclass
class CandidateIssue:
    """A candidate GitHub issue produced by the pipeline (pre-publish)."""

    title: str
    body: str
    labels: list[str] = field(default_factory=list)
    source_citation: str = ""

    @property
    def fingerprint(self) -> str:
        return fingerprint(self.title)


@dataclass
class DedupeDecision:
    """Why a candidate was kept or dropped."""

    candidate: CandidateIssue
    keep: bool
    # "new" | "dup-in-batch" | "dup-in-ledger"
    # | "dup-on-github" (exact-fingerprint match)
    # | "dup-on-github-overlap" (fuzzy token-overlap match — softer signal)
    reason: str
    existing_ref: Optional[str] = None  # gh issue # / url / ledger fingerprint


def fingerprint(title: str) -> str:
    """``slugify(title)`` — the stable dedup key across runs."""
    s = re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")
    return s[:60] or "untitled"


def _tokens(title: str) -> set[str]:
    return {w for w in re.findall(r"[a-z0-9]+", title.lower()) if w not in _STOP and len(w) > 2}


def _token_overlap(a: str, b: str) -> tuple[float, int]:
    """Symmetric Jaccard overlap of two titles and the shared-token count.

    Returns ``(jaccard, shared)`` where ``jaccard = |a ∩ b| / |a ∪ b|``. Jaccard
    is symmetric, so a short candidate is not penalized for being shorter than a
    long existing title the way the old shared/candidate ratio was.
    """
    ta, tb = _tokens(a), _tokens(b)
    if not ta or not tb:
        return 0.0, 0
    shared = ta & tb
    union = ta | tb
    return len(shared) / len(union), len(shared)


def _default_runner(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, capture_output=True, text=True)


# ── local ledger ─────────────────────────────────────────────────────────────


def state_dir() -> Path:
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect")))


def ledger_path() -> Path:
    return state_dir() / "filed_issues.json"


def load_ledger(path: Optional[Path] = None) -> dict:
    p = path or ledger_path()
    if not p.exists():
        return {"version": _LEDGER_VERSION, "filed_issues": []}
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return {"version": _LEDGER_VERSION, "filed_issues": []}
    if not isinstance(data, dict) or "filed_issues" not in data:
        return {"version": _LEDGER_VERSION, "filed_issues": []}
    return data


def ledger_fingerprints(ledger: dict) -> set[str]:
    return {
        str(e.get("fingerprint", ""))
        for e in ledger.get("filed_issues", [])
        if e.get("fingerprint")
    }


def record_filed(
    ledger: dict,
    candidate: CandidateIssue,
    *,
    gh_issue_number: Optional[int] = None,
    gh_url: Optional[str] = None,
    status: str = "open",
) -> dict:
    """Append a filed-issue record to ``ledger`` (in-place) and return it."""
    ledger.setdefault("version", _LEDGER_VERSION)
    ledger.setdefault("filed_issues", []).append(
        {
            "fingerprint": candidate.fingerprint,
            "title": candidate.title,
            "gh_issue_number": gh_issue_number,
            "gh_url": gh_url,
            "source_citation": candidate.source_citation,
            "filed_at": datetime.now(timezone.utc).isoformat(),
            "status": status,
        }
    )
    return ledger


def save_ledger(ledger: dict, path: Optional[Path] = None) -> Path:
    """Atomic write (.tmp then replace) — agent-deck's file_issue.py wrote the
    ledger non-atomically and a crash mid-write could corrupt it; we don't
    repeat that footgun."""
    p = path or ledger_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(p.suffix + ".tmp")
    tmp.write_text(json.dumps(ledger, indent=2) + "\n", encoding="utf-8")
    tmp.replace(p)
    return p


# ── remote (gh) ──────────────────────────────────────────────────────────────


def fetch_existing_titles(
    repo: Optional[str] = None,
    *,
    limit: int = 500,
    runner: Runner = _default_runner,
) -> list[str]:
    """Return existing issue titles (open + closed) via ``gh issue list``.

    Returns ``[]`` if ``gh`` is unavailable or errors — dedupe degrades to the
    local ledger only (still idempotent for issues we filed ourselves).
    """
    cmd = ["gh", "issue", "list"]
    if repo:
        cmd += ["-R", repo]
    cmd += ["--state", "all", "--limit", str(limit), "--json", "title"]
    try:
        res = runner(cmd)
    except (subprocess.CalledProcessError, FileNotFoundError, OSError):
        return []
    try:
        rows = json.loads(res.stdout or "[]")
    except json.JSONDecodeError:
        return []
    return [str(r.get("title", "")) for r in rows if isinstance(r, dict)]


# ── partition ────────────────────────────────────────────────────────────────


def partition_candidates(
    candidates: list[CandidateIssue],
    *,
    ledger: Optional[dict] = None,
    existing_titles: Optional[list[str]] = None,
) -> list[DedupeDecision]:
    """Classify every candidate as keep/drop with a reason.

    Ordering matters: in-batch dupes are caught before ledger/remote so the
    reasons are precise and the cheapest check wins.
    """
    ledger = ledger or {"version": _LEDGER_VERSION, "filed_issues": []}
    existing_titles = existing_titles or []
    filed_fps = ledger_fingerprints(ledger)

    decisions: list[DedupeDecision] = []
    seen_in_batch: set[str] = set()

    for cand in candidates:
        fp = cand.fingerprint

        if fp in seen_in_batch:
            decisions.append(DedupeDecision(cand, False, "dup-in-batch", fp))
            continue

        if fp in filed_fps:
            decisions.append(DedupeDecision(cand, False, "dup-in-ledger", fp))
            seen_in_batch.add(fp)
            continue

        match = _match_existing(cand, existing_titles)
        if match is not None:
            existing_title, reason = match
            decisions.append(DedupeDecision(cand, False, reason, existing_title))
            seen_in_batch.add(fp)
            continue

        decisions.append(DedupeDecision(cand, True, "new"))
        seen_in_batch.add(fp)

    return decisions


def _match_existing(cand: CandidateIssue, existing_titles: list[str]) -> Optional[tuple[str, str]]:
    """Return ``(existing_title, reason)`` for the best dedupe match, else None.

    Exact-fingerprint matches win and are reported as ``dup-on-github``. Fuzzy
    token-overlap matches require BOTH a high symmetric-Jaccard score AND a
    minimum shared-token count, and are reported distinctly as
    ``dup-on-github-overlap`` so a reviewer can tell a soft fuzzy suppression
    from an exact one.
    """
    cand_fp = cand.fingerprint
    # Exact-fingerprint pass first — a precise match always beats a fuzzy one.
    for title in existing_titles:
        if fingerprint(title) == cand_fp:
            return title, "dup-on-github"
    # Fuzzy token-overlap pass — symmetric Jaccard AND a shared-count floor.
    for title in existing_titles:
        jaccard, shared = _token_overlap(cand.title, title)
        if jaccard >= _TOKEN_OVERLAP and shared >= _MIN_SHARED_TOKENS:
            return title, "dup-on-github-overlap"
    return None
