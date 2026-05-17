"""Confidence-gated write routing for reflect-kb (v4 spec §Phase 4).

Every learning captured via `/reflect` or `reflect share` flows through
:func:`route_document`, which dispatches based on the ``confidence`` field
in the YAML frontmatter:

    HIGH   → stage document + sidecar, commit ``feat(knowledge): <title> [HIGH]``,
             push to team-kb ``main``.
    MED    → create branch ``knowledge/<slug>``, commit, push, open draft PR
             via ``gh pr create``. If ``gh`` is not configured, the PR step is
             a no-op (the branch is still pushed so a human can finish it).
    LOW    → write a pointer record to ``~/.learnings/review-queue/<slug>.yaml``
             so the user can decide later whether to promote.

    (missing) → treated as MED (v4 default).

The module never rewrites the source document — callers stage a local copy
first (see spec §"Team KB Write Flow": "ALWAYS writes local copy to
~/.learnings/ first (no data loss if push fails)").
"""

from __future__ import annotations

import re
import shutil
import subprocess
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Optional

import yaml

ROUTE_HIGH = "high"
ROUTE_MED = "medium"
ROUTE_LOW = "low"
DEFAULT_ROUTE = ROUTE_MED

_CONFIDENCE_ALIASES: dict[str, str] = {
    "high": ROUTE_HIGH,
    "h": ROUTE_HIGH,
    "med": ROUTE_MED,
    "medium": ROUTE_MED,
    "m": ROUTE_MED,
    "low": ROUTE_LOW,
    "l": ROUTE_LOW,
}

REVIEW_QUEUE_DIR = Path.home() / ".learnings" / "review-queue"

# Callable protocol for subprocess wrappers used in git/gh operations.
# Tests inject fakes; production uses :func:`_default_runner`.
Runner = Callable[..., subprocess.CompletedProcess]


def _default_runner(cmd: list[str], cwd: Optional[Path] = None, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, check=check, capture_output=True, text=True)


def parse_frontmatter(text: str) -> tuple[dict, str]:
    """Split a ``--- yaml --- body`` document. Missing/invalid → ``({}, text)``."""
    if not text.startswith("---"):
        return {}, text
    parts = text.split("---", 2)
    if len(parts) < 3:
        return {}, text
    try:
        fm = yaml.safe_load(parts[1]) or {}
    except yaml.YAMLError:
        return {}, text
    if not isinstance(fm, dict):
        return {}, text
    return fm, parts[2].strip()


def classify_confidence(frontmatter: dict) -> str:
    """Return one of ``high``/``medium``/``low``. Unknown or missing → ``DEFAULT_ROUTE``.

    The schema stores confidence as lowercase (``high``/``medium``/``low``), but
    ingested docs in the wild frequently use ``HIGH`` or ``MED``. We normalise.
    """
    val = frontmatter.get("confidence")
    if val is None:
        return DEFAULT_ROUTE
    key = str(val).strip().lower()
    return _CONFIDENCE_ALIASES.get(key, DEFAULT_ROUTE)


def slugify(title: str) -> str:
    s = re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")
    return s[:60] or "untitled"


@dataclass
class RouteResult:
    """Outcome of a single document routing decision."""

    route: str
    title: str
    slug: str
    confidence: str
    staged_paths: list[Path] = field(default_factory=list)
    branch: Optional[str] = None
    commit_sha: Optional[str] = None
    pushed: bool = False
    pr_url: Optional[str] = None
    queue_path: Optional[Path] = None
    notes: list[str] = field(default_factory=list)


def _find_sidecar(doc: Path) -> Optional[Path]:
    candidate = doc.with_suffix(".entities.yaml")
    return candidate if candidate.exists() else None


