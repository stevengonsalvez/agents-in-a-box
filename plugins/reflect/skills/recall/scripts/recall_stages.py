#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "pyyaml",
# ]
# ///
"""
Reflect staged recall — enforced 3-layer search workflow (port M1, claude-mem).

Single-shot recall either dumps too much context or forces the caller to
write its own query plan. This script herds callers into a deterministic,
token-cheap pipeline:

    Step 0 (bootstrap): `workflow`            — the staged-recall contract
    Step 1:             `index <query>`       — ID-only rows (~50-100 tok/result)
    Step 2:             `timeline ...`        — chronological neighbours of an anchor
    Step 3:             `hydrate <id> [...]`  — full bodies + entity sidecars

Never hydrate without filtering through Steps 1-2 first (~10x token savings).

Exit codes:
    0 = success (including "not found" — errors are reported in the JSON
        payload, matching recall.py's silent-no-op discipline)
    2 = invalid args
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

import yaml  # declared in PEP 723 header; uv run --script always installs

# recall.py lives in the same directory; reuse its retrieval machinery
# (hybrid recall, frontmatter parsing, lexical overlap, token estimate).
sys.path.insert(0, str(Path(__file__).resolve().parent))
import recall as recall_mod  # noqa: E402


# --- Tool contract --------------------------------------------------------

# M1: the literal 'Step N:' prefixes are load-bearing — any LLM client that
# surfaces these descriptions (CLI --help, MCP tool listing, skill docs)
# sees the staged order without extra prompt engineering. Mirrors
# claude-mem's __IMPORTANT / search / timeline / get_observations surface.
TOOL_DESCRIPTIONS: dict[str, str] = {
    "reflect_workflow": (
        "3-LAYER WORKFLOW (ALWAYS FOLLOW): "
        "1. index(query) -> ID-only rows (~50-100 tokens/result). "
        "2. timeline(anchor=ID) -> chronological context around interesting results. "
        "3. hydrate([IDs]) -> full details ONLY for filtered IDs. "
        "NEVER hydrate without filtering first. 10x token savings."
    ),
    "reflect_index": (
        "Step 1: Search the KB. Returns a compact index of ID-only rows "
        "(id, title, score, project, date — ~50-100 tokens/result). "
        "Params: query, limit"
    ),
    "reflect_timeline": (
        "Step 2: Get chronological context around a result. "
        "Params: anchor (learning ID) OR query (finds the anchor "
        "automatically), depth_before, depth_after"
    ),
    "reflect_hydrate": (
        "Step 3: Fetch full learning bodies + entity sidecars for the "
        "filtered IDs only (~500-1000 tokens/result). ALWAYS batch 2+ ids. "
        "Params: ids (required)"
    ),
}

WORKFLOW_CONTRACT = """# Reflect Staged Recall Workflow

**3-Layer Pattern (ALWAYS follow this):**

1. **Index** — get a compact index of results with IDs
   `recall_stages.py index "<query>" --limit 20`
   Returns: ID-only rows `{id, title, score, project, date}` (~50-100 tokens/result)

2. **Timeline** — get chronological context around interesting results
   `recall_stages.py timeline --anchor <ID> --depth-before 3 --depth-after 3`
   (or `recall_stages.py timeline "<query>"` to find the anchor automatically)
   Returns: chronological neighbours showing what was happening around the anchor

3. **Hydrate** — fetch full details ONLY for the filtered IDs
   `recall_stages.py hydrate <ID> [<ID> ...]`  # ALWAYS batch for 2+ items
   Returns: full learning bodies + entity sidecars (~500-1000 tokens/result)

