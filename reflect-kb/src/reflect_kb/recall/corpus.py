"""M7: Knowledge-corpus Q&A — a saved-filter abstraction over the learnings KB.

reflect-kb is search-shaped: every recall is a fresh hybrid query. A *corpus*
is the complementary pattern — a long-lived, filtered SLICE of the user's own
code history that an agent can hold open and ask a question-set against ("ask
the auth subsystem", "ask the migration log").

A corpus is built by snapshotting every learning whose frontmatter matches a
deterministic FILTER (tag / category / project / date-window) into one JSON
document under ``$REFLECT_STATE_DIR/corpora/<name>.json`` (default
``~/.reflect/corpora/``). The snapshot persists the filter plus a last-built
timestamp and the KB mtime it was built against, so:

  * the selection is fully reproducible from seeds + filter (no LLM involved —
    the build/filter/snapshot/reprime path is the deterministic unit under
    test);
  * the corpus survives a process restart (re-read from disk);
  * ``rebuild_corpus`` re-runs the saved filter, dropping learnings that no
    longer match (or were deleted) and pulling in newly-matching ones;
  * a KB write (mtime change) marks the corpus STALE so the holding agent
    knows to reprime.

The conversational Q&A itself is NOT done here. In reflect's no-SDK
architecture the calling agent IS the Q&A session: ``/reflect:corpus`` builds
the snapshot and instructs the agent to read it and answer over it. This
module only owns the deterministic corpus lifecycle.

Mirrors claude-mem's CorpusBuilder (filter -> snapshot) and the prime/query
lifecycle of its KnowledgeAgent, adapted to reflect's file-backed KB.
"""

from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass, field
from datetime import date, datetime
from pathlib import Path
from typing import Any

import yaml

# --- KB layout ------------------------------------------------------------
# Learnings live as `<name>.md` files with YAML frontmatter under the KB's
# `documents/` dir. This is the same layout recall.py's corpus-scan arms read
# (see recall.py: documents_root / QMD_DOCS_ROOT) and the shape conftest.py
# seeds. We read the files directly: the filter is pure frontmatter logic, so
# no embedding model / graph / subprocess is needed.
ARCHIVE_HEADER_RE = re.compile(r"<!--\s*archived:\s*([0-9T:.+\-Z]+)\s*-->")

SNAPSHOT_VERSION = 1


# --- Paths ----------------------------------------------------------------

def state_dir() -> Path:
    """Root for reflect's mutable state. Overridable for tests via env."""
    return Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))


def corpora_dir() -> Path:
    """Directory holding the persisted corpus snapshots."""
    d = state_dir() / "corpora"
    d.mkdir(parents=True, exist_ok=True)
    return d


def corpus_path(name: str) -> Path:
    """Snapshot file for a named corpus: ``<state>/corpora/<name>.json``."""
    return corpora_dir() / f"{_safe_name(name)}.json"


def documents_root() -> Path:
    """The KB ``documents/`` dir whose `.md` learnings the filter scans.

    Honours ``$GLOBAL_LEARNINGS_PATH`` (the harness/test contract recall.py
    also reads) so a corpus build inside a hermetic test KB sees exactly the
    seeded learnings — never the real global KB.
    """
    override = os.environ.get("GLOBAL_LEARNINGS_PATH")
    if override:
        return Path(override) / "documents"
    return Path.home() / ".learnings" / "documents"


def _safe_name(name: str) -> str:
    """Restrict a corpus name to a single filesystem-safe path component."""
    cleaned = re.sub(r"[^A-Za-z0-9._-]+", "-", name.strip()).strip("-._")
    return cleaned or "corpus"


# --- Filter ---------------------------------------------------------------

