"""``reflect issues`` — distill recent session transcripts into privacy-sanitized,
deduplicated GitHub issues.

This is a NEW MODE inside the existing ``/reflect`` ecosystem, not a parallel
pipeline. It reuses:

* the ``~/.reflect/pending_reflections.jsonl`` queue (the same one
  ``stop_reflect.py`` / ``precompact_reflect.py`` append to) as the source of
  "recent transcripts" — see :mod:`reflect_kb.issues.manifest`;
* the reflect state dir (``REFLECT_STATE_DIR``, default ``~/.reflect``) for its
  idempotency ledger (``filed_issues.json``);
* the same conservative, secrets-first privacy posture the reflect plugin's
  prompts already mandate — see :mod:`reflect_kb.issues.sanitize`.

It ports the *logic* of agent-deck's self-improvement pipeline (distill →
analyze → sanitize → dedupe → file-issues) onto reflect-kb's Python style, but
NOT its storage (no ``analysis-manifest.json``, no per-conductor directory
tree) and NOT its agent-deck-specific session spawner.

Pipeline (see :mod:`reflect_kb.issues.pipeline`)::

    queue → distill (~30x, no LLM) → analyze (LLM, gated on auth)
          → candidate issues → sanitize (regex) → dedupe (gh + ledger)
          → gh issue create   (skipped entirely under --dry-run)

The single hard guarantee: nothing reaches ``gh issue create`` (or even the
terminal under ``--dry-run``) before passing through
:func:`reflect_kb.issues.sanitize.sanitize`.
"""

from __future__ import annotations

from reflect_kb.issues.dedupe import (
    CandidateIssue,
    DedupeDecision,
    fingerprint,
    partition_candidates,
)
from reflect_kb.issues.distill import DistillStats, distill, distill_file
from reflect_kb.issues.sanitize import SanitizeResult, sanitize
from reflect_kb.issues.pipeline import IssuesRunResult, run_issues

__all__ = [
    "CandidateIssue",
    "DedupeDecision",
    "DistillStats",
    "IssuesRunResult",
    "SanitizeResult",
    "distill",
    "distill_file",
    "fingerprint",
    "partition_candidates",
    "run_issues",
    "sanitize",
]