def _copy_into_team(doc: Path, team_root: Path) -> list[Path]:
    """Copy the document (and sidecar, if present) into ``team_root/documents``."""
    docs_dir = team_root / "documents"
    docs_dir.mkdir(parents=True, exist_ok=True)
    dest_doc = docs_dir / doc.name
    dest_doc.write_bytes(doc.read_bytes())
    staged = [dest_doc]
    sidecar = _find_sidecar(doc)
    if sidecar is not None:
        dest_sidecar = docs_dir / sidecar.name
        dest_sidecar.write_bytes(sidecar.read_bytes())
        staged.append(dest_sidecar)
    return staged


def _stage_and_commit(
    staged: list[Path], team_root: Path, message: str, git: Runner,
) -> str:
    rels = [str(p.relative_to(team_root)) for p in staged]
    git(["git", "add", *rels], cwd=team_root)
    git(["git", "commit", "-m", message], cwd=team_root)
    return git(["git", "rev-parse", "HEAD"], cwd=team_root).stdout.strip()


def _safe_push(cmd: list[str], team_root: Path, git: Runner, notes: list[str]) -> bool:
    try:
        git(cmd, cwd=team_root)
        return True
    except subprocess.CalledProcessError as exc:
        stderr = (exc.stderr or "").strip()
        notes.append(f"push failed ({' '.join(cmd[2:])}): {stderr[:200]}")
        return False


def route_high(
    doc: Path,
    *,
    team_root: Path,
    title: str,
    slug: str,
    confidence: str = ROUTE_HIGH,
    git: Runner = _default_runner,
) -> RouteResult:
    """HIGH path: commit directly to ``main`` and push."""
    staged = _copy_into_team(doc, team_root)
    message = f"feat(knowledge): {title} [HIGH]"
    sha = _stage_and_commit(staged, team_root, message, git)
    notes: list[str] = []
    pushed = _safe_push(["git", "push", "origin", "HEAD:main"], team_root, git, notes)
    return RouteResult(
        route=ROUTE_HIGH,
        title=title,
        slug=slug,
        confidence=confidence,
        staged_paths=staged,
        commit_sha=sha,
        pushed=pushed,
        notes=notes,
    )


def route_medium(
    doc: Path,
    *,
    team_root: Path,
    title: str,
    slug: str,
    confidence: str = ROUTE_MED,
    git: Runner = _default_runner,
    gh: Runner = _default_runner,
    gh_available: Optional[Callable[[], bool]] = None,
) -> RouteResult:
    """MED path: push a ``knowledge/<slug>`` branch and open a draft PR."""
    branch = f"knowledge/{slug}"
    # -B creates or resets, so rerunning the flow is idempotent when the
    # branch already exists locally.
    git(["git", "checkout", "-B", branch], cwd=team_root)
    staged = _copy_into_team(doc, team_root)
    message = f"docs(knowledge): {title} [MED]"
    sha = _stage_and_commit(staged, team_root, message, git)
    notes: list[str] = []
    pushed = _safe_push(["git", "push", "-u", "origin", branch], team_root, git, notes)

    pr_url: Optional[str] = None
    gh_check = gh_available if gh_available is not None else (lambda: shutil.which("gh") is not None)
    if pushed and gh_check():
        try:
            res = gh(
                [
                    "gh", "pr", "create",
                    "--draft",
                    "--title", message,
                    "--body", _pr_body(title, slug, confidence),
                    "--head", branch,
                ],
                cwd=team_root,
            )
            pr_url = _extract_pr_url(res.stdout)
        except subprocess.CalledProcessError as exc:
            stderr = (exc.stderr or "").strip()
            notes.append(f"gh pr create failed: {stderr[:200]}")
    elif pushed:
        notes.append("gh not available — branch pushed; open the PR manually")
    # If not pushed, _safe_push already logged why.

    return RouteResult(
        route=ROUTE_MED,
        title=title,
        slug=slug,
        confidence=confidence,
        staged_paths=staged,
        branch=branch,
        commit_sha=sha,
        pushed=pushed,
        pr_url=pr_url,
        notes=notes,
    )