**Why:** ~10x token savings. Never hydrate full details without filtering first.
"""

DEFAULT_INDEX_LIMIT = 20
DEFAULT_DEPTH = 3
TITLE_MAX_CHARS = 120
# Acceptance: each index row must cost <= 100 estimated tokens.
ROW_TOKEN_CAP = 100


# --- KB document access ----------------------------------------------------

@dataclass
class DocRecord:
    """One learning document on disk, with enough metadata for timeline rows."""

    doc_id: str
    title: str
    project: str
    date: str  # ISO string ("" when unknown)
    sort_key: float  # epoch seconds for chronological ordering
    path: Path
    frontmatter: dict[str, Any]
    body: str


def docs_root() -> Path | None:
    """Resolve the learnings documents directory.

    Resolution order mirrors the engine (learnings_cli.get_repo_path):
      1. $GLOBAL_LEARNINGS_PATH/documents
      2. ~/.claude/global-learnings/documents (engine default)
      3. ~/.learnings/documents (QMD root, pre-migration layout)
    Returns None when none exists — callers surface a graceful empty result.
    """
    env_path = os.environ.get("GLOBAL_LEARNINGS_PATH")
    if env_path:
        candidate = Path(env_path) / "documents"
        return candidate if candidate.is_dir() else None
    for candidate in (
        Path.home() / ".claude" / "global-learnings" / "documents",
        Path.home() / ".learnings" / "documents",
    ):
        if candidate.is_dir():
            return candidate
    return None


def _parse_date(raw: Any, fallback_mtime: float) -> tuple[str, float]:
    """Normalise a frontmatter date to (iso_string, epoch_sort_key).

    Tolerates ISO strings (with or without trailing Z), date/datetime
    objects from yaml, and junk (falls back to file mtime so every doc
    still sorts deterministically).
    """
    if isinstance(raw, datetime):
        return raw.isoformat(), raw.timestamp()
    if hasattr(raw, "isoformat") and raw is not None:  # datetime.date
        try:
            dt = datetime.fromisoformat(raw.isoformat())
            return raw.isoformat(), dt.timestamp()
        except (ValueError, TypeError):
            pass
    if isinstance(raw, str) and raw.strip():
        text = raw.strip()
        try:
            dt = datetime.fromisoformat(text.replace("Z", "+00:00"))
            return text, dt.timestamp()
        except ValueError:
            pass
    return "", fallback_mtime


def _doc_date(fm: dict[str, Any], body: str, path: Path) -> tuple[str, float]:
    """Best-effort document date: frontmatter created/date/archived header/mtime."""
    try:
        mtime = path.stat().st_mtime
    except OSError:
        mtime = 0.0
    for key in ("created", "date", "updated"):
        if fm.get(key) is not None:
            iso, sort_key = _parse_date(fm.get(key), mtime)
            if iso:
                return iso, sort_key
    m = recall_mod.ARCHIVE_HEADER_RE.search(body)
    if m:
        iso, sort_key = _parse_date(m.group(1), mtime)
        if iso:
            return iso, sort_key
    return "", mtime


def load_documents(root: Path | None = None) -> list[DocRecord]:
    """Scan the documents dir into chronologically sorted DocRecords.

    Unreadable / frontmatter-less files are skipped silently — this script
    must never crash recall over one corrupt note.
    """
    root = root or docs_root()
    if root is None:
        return []
    records: list[DocRecord] = []
    for path in sorted(root.glob("*.md")):
        try:
            content = path.read_text()
        except OSError:
            continue
        fm, body = recall_mod.parse_frontmatter(content)
        doc_id = str(fm.get("id") or fm.get("name") or path.stem)
        title = str(fm.get("title") or fm.get("name") or path.stem).strip().strip('"')
        project = str(
            fm.get("project") or fm.get("category") or fm.get("agent") or ""
        )
        date_iso, sort_key = _doc_date(fm, body, path)
        records.append(
            DocRecord(
                doc_id=doc_id,
                title=title,
                project=project,
                date=date_iso,
                sort_key=sort_key,
                path=path,
                frontmatter=fm,
                body=body,
            )
        )
    records.sort(key=lambda r: (r.sort_key, r.doc_id))
    return records


# --- Step 1: index ---------------------------------------------------------

def _index_row(
    doc_id: str, title: str, score: float, project: str, date: str
) -> dict[str, Any]:
    """One compact index row, hard-capped at ROW_TOKEN_CAP estimated tokens."""
    row = {
        "id": doc_id[:120],
        "title": (title or "")[:TITLE_MAX_CHARS],
        "score": round(float(score), 4),
        "project": (project or "")[:40],
        "date": (date or "")[:25],
    }
    # Defense in depth: if a pathological id/title still blows the cap,
    # shave the title until the serialized row fits.
    while (
        recall_mod._est_tokens(json.dumps(row)) > ROW_TOKEN_CAP and row["title"]
    ):
        row["title"] = row["title"][: max(0, len(row["title"]) - 20)]
    return row


def _learning_meta(lrn: recall_mod.Learning) -> tuple[str, str]:
    """(project, date) for a recall.Learning, mirroring DocRecord fields."""
    fm = lrn.frontmatter
    project = str(fm.get("project") or fm.get("category") or fm.get("agent") or "")
    raw_date = fm.get("created") or fm.get("date") or lrn.archived_at
    date_iso, _ = _parse_date(raw_date, 0.0)
    return project, date_iso


def _lexical_index(query: str, limit: int) -> list[dict[str, Any]]:
    """Fallback Step 1 when the reflect CLI is unavailable: rank the local
    documents by stdlib query-term coverage (recall.lexical_overlap)."""
    rows: list[tuple[float, DocRecord]] = []
    for doc in load_documents():
        lrn = recall_mod.Learning(chunk_text=doc.body, frontmatter=doc.frontmatter)
        score = recall_mod.lexical_overlap(query, lrn)
        if score > 0.0:
            rows.append((score, doc))
    rows.sort(key=lambda pair: (-pair[0], pair[1].doc_id))
    return [
        _index_row(doc.doc_id, doc.title, score, doc.project, doc.date)
        for score, doc in rows[:limit]
    ]


def reflect_index(query: str, limit: int = DEFAULT_INDEX_LIMIT) -> dict[str, Any]:
    """Step 1: compact ID-only index of search results.

    Wraps the hybrid recall pipeline; each row is {id, title, score,
    project, date} only (~50-100 tokens). Falls back to a local lexical
    scan when the reflect CLI is missing so the staged workflow degrades
    instead of breaking.
    """
    result = recall_mod.recall(query, limit=limit)
    if result.error or not result.learnings:
        rows = _lexical_index(query, limit)
        return {"step": 1, "query": query, "count": len(rows), "results": rows}
    rows = []
    for lrn in result.learnings:
        key = recall_mod._learning_key(lrn)
        project, date_iso = _learning_meta(lrn)
        rows.append(
            _index_row(
                lrn.id, lrn.title, result.scores.get(key, 0.0), project, date_iso
            )
        )
    return {"step": 1, "query": query, "count": len(rows), "results": rows}


# --- Step 2: timeline -------------------------------------------------------

def _resolve_anchor(
    docs: list[DocRecord], anchor_id: str | None, query: str | None
) -> int | None:
    """Index of the anchor document, by exact ID, file stem, or query."""
    if anchor_id:
        for i, doc in enumerate(docs):
            if doc.doc_id == anchor_id or doc.path.stem == anchor_id:
                return i
        return None
    if query:
        # Reuse Step 1 so the anchor matches what the caller just saw in the
        # index; fall back to best lexical doc if the top hit is not a local
        # document (e.g. a chunk id from the engine).
        index = reflect_index(query, limit=5)
        for row in index["results"]:
            for i, doc in enumerate(docs):
                if doc.doc_id == row["id"] or doc.path.stem == row["id"]:
                    return i
        best_i, best_score = None, 0.0
        for i, doc in enumerate(docs):
            lrn = recall_mod.Learning(chunk_text=doc.body, frontmatter=doc.frontmatter)
            score = recall_mod.lexical_overlap(query, lrn)
            if score > best_score:
                best_i, best_score = i, score
        return best_i
    return None


def reflect_timeline(
    anchor_id: str | None = None,
    query: str | None = None,
    depth_before: int = DEFAULT_DEPTH,
    depth_after: int = DEFAULT_DEPTH,
) -> dict[str, Any]:
    """Step 2: chronological neighbours around an anchor learning.

    Accepts either an explicit anchor ID or a free-text query (the anchor is
    then resolved through Step 1). Returns compact rows bounded by
    depth_before/depth_after, anchor flagged.
    """
    depth_before = max(0, depth_before)
    depth_after = max(0, depth_after)
    docs = load_documents()
    if not docs:
        return {"step": 2, "error": "no learnings KB found", "results": []}
    anchor_i = _resolve_anchor(docs, anchor_id, query)
    if anchor_i is None:
        return {
            "step": 2,
            "error": f"anchor not found ({anchor_id or query!r})",
            "results": [],
        }
    lo = max(0, anchor_i - depth_before)
    hi = min(len(docs), anchor_i + depth_after + 1)
    rows = []
    for i in range(lo, hi):
        doc = docs[i]
        row = _index_row(doc.doc_id, doc.title, 0.0, doc.project, doc.date)
        del row["score"]  # timeline rows are positional, not scored
        row["anchor"] = i == anchor_i
        rows.append(row)
    return {
        "step": 2,
        "anchor": docs[anchor_i].doc_id,
        "depth_before": depth_before,
        "depth_after": depth_after,
        "results": rows,
    }


# --- Step 3: hydrate --------------------------------------------------------

def _read_sidecar(doc_path: Path) -> dict[str, Any] | None:
    """Parse the sibling .entities.yaml sidecar; None when absent/corrupt."""
    sidecar = doc_path.with_name(doc_path.stem + ".entities.yaml")
    if not sidecar.is_file():
        return None
    try:
        data = yaml.safe_load(sidecar.read_text())
    except (OSError, yaml.YAMLError):
        return None
    return data if isinstance(data, dict) else None


def reflect_hydrate(ids: list[str]) -> dict[str, Any]:
    """Step 3: full learning bodies + entity sidecars for the filtered IDs.

    Unknown IDs come back as {found: false} rows rather than failing the
    whole batch — partial hydration is still useful to the caller.
    """
    docs = load_documents()
    by_id: dict[str, DocRecord] = {}
    for doc in docs:
        by_id.setdefault(doc.doc_id, doc)
        by_id.setdefault(doc.path.stem, doc)
    results = []
    for doc_id in ids:
        doc = by_id.get(doc_id)
        if doc is None:
            results.append({"id": doc_id, "found": False})
            continue
        results.append(
            {
                "id": doc.doc_id,
                "found": True,
                "title": doc.title,
                "project": doc.project,
                "date": doc.date,
                "frontmatter": doc.frontmatter,
                "body": doc.body,
                "entities": _read_sidecar(doc.path),
            }
        )
    return {"step": 3, "count": len(results), "results": results}


# --- Step 0: workflow bootstrap ----------------------------------------------

def reflect_workflow() -> str:
    """Bootstrap tool: the staged-recall contract (claude-mem __IMPORTANT)."""
    return WORKFLOW_CONTRACT


# --- CLI ---------------------------------------------------------------------

def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=TOOL_DESCRIPTIONS["reflect_workflow"],
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = ap.add_subparsers(dest="command", required=True)

    sub.add_parser(
        "workflow",
        help=TOOL_DESCRIPTIONS["reflect_workflow"],
        description=TOOL_DESCRIPTIONS["reflect_workflow"],
    )

    p_index = sub.add_parser(
        "index",
        help=TOOL_DESCRIPTIONS["reflect_index"],
        description=TOOL_DESCRIPTIONS["reflect_index"],
    )
    p_index.add_argument("query", nargs="+", help="Search query")
    p_index.add_argument("--limit", type=int, default=DEFAULT_INDEX_LIMIT)

    p_timeline = sub.add_parser(
        "timeline",
        help=TOOL_DESCRIPTIONS["reflect_timeline"],
        description=TOOL_DESCRIPTIONS["reflect_timeline"],
    )
    p_timeline.add_argument(
        "query", nargs="*", help="Free-text query to find the anchor automatically"
    )
    p_timeline.add_argument("--anchor", help="Anchor learning ID")
    p_timeline.add_argument("--depth-before", type=int, default=DEFAULT_DEPTH)
    p_timeline.add_argument("--depth-after", type=int, default=DEFAULT_DEPTH)

    p_hydrate = sub.add_parser(
        "hydrate",
        help=TOOL_DESCRIPTIONS["reflect_hydrate"],
        description=TOOL_DESCRIPTIONS["reflect_hydrate"],
    )
    p_hydrate.add_argument("ids", nargs="+", help="Learning IDs to hydrate")

    args = ap.parse_args(argv)

    if args.command == "workflow":
        print(reflect_workflow())
        return 0

    if args.command == "index":
        query = " ".join(args.query).strip()
        if not query:
            print("error: empty query", file=sys.stderr)
            return 2
        print(json.dumps(reflect_index(query, limit=args.limit), indent=2, default=str))
        return 0

    if args.command == "timeline":
        query = " ".join(args.query).strip() or None
        if not args.anchor and not query:
            print("error: provide an anchor ID or a query", file=sys.stderr)
            return 2
        payload = reflect_timeline(
            anchor_id=args.anchor,
            query=query,
            depth_before=args.depth_before,
            depth_after=args.depth_after,
        )
        print(json.dumps(payload, indent=2, default=str))
        return 0

    if args.command == "hydrate":
        print(json.dumps(reflect_hydrate(args.ids), indent=2, default=str))
        return 0

    return 2  # unreachable — argparse enforces the subcommand set


if __name__ == "__main__":
    sys.exit(main())
