"""End-to-end orchestration of ``reflect issues``.

Wires the modules together::

    gather (manifest)  →  distill (per transcript, ~30x, no LLM)
        →  analyze (one bounded LLM call, gated on Claude auth)
        →  sanitize EVERY candidate (regex, conservative)
        →  dedupe (in-batch / local ledger / gh issue list)
        →  file via `gh issue create`   [skipped under dry_run]

Safety invariants enforced here (not left to callers):

* The distilled timelines are sanitized BEFORE the analyzer sees them, and
  every candidate's title+body are sanitized AGAIN before it can be printed
  (dry-run) or filed. Nothing un-sanitized ever leaves :func:`run_issues`.
* ``dry_run=True`` never invokes ``gh issue create`` — it prints the exact
  bodies that WOULD be filed and returns.
* Filing appends to the atomic ``filed_issues.json`` ledger so a second run
  files zero duplicates even with ``gh`` offline.

The ``gh`` and ``claude`` calls are injected (``gh_runner`` / ``analyze_fn``)
so the whole flow is testable without a network or a model.
"""

from __future__ import annotations

import re
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional

from reflect_kb.issues import analyze as analyze_mod
from reflect_kb.issues import dedupe as dedupe_mod
from reflect_kb.issues import manifest as manifest_mod
from reflect_kb.issues.dedupe import CandidateIssue, DedupeDecision
from reflect_kb.issues.distill import distill_text
from reflect_kb.issues.sanitize import sanitize

Runner = Callable[..., subprocess.CompletedProcess]
AnalyzeFn = Callable[[list[str]], tuple[list[CandidateIssue], str]]


@dataclass
class FiledIssue:
    title: str
    fingerprint: str
    gh_issue_number: Optional[int] = None
    gh_url: Optional[str] = None


@dataclass
class IssuesRunResult:
    """Summary of one ``reflect issues`` run."""

    dry_run: bool
    transcripts_seen: int = 0
    transcripts_distilled: int = 0
    analyze_reason: str = ""
    candidates: int = 0
    filed: list[FiledIssue] = field(default_factory=list)
    skipped: list[DedupeDecision] = field(default_factory=list)
    previews: list[str] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)
    # Residual-suspicious flags from the sanitizer's non-mutating audit pass,
    # aggregated across every candidate's title+body. Each entry carries the
    # candidate title so a reviewer can see WHICH issue still looks suspicious.
    # Non-empty audit does NOT mean unsafe — it means "a human should eyeball
    # this before it's published".
    audit: list[dict] = field(default_factory=list)

    @property
    def filed_count(self) -> int:
        return len(self.filed)


def _default_gh_runner(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, capture_output=True, text=True)


def _sanitize_candidate(
    cand: CandidateIssue, maps: Optional[dict[str, str]]
) -> tuple[CandidateIssue, list[dict]]:
    """Sanitize ``cand`` for publication and surface its residual-audit flags.

    Returns the sanitized candidate AND the list of audit findings (the
    non-mutating "look here" flags the sanitizer raised on title+body), each
    tagged with the candidate title so the caller can attribute it. Dropping
    the audit — as an earlier version did — silently discarded the documented
    safety net that flags residual suspicious tokens for a human.
    """
    title_res = sanitize(cand.title, maps=maps)
    body_res = sanitize(cand.body, maps=maps)
    s_title = title_res.text.strip()
    safe = CandidateIssue(
        title=s_title or cand.title,
        body=body_res.text,
        labels=list(cand.labels),
        source_citation=cand.source_citation,
    )
    audit: list[dict] = []
    for finding in (*title_res.audit, *body_res.audit):
        audit.append({**finding, "candidate": safe.title})
    return safe, audit


def _filter_labels(
    labels: list[str],
    repo: Optional[str],
    gh_runner: Runner,
) -> list[str]:
    """Drop labels that don't exist in the target repo.

    Passing an unknown label to ``gh issue create`` fails the whole create
    atomically (a real agent-deck footgun). We fetch the repo's label set and
    keep only the intersection. On any error we return ``[]`` rather than risk
    a hard failure.
    """
    if not labels:
        return []
    cmd = ["gh", "label", "list"]
    if repo:
        cmd += ["-R", repo]
    cmd += ["--limit", "200", "--json", "name"]
    try:
        res = gh_runner(cmd)
        import json as _json

        valid = {str(r.get("name", "")) for r in _json.loads(res.stdout or "[]")}
    except (subprocess.CalledProcessError, FileNotFoundError, OSError, ValueError):
        return []
    return [lab for lab in labels if lab in valid]