@dataclass(frozen=True)
class CorpusFilter:
    """A saved, deterministic selection over learning frontmatter.

    Every supplied predicate must hold (logical AND) for a learning to be IN
    the corpus. An unset predicate is ignored. ``tags`` matches if the
    learning carries *any* of the requested tags (case-insensitive). The date
    window bounds the learning's ``created`` (or, when present, its archived
    timestamp) date inclusively.
    """

    tags: tuple[str, ...] = ()
    category: str | None = None
    project: str | None = None
    since: str | None = None  # ISO date (YYYY-MM-DD), inclusive lower bound
    until: str | None = None  # ISO date (YYYY-MM-DD), inclusive upper bound

    def to_dict(self) -> dict[str, Any]:
        return {
            "tags": list(self.tags),
            "category": self.category,
            "project": self.project,
            "since": self.since,
            "until": self.until,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "CorpusFilter":
        return cls(
            tags=tuple(d.get("tags") or ()),
            category=d.get("category"),
            project=d.get("project"),
            since=d.get("since"),
            until=d.get("until"),
        )

    def matches(self, fm: dict[str, Any], doc_date: date | None) -> bool:
        """True iff a learning's frontmatter + resolved date satisfy the filter."""
        if self.category is not None:
            if str(fm.get("category", "")).strip().lower() != self.category.strip().lower():
                return False
        if self.project is not None:
            proj = str(fm.get("project_id") or fm.get("project") or "").strip()
            if proj.lower() != self.project.strip().lower():
                return False
        if self.tags:
            have = {t.lower() for t in _tags_of(fm)}
            want = {t.strip().lower() for t in self.tags}
            if not (have & want):
                return False
        if self.since or self.until:
            if doc_date is None:
                return False
            if self.since and doc_date < _parse_date(self.since):
                return False
            if self.until and doc_date > _parse_date(self.until):
                return False
        return True


def _tags_of(fm: dict[str, Any]) -> list[str]:
    raw = fm.get("tags") or []
    if isinstance(raw, str):
        raw = [t.strip() for t in re.split(r"[\[\],]", raw) if t.strip()]
    return [str(t).strip() for t in raw]


def _parse_date(value: str) -> date:
    """Parse an ISO date or datetime string to a ``date`` (date-only compare)."""
    s = str(value).strip().strip('"').rstrip("Z")
    try:
        return datetime.fromisoformat(s).date()
    except ValueError:
        return datetime.strptime(s[:10], "%Y-%m-%d").date()


# --- Learning scan --------------------------------------------------------

@dataclass
class CorpusEntry:
    """One learning admitted to the corpus — the priming context per note."""

    id: str
    title: str
    category: str
    tags: list[str]
    project: str | None
    created: str | None
    key_insight: str
    body: str

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "title": self.title,
            "category": self.category,
            "tags": self.tags,
            "project": self.project,
            "created": self.created,
            "key_insight": self.key_insight,
            "body": self.body,
        }


def _parse_frontmatter(text: str) -> tuple[dict[str, Any], str]:
    """Extract YAML frontmatter; return (dict, body). Same shape as recall.py."""
    if not text.startswith("---"):
        return {}, text
    end = text.find("\n---", 3)
    if end == -1:
        return {}, text
    header = text[3:end].strip()
    body = text[end + 4:].lstrip("\n")
    try:
        data = yaml.safe_load(header) or {}
        return (data if isinstance(data, dict) else {}), body
    except yaml.YAMLError:
        return {}, body


def _doc_date(fm: dict[str, Any], archived_at: str | None) -> date | None:
    """The date used for window filtering: archived timestamp wins, else created."""
    for raw in (archived_at, fm.get("created")):
        if not raw:
            continue
        try:
            return _parse_date(str(raw))
        except (ValueError, TypeError):
            continue
    return None


def _entry_from_doc(fm: dict[str, Any], body: str, fallback_id: str) -> CorpusEntry:
    name = str(fm.get("id") or fm.get("name") or fallback_id)
    return CorpusEntry(
        id=name,
        title=str(fm.get("title") or fm.get("name") or fallback_id).strip().strip('"'),
        category=str(fm.get("category") or "general").strip(),
        tags=_tags_of(fm),
        project=(str(fm.get("project_id") or fm.get("project")).strip()
                 if (fm.get("project_id") or fm.get("project")) else None),
        created=(str(fm.get("created")).strip().strip('"') if fm.get("created") else None),
        key_insight=str(fm.get("key_insight") or "").strip().strip('"'),
        body=body.strip(),
    )