def route_low(
    doc: Path,
    *,
    title: str,
    slug: str,
    frontmatter: dict,
    queue_dir: Path = REVIEW_QUEUE_DIR,
    confidence: str = ROUTE_LOW,
) -> RouteResult:
    """LOW path: write a pointer YAML to the review queue."""
    queue_dir.mkdir(parents=True, exist_ok=True)
    queue_path = queue_dir / f"{slug}.yaml"
    payload = {
        "slug": slug,
        "title": title,
        "source": str(doc),
        "confidence": frontmatter.get("confidence", "low"),
        "category": frontmatter.get("category"),
        "tags": frontmatter.get("tags"),
        "created": _coerce_yaml_scalar(frontmatter.get("created")),
        "queued_at": datetime.now(timezone.utc).isoformat(),
    }
    queue_path.write_text(yaml.safe_dump(payload, sort_keys=True), encoding="utf-8")
    return RouteResult(
        route=ROUTE_LOW,
        title=title,
        slug=slug,
        confidence=confidence,
        queue_path=queue_path,
        notes=[f"queued for review: {queue_path}"],
    )


def route_document(
    doc: Path,
    *,
    team_root: Optional[Path] = None,
    queue_dir: Path = REVIEW_QUEUE_DIR,
    git: Runner = _default_runner,
    gh: Runner = _default_runner,
    gh_available: Optional[Callable[[], bool]] = None,
) -> RouteResult:
    """Classify ``doc`` and dispatch to the appropriate route.

    Args:
        doc: Absolute path to a markdown learning with YAML frontmatter.
        team_root: Path to a cloned team-kb. When ``None``, HIGH/MED routes fall
            back to the review queue so no content is silently dropped.
        queue_dir: Where LOW-routed (and fallback) entries are queued.
        git: Runner used for all ``git`` invocations. Tests pass fakes.
        gh: Runner used for the ``gh pr create`` step.
        gh_available: Predicate that returns ``True`` when the ``gh`` CLI is
            usable. Defaults to a ``shutil.which`` check; tests override.

    Returns:
        A :class:`RouteResult` summarising what happened. ``notes`` always
        explains degraded paths (push failures, missing tooling, etc.).
    """
    content = doc.read_text(encoding="utf-8")
    fm, _ = parse_frontmatter(content)
    title = fm.get("title") or doc.stem
    slug = slugify(title)
    route = classify_confidence(fm)

    if route == ROUTE_LOW:
        return route_low(
            doc, title=title, slug=slug, frontmatter=fm,
            queue_dir=queue_dir, confidence=route,
        )

    if team_root is None:
        result = route_low(
            doc, title=title, slug=slug, frontmatter=fm,
            queue_dir=queue_dir, confidence=route,
        )
        result.notes.insert(
            0,
            "no team KB configured (run `reflect team clone <url>`); queued for review",
        )
        return result

    if route == ROUTE_HIGH:
        return route_high(
            doc, team_root=team_root, title=title, slug=slug, confidence=route, git=git,
        )
    return route_medium(
        doc, team_root=team_root, title=title, slug=slug, confidence=route,
        git=git, gh=gh, gh_available=gh_available,
    )


def _pr_body(title: str, slug: str, confidence: str) -> str:
    return (
        f"Auto-generated MED-confidence learning from `reflect share`.\n\n"
        f"- Title: **{title}**\n"
        f"- Slug: `{slug}`\n"
        f"- Confidence: `{confidence}`\n\n"
        f"This PR was opened as a draft because the confidence tier requires "
        f"human review before landing in the team KB. Squash-merge when ready."
    )


def _extract_pr_url(stdout: str) -> Optional[str]:
    """``gh pr create`` prints the URL on its own line. Grab the first https:// match."""
    if not stdout:
        return None
    for line in stdout.splitlines():
        line = line.strip()
        if line.startswith("https://"):
            return line
    return None


def _coerce_yaml_scalar(value):
    """``yaml.safe_dump`` can't serialise ``datetime.date`` cleanly when embedded
    in a ``queued_at`` field that's already a string. Keep dates as ISO strings.
    """
    if hasattr(value, "isoformat"):
        return value.isoformat()
    return value