# Default provenance markers. Every issue this mode files is titled
# ``reflect: <title>`` and carries a ``reflect`` label so it's obvious — in the
# issue list, in search, and in filters — that it came from the reflect issues
# pipeline rather than a human. Both are overridable via the ``[issues]`` config
# (``title_prefix`` / ``label``) or CLI flags.
DEFAULT_TITLE_PREFIX = "reflect: "
DEFAULT_LABEL = "reflect"
# GitHub label colour (clay) + description used when we auto-create the label.
_LABEL_COLOR = "D97757"
_LABEL_DESC = "Filed automatically by `reflect issues` from session transcripts"


def _decorate(
    cand: CandidateIssue,
    title_prefix: str,
    label: str,
) -> CandidateIssue:
    """Stamp provenance onto a candidate: ``reflect:`` title prefix + label.

    Idempotent — a title that already starts with the prefix (case-insensitive)
    is left as-is, and the label is only added once. Applied BEFORE dedupe so
    the fingerprint (``slugify(title)``) is computed on the final, prefixed
    title — keeping it consistent with previously-filed ``reflect: ...`` issues.
    """
    title = cand.title
    if title_prefix and not title.lower().startswith(title_prefix.strip().lower()):
        title = f"{title_prefix}{title}"
    labels = list(cand.labels)
    if label and label not in labels:
        labels = [label, *labels]
    return CandidateIssue(
        title=title,
        body=cand.body,
        labels=labels,
        source_citation=cand.source_citation,
    )


def _ensure_label(label: str, repo: Optional[str], gh_runner: Runner) -> None:
    """Best-effort create the provenance label so ``_filter_labels`` keeps it.

    ``_filter_labels`` drops labels absent from the repo (an unknown label fails
    the whole ``gh issue create``), so the ``reflect`` label would silently
    vanish on a repo that doesn't have it yet. Create it idempotently — ``gh
    label create`` exits non-zero if it already exists, which we swallow. Never
    raises; a failure just means the label may get filtered out (issues still
    file, just unlabelled).
    """
    if not label:
        return
    cmd = ["gh", "label", "create", label, "--color", _LABEL_COLOR, "--description", _LABEL_DESC]
    if repo:
        cmd += ["-R", repo]
    try:
        gh_runner(cmd)
    except (subprocess.CalledProcessError, FileNotFoundError, OSError):
        # Already exists (most common) or gh unavailable — non-fatal.
        return


_ISSUE_URL_RE = re.compile(r"/issues/(\d+)\s*$")


def _file_one(
    cand: CandidateIssue,
    repo: Optional[str],
    gh_runner: Runner,
) -> FiledIssue:
    """Create one GitHub issue via ``gh issue create``. Caller has already
    sanitized + deduped ``cand``."""
    labels = _filter_labels(cand.labels, repo, gh_runner)
    cmd = ["gh", "issue", "create"]
    if repo:
        cmd += ["-R", repo]
    cmd += ["--title", cand.title, "--body", cand.body]
    if labels:
        cmd += ["--label", ",".join(labels)]
    res = gh_runner(cmd)
    url = (getattr(res, "stdout", "") or "").strip().splitlines()
    gh_url = next((ln.strip() for ln in url if ln.strip().startswith("https://")), None)
    number = None
    if gh_url:
        m = _ISSUE_URL_RE.search(gh_url)
        if m:
            number = int(m.group(1))
    return FiledIssue(
        title=cand.title,
        fingerprint=cand.fingerprint,
        gh_issue_number=number,
        gh_url=gh_url,
    )


def _render_preview(cand: CandidateIssue) -> str:
    labels = f" [{', '.join(cand.labels)}]" if cand.labels else ""
    return f"### {cand.title}{labels}\n{cand.body}\n"