def select_learnings(filt: CorpusFilter, docs_root: Path | None = None) -> list[CorpusEntry]:
    """Scan the KB ``documents/`` dir and return every learning matching the filter.

    Deterministic: the seeds on disk plus the filter fully determine the
    result. Entries are returned sorted by id for snapshot stability.
    """
    root = docs_root or documents_root()
    out: list[CorpusEntry] = []
    if not root.exists():
        return out
    for md in sorted(root.glob("*.md")):
        try:
            text = md.read_text()
        except OSError:
            continue
        fm, body = _parse_frontmatter(text)
        am = ARCHIVE_HEADER_RE.search(text)
        archived_at = am.group(1) if am else None
        if filt.matches(fm, _doc_date(fm, archived_at)):
            out.append(_entry_from_doc(fm, body, md.stem))
    out.sort(key=lambda e: e.id)
    return out


# --- Snapshot model -------------------------------------------------------

@dataclass
class Corpus:
    """A persisted, primeable corpus snapshot."""

    name: str
    filt: CorpusFilter
    entries: list[CorpusEntry] = field(default_factory=list)
    built_at: str = ""
    kb_mtime: float = 0.0
    version: int = SNAPSHOT_VERSION

    @property
    def ids(self) -> list[str]:
        return [e.id for e in self.entries]

    def to_dict(self) -> dict[str, Any]:
        return {
            "version": self.version,
            "name": self.name,
            "filter": self.filt.to_dict(),
            "built_at": self.built_at,
            "kb_mtime": self.kb_mtime,
            "count": len(self.entries),
            "entries": [e.to_dict() for e in self.entries],
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Corpus":
        entries = [
            CorpusEntry(
                id=e["id"], title=e.get("title", ""), category=e.get("category", "general"),
                tags=list(e.get("tags") or []), project=e.get("project"),
                created=e.get("created"), key_insight=e.get("key_insight", ""),
                body=e.get("body", ""),
            )
            for e in d.get("entries", [])
        ]
        return cls(
            name=d["name"],
            filt=CorpusFilter.from_dict(d.get("filter") or {}),
            entries=entries,
            built_at=d.get("built_at", ""),
            kb_mtime=float(d.get("kb_mtime", 0.0)),
            version=int(d.get("version", SNAPSHOT_VERSION)),
        )

    def prime_document(self) -> str:
        """Render the ONE primed context document an agent reads to answer Q&A.

        This is the prime() half of the prime+query lifecycle: a single
        markdown digest of every admitted learning. No LLM — just the
        deterministic assembly of the filtered slice.
        """
        lines = [
            f"# Corpus: {self.name}",
            "",
            f"Filter: {json.dumps(self.filt.to_dict(), sort_keys=True)}",
            f"Built: {self.built_at} · {len(self.entries)} learning(s)",
            "",
            "Answer questions using ONLY the learnings below.",
            "",
        ]
        for e in self.entries:
            tags = f" · tags: {', '.join(e.tags)}" if e.tags else ""
            proj = f" · project: {e.project}" if e.project else ""
            lines.append(f"## {e.title}  (`{e.id}`)")
            lines.append(f"category: {e.category}{proj}{tags}")
            if e.key_insight:
                lines.append(f"key insight: {e.key_insight}")
            if e.body:
                lines.append("")
                lines.append(e.body)
            lines.append("")
        return "\n".join(lines)


# --- KB freshness ---------------------------------------------------------

def kb_mtime(docs_root: Path | None = None) -> float:
    """Newest mtime across the KB ``documents/`` dir — the reprime trigger.

    A learning written, changed or removed shifts this value, which is how
    ``is_stale`` detects KB drift since the corpus was last built.
    """
    root = docs_root or documents_root()
    if not root.exists():
        return 0.0
    newest = 0.0
    try:
        newest = root.stat().st_mtime
    except OSError:
        pass
    for md in root.glob("*.md"):
        try:
            newest = max(newest, md.stat().st_mtime)
        except OSError:
            continue
    return newest


def is_stale(corpus: Corpus, docs_root: Path | None = None) -> bool:
    """True when the KB has been written since the corpus snapshot was built."""
    return kb_mtime(docs_root) > corpus.kb_mtime


# --- Build / persist / load ----------------------------------------------

def build_corpus(
    name: str,
    filt: CorpusFilter,
    docs_root: Path | None = None,
) -> Corpus:
    """Snapshot every learning matching ``filt`` into ``corpora/<name>.json``.

    The selection is exactly ``select_learnings(filt)`` — a matching learning
    is IN, a non-matching one is OUT. The filter and a last-built timestamp +
    KB mtime are persisted so the corpus survives a process restart and can be
    rebuilt / checked for staleness later.
    """
    root = docs_root or documents_root()
    entries = select_learnings(filt, root)
    corpus = Corpus(
        name=name,
        filt=filt,
        entries=entries,
        built_at=datetime.now().isoformat(timespec="seconds"),
        kb_mtime=kb_mtime(root),
    )
    save_corpus(corpus)
    return corpus


def rebuild_corpus(name: str, docs_root: Path | None = None) -> Corpus:
    """Re-run a saved corpus's filter against the current KB.

    Re-priming: newly-matching learnings are pulled in and entries that no
    longer match (or were deleted) are discarded. The persisted filter is the
    source of truth, so this works after a process restart with no extra args.
    """
    existing = load_corpus(name)
    if existing is None:
        raise FileNotFoundError(f"no corpus named {name!r} at {corpus_path(name)}")
    return build_corpus(name, existing.filt, docs_root)


def save_corpus(corpus: Corpus) -> Path:
    path = corpus_path(corpus.name)
    path.write_text(json.dumps(corpus.to_dict(), indent=2, sort_keys=False))
    return path


def load_corpus(name: str) -> Corpus | None:
    """Read a persisted corpus from disk. Returns None when it does not exist."""
    path = corpus_path(name)
    if not path.exists():
        return None
    try:
        return Corpus.from_dict(json.loads(path.read_text()))
    except (json.JSONDecodeError, OSError, KeyError):
        return None


def list_corpora() -> list[str]:
    """Names of every persisted corpus."""
    d = state_dir() / "corpora"
    if not d.exists():
        return []
    return sorted(p.stem for p in d.glob("*.json"))


# --- CLI-friendly filter parsing -----------------------------------------

def parse_filter_spec(spec: str) -> CorpusFilter:
    """Parse a ``key:value`` filter spec string into a CorpusFilter.

    Accepts space-separated ``tag:auth category:security project:api
    since:2026-01-01 until:2026-06-30`` tokens. Bare tokens (no recognised
    key) are treated as tags. ``tag:`` may be repeated to OR several tags.
    """
    tags: list[str] = []
    category = project = since = until = None
    for tok in spec.split():
        if ":" in tok:
            key, _, val = tok.partition(":")
            key = key.strip().lower()
            val = val.strip()
            if key in ("tag", "tags"):
                tags.extend(t for t in val.split(",") if t)
            elif key in ("category", "cat"):
                category = val or None
            elif key in ("project", "proj", "project_id"):
                project = val or None
            elif key == "since":
                since = val or None
            elif key == "until":
                until = val or None
            else:
                tags.append(tok)
        elif tok:
            tags.append(tok)
    # Validate the date bounds eagerly so a malformed since:/until: surfaces as
    # one clean ValueError here, not a raw traceback deep inside per-doc
    # matching (matches() calls _parse_date on every candidate).
    for label, bound in (("since", since), ("until", until)):
        if bound:
            try:
                _parse_date(bound)
            except (ValueError, TypeError):
                raise ValueError(
                    f"invalid {label} date {bound!r} — use YYYY-MM-DD"
                ) from None
    return CorpusFilter(
        tags=tuple(tags), category=category, project=project,
        since=since, until=until,
    )
