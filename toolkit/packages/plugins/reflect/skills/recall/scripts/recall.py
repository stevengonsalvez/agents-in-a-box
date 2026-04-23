#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "pyyaml",
# ]
# ///
"""
Reflect Recall — hybrid retrieval from the global learnings KB.

Wraps ~/.learnings/cli/learnings as a subprocess so we inherit GraphRAG +
embeddings without pulling the nano-graphrag dep chain into this plugin.

Usage:
    recall.py <query> [--limit N] [--mode naive|local|global]
                      [--confidence HIGH|MEDIUM|LOW|ANY]
                      [--format markdown|json]
                      [--max-chars 2000]
                      [--no-cache]
                      [--cache-ttl 3600]

Exit codes:
    0 = success (including empty results when KB absent — see D9)
    2 = invalid args
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

import yaml  # declared in PEP 723 header; uv run --script always installs


# --- Config --------------------------------------------------------------

DEFAULT_LIMIT = 10
DEFAULT_MODE = "naive"
DEFAULT_CACHE_TTL = 3600  # 1 hour
DEFAULT_MAX_CHARS = 2000
LEARNINGS_CLI_CANDIDATES = [
    Path.home() / ".learnings" / "cli" / "learnings",
    Path("/opt/homebrew/bin/learnings"),
]

CONFIDENCE_WEIGHTS = {"HIGH": 1.0, "MEDIUM": 0.7, "LOW": 0.4}
CHUNK_SEPARATOR = "--New Chunk--"
ARCHIVE_HEADER_RE = re.compile(r"<!--\s*archived:\s*([0-9T:\-Z]+)\s*-->")


# --- Data models ---------------------------------------------------------

@dataclass
class Learning:
    """One parsed chunk from the learnings search output."""

    chunk_text: str
    frontmatter: dict[str, Any] = field(default_factory=dict)
    archived_at: str | None = None  # ISO timestamp from the <!-- archived --> comment

    @property
    def id(self) -> str:
        return self.frontmatter.get("id") or self.frontmatter.get("name") or "?"

    @property
    def title(self) -> str:
        return (
            self.frontmatter.get("title")
            or self.frontmatter.get("name")
            or "(no title)"
        ).strip().strip('"')

    @property
    def key_insight(self) -> str:
        return (self.frontmatter.get("key_insight") or "").strip().strip('"')

    @property
    def confidence(self) -> str:
        raw = self.frontmatter.get("confidence") or "MEDIUM"
        # Coerce numeric confidence (instinct-style 0.0-1.0) to tier
        if isinstance(raw, (int, float)):
            if raw >= 0.8:
                return "HIGH"
            if raw >= 0.5:
                return "MEDIUM"
            return "LOW"
        return str(raw).upper()

    @property
    def tags(self) -> list[str]:
        raw = self.frontmatter.get("tags", [])
        if isinstance(raw, str):
            # yaml sometimes leaves unquoted lists as strings; split tolerantly
            raw = [t.strip() for t in re.split(r"[\[\],]", raw) if t.strip()]
        return [str(t).strip() for t in raw]

    @property
    def how_to_apply(self) -> str:
        """Extract the "How to apply:" paragraph from the chunk body."""
        m = re.search(
            r"\*\*How to apply:\*\*\s*\n?(.*?)(?=\n\n|\n\*\*|\Z)",
            self.chunk_text,
            re.DOTALL,
        )
        if m:
            text = m.group(1).strip()
            # Cap at one sentence / 280 chars for SessionStart brevity
            text = text.split("\n")[0]
            return text[:280]
        return ""


@dataclass
class RecallResult:
    learnings: list[Learning]
    query: str
    mode: str
    cache_hit: bool = False
    error: str | None = None


# --- Helpers -------------------------------------------------------------

def find_learnings_cli() -> Path | None:
    """Locate the learnings CLI. D1: subprocess wrapper."""
    for candidate in LEARNINGS_CLI_CANDIDATES:
        if candidate.exists() and os.access(candidate, os.X_OK):
            return candidate
    cli_on_path = shutil.which("learnings")
    return Path(cli_on_path) if cli_on_path else None


def cache_path(query: str, mode: str) -> Path:
    """Per-query cache file. D4: 1-hour TTL."""
    digest = hashlib.sha1(f"{query}|{mode}".encode()).hexdigest()[:16]
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    cache_dir = base / "recall_cache"
    cache_dir.mkdir(parents=True, exist_ok=True)
    return cache_dir / f"{digest}.json"


def kb_last_modified() -> float:
    """mtime of the GraphRAG cache dir — proxy for last KB write."""
    kb = Path.home() / ".learnings" / "nano_graphrag_cache"
    try:
        return kb.stat().st_mtime if kb.exists() else 0.0
    except OSError:
        return 0.0


def read_cache(path: Path, ttl: int) -> dict | None:
    if not path.exists():
        return None
    cache_mtime = path.stat().st_mtime
    # Invalidate on TTL or when KB has been written since the cache was created
    if time.time() - cache_mtime > ttl or kb_last_modified() > cache_mtime:
        return None
    try:
        return json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return None


def write_cache(path: Path, payload: dict) -> None:
    try:
        path.write_text(json.dumps(payload, default=str))
    except OSError:
        pass  # non-fatal


def parse_frontmatter(text: str) -> tuple[dict[str, Any], str]:
    """Extract YAML frontmatter if present; return (dict, remaining_body)."""
    if not text.startswith("---"):
        return {}, text
    end = text.find("\n---", 3)
    if end == -1:
        return {}, text
    header = text[3:end].strip()
    body = text[end + 4 :].lstrip("\n")
    try:
        data = yaml.safe_load(header) or {}
        return (data if isinstance(data, dict) else {}), body
    except yaml.YAMLError:
        return {}, body


def parse_learnings_output(json_blob: str) -> list[Learning]:
    """Split a `learnings search --format json` response into Learning objects."""
    try:
        envelope = json.loads(json_blob)
    except json.JSONDecodeError:
        return []
    context = envelope.get("context", "")
    if not context:
        return []
    chunks = [c.strip() for c in context.split(CHUNK_SEPARATOR) if c.strip()]
    results: list[Learning] = []
    for chunk in chunks:
        fm, body = parse_frontmatter(chunk)
        archived = None
        m = ARCHIVE_HEADER_RE.search(body)
        if m:
            archived = m.group(1)
        results.append(Learning(chunk_text=chunk, frontmatter=fm, archived_at=archived))
    return results


def rerank(
    learnings: list[Learning],
    query_tags: list[str] | None = None,
    now: datetime | None = None,
) -> list[Learning]:
    """
    D8: score = confidence × recency × (1 + tag_bonus).
    Sorts in-place and returns the same list.
    """
    now = now or datetime.now(tz=None)
    qt = set(t.lower() for t in (query_tags or []))

    def score(lrn: Learning) -> float:
        c = CONFIDENCE_WEIGHTS.get(lrn.confidence, 0.5)
        # Recency: half-life 60d via exp(-age / 90)
        recency = 1.0
        if lrn.archived_at:
            try:
                ts = datetime.fromisoformat(lrn.archived_at.rstrip("Z"))
                age_days = max(0.0, (now - ts).days)
                recency = math.exp(-age_days / 90.0)
            except ValueError:
                pass
        lt = set(t.lower() for t in lrn.tags)
        bonus = 0.1 * len(qt & lt) if qt else 0.0
        return c * recency * (1 + bonus)

    learnings.sort(key=score, reverse=True)
    return learnings


def filter_by_confidence(learnings: list[Learning], threshold: str) -> list[Learning]:
    """threshold ∈ {HIGH, MEDIUM, LOW, ANY}"""
    if threshold == "ANY":
        return learnings
    rank = {"HIGH": 3, "MEDIUM": 2, "LOW": 1}
    min_rank = rank.get(threshold, 0)
    return [l for l in learnings if rank.get(l.confidence, 0) >= min_rank]


def render_markdown(
    learnings: list[Learning], query: str, max_chars: int = DEFAULT_MAX_CHARS
) -> str:
    """D5: compact markdown block for agent context."""
    if not learnings:
        return ""
    lines = [f"## Prior learnings relevant to `{query[:80]}`\n"]
    used = len(lines[0])
    for lrn in learnings:
        header = f"- **[{lrn.id}]** {lrn.key_insight or lrn.title}"
        how = lrn.how_to_apply
        entry = header + (f"\n  How to apply: {how}" if how else "") + "\n"
        if used + len(entry) > max_chars:
            lines.append(f"- _(…{len(learnings) - (len(lines) - 1)} more truncated)_\n")
            break
        lines.append(entry)
        used += len(entry)
    return "".join(lines).rstrip() + "\n"


def render_json(learnings: list[Learning], query: str, mode: str) -> str:
    return json.dumps(
        {
            "query": query,
            "mode": mode,
            "count": len(learnings),
            "results": [
                {
                    "id": l.id,
                    "title": l.title,
                    "key_insight": l.key_insight,
                    "confidence": l.confidence,
                    "tags": l.tags,
                    "how_to_apply": l.how_to_apply,
                    "archived_at": l.archived_at,
                }
                for l in learnings
            ],
        },
        indent=2,
    )


def log_recall(query: str, mode: str, count: int, cached: bool) -> None:
    """D_phase6: append-only jsonl for future helpfulness tracking."""
    base = Path(os.environ.get("REFLECT_STATE_DIR", Path.home() / ".reflect"))
    log = base / "recall_log.jsonl"
    try:
        base.mkdir(parents=True, exist_ok=True)
        with log.open("a") as f:
            f.write(
                json.dumps(
                    {
                        "ts": datetime.now().isoformat(timespec="seconds"),
                        "query": query,
                        "mode": mode,
                        "count": count,
                        "cached": cached,
                    }
                )
                + "\n"
            )
    except OSError:
        pass


# --- Core entry ----------------------------------------------------------

def recall(
    query: str,
    *,
    limit: int = DEFAULT_LIMIT,
    mode: str = DEFAULT_MODE,
    confidence: str = "ANY",
    max_chars: int = DEFAULT_MAX_CHARS,
    use_cache: bool = True,
    cache_ttl: int = DEFAULT_CACHE_TTL,
    query_tags: list[str] | None = None,
) -> RecallResult:
    """High-level API: query → ranked Learnings. Never raises on KB issues."""
    cli = find_learnings_cli()
    if not cli:
        return RecallResult([], query, mode, error="learnings CLI not found")

    cache_file = cache_path(query, mode)
    if use_cache:
        cached = read_cache(cache_file, cache_ttl)
        if cached:
            learnings = [
                Learning(
                    chunk_text=r.get("chunk_text", ""),
                    frontmatter=r.get("frontmatter", {}),
                    archived_at=r.get("archived_at"),
                )
                for r in cached.get("results", [])
            ]
            learnings = rerank(learnings, query_tags)
            learnings = filter_by_confidence(learnings, confidence.upper())[:limit]
            log_recall(query, mode, len(learnings), cached=True)
            return RecallResult(learnings, query, mode, cache_hit=True)

    try:
        proc = subprocess.run(
            [str(cli), "search", query, "--mode", mode, "--format", "json",
             "--limit", str(max(limit * 2, 10))],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError, OSError) as e:
        return RecallResult([], query, mode, error=f"subprocess failed: {e}")

    if proc.returncode != 0:
        return RecallResult([], query, mode, error=f"learnings exit {proc.returncode}")

    learnings = parse_learnings_output(proc.stdout)
    # persist raw results to cache before filtering (so different confidence/limit
    # combinations can reuse the same fetch)
    if use_cache:
        write_cache(
            cache_file,
            {
                "query": query,
                "mode": mode,
                "fetched_at": time.time(),
                "results": [
                    {
                        "chunk_text": l.chunk_text,
                        "frontmatter": l.frontmatter,
                        "archived_at": l.archived_at,
                    }
                    for l in learnings
                ],
            },
        )
    learnings = rerank(learnings, query_tags)
    learnings = filter_by_confidence(learnings, confidence.upper())[:limit]
    log_recall(query, mode, len(learnings), cached=False)
    return RecallResult(learnings, query, mode)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("query", nargs="+", help="Search query")
    ap.add_argument("--limit", type=int, default=DEFAULT_LIMIT)
    ap.add_argument("--mode", choices=["naive", "local", "global"], default=DEFAULT_MODE)
    ap.add_argument("--confidence", choices=["HIGH", "MEDIUM", "LOW", "ANY"], default="ANY")
    ap.add_argument("--format", choices=["markdown", "json"], default="markdown")
    ap.add_argument("--max-chars", type=int, default=DEFAULT_MAX_CHARS)
    ap.add_argument("--no-cache", action="store_true")
    ap.add_argument("--cache-ttl", type=int, default=DEFAULT_CACHE_TTL)
    ap.add_argument("--tags", default="",
                    help="Comma-separated query tags for tag-overlap reranking")
    args = ap.parse_args()

    query = " ".join(args.query).strip()
    if not query:
        print("error: empty query", file=sys.stderr)
        return 2

    query_tags = [t.strip() for t in args.tags.split(",") if t.strip()]

    result = recall(
        query,
        limit=args.limit,
        mode=args.mode,
        confidence=args.confidence,
        max_chars=args.max_chars,
        use_cache=not args.no_cache,
        cache_ttl=args.cache_ttl,
        query_tags=query_tags,
    )

    if result.error:
        # D9: silent no-op on KB absence; only print to stderr when diagnostic
        if os.environ.get("REFLECT_RECALL_DEBUG"):
            print(f"recall: {result.error}", file=sys.stderr)
        # Empty output, exit 0
        return 0

    if args.format == "json":
        print(render_json(result.learnings, query, args.mode))
    else:
        out = render_markdown(result.learnings, query, max_chars=args.max_chars)
        if out:
            print(out)

    return 0


if __name__ == "__main__":
    sys.exit(main())