def run_issues(
    *,
    repo: Optional[str] = None,
    limit: int = 20,
    dry_run: bool = False,
    maps: Optional[dict[str, str]] = None,
    model: str = "sonnet",
    title_prefix: str = DEFAULT_TITLE_PREFIX,
    label: str = DEFAULT_LABEL,
    queue: Optional[Path] = None,
    ledger_path: Optional[Path] = None,
    analyze_fn: Optional[AnalyzeFn] = None,
    gh_runner: Optional[Runner] = None,
    fetch_titles: Optional[Callable[[], list[str]]] = None,
) -> IssuesRunResult:
    """Run the transcripts → issues pipeline once.

    Args:
        repo: ``owner/name`` for ``gh`` (defaults to the cwd's repo).
        limit: max transcripts to pull from the queue.
        dry_run: print bodies that WOULD be filed; never call ``gh``.
        maps: caller-supplied sanitizer substitutions.
        model: model passed to the analyzer.
        queue / ledger_path: override state locations (tests).
        analyze_fn / gh_runner / fetch_titles: injected for tests; default to
            the real :mod:`reflect_kb.issues.analyze` / ``gh`` calls.

    Returns:
        An :class:`IssuesRunResult`.
    """
    result = IssuesRunResult(dry_run=dry_run)
    gh_run = gh_runner or _default_gh_runner

    # 1. Gather recent transcripts from the existing reflect queue.
    refs = manifest_mod.gather_transcripts(queue=queue, limit=limit)
    result.transcripts_seen = len(refs)
    if not refs:
        result.notes.append(
            "no transcripts in the reflect queue (~/.reflect/pending_reflections.jsonl)"
        )
        return result

    # 2. Distill each transcript and sanitize the timeline before the model.
    timelines: list[str] = []
    for ref in refs:
        try:
            raw = ref.transcript_path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        markdown, stats = distill_text(raw)
        if stats.kept_total == 0:
            continue
        safe_timeline = sanitize(markdown, maps=maps).text
        timelines.append(safe_timeline)
        result.transcripts_distilled += 1

    if not timelines:
        result.notes.append("transcripts distilled to zero signal; nothing to analyze")
        return result

    # 3. Analyze → candidate issues (gated on Claude auth; degrades to []).
    if analyze_fn is not None:
        candidates, reason = analyze_fn(timelines)
    else:
        candidates, reason = analyze_mod.analyze(timelines, model=model)
    result.analyze_reason = reason
    result.candidates = len(candidates)
    if not candidates:
        result.notes.append(f"analyzer produced no candidates (reason: {reason})")
        return result

    # 4. Sanitize EVERY candidate again (defence in depth) before it can leave,
    #    aggregating each candidate's residual-audit flags onto the run result.
    safe_candidates: list[CandidateIssue] = []
    for c in candidates:
        safe, audit = _sanitize_candidate(c, maps)
        # Stamp provenance (reflect: prefix + label) BEFORE dedupe so the
        # fingerprint matches previously-filed `reflect: ...` issues.
        safe_candidates.append(_decorate(safe, title_prefix, label))
        result.audit.extend(audit)

    # 5. Dedupe: in-batch → local ledger → existing GitHub issues.
    ledger = dedupe_mod.load_ledger(ledger_path)
    if fetch_titles is not None:
        existing = fetch_titles()
    elif dry_run:
        # Dry-run still consults gh for an accurate preview of what's new, but
        # tolerates gh being absent.
        existing = dedupe_mod.fetch_existing_titles(repo, runner=gh_run)
    else:
        existing = dedupe_mod.fetch_existing_titles(repo, runner=gh_run)

    decisions = dedupe_mod.partition_candidates(
        safe_candidates, ledger=ledger, existing_titles=existing
    )
    keepers = [d.candidate for d in decisions if d.keep]
    result.skipped = [d for d in decisions if not d.keep]

    # 6. File (or preview).
    if dry_run:
        for cand in keepers:
            result.previews.append(_render_preview(cand))
        result.notes.append(
            f"dry-run: {len(keepers)} issue(s) WOULD be filed, "
            f"{len(result.skipped)} skipped as duplicates"
        )
        return result

    # Ensure the provenance label exists so it survives _filter_labels (an
    # unknown label would otherwise be dropped). Best-effort, once per run.
    if keepers and label:
        _ensure_label(label, repo, gh_run)

    for cand in keepers:
        try:
            filed = _file_one(cand, repo, gh_run)
        except (subprocess.CalledProcessError, FileNotFoundError, OSError) as exc:
            result.notes.append(f"gh issue create failed for '{cand.title}': {exc}")
            continue
        result.filed.append(filed)
        dedupe_mod.record_filed(
            ledger,
            cand,
            gh_issue_number=filed.gh_issue_number,
            gh_url=filed.gh_url,
        )
        # Persist incrementally — the atomic save runs after EACH successful
        # file, not once at the end. A crash after `gh issue create` succeeds
        # but before the ledger is saved would otherwise leave the filed issue
        # unrecorded → a duplicate on the next run.
        dedupe_mod.save_ledger(ledger, ledger_path)

    return result
