#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "pyyaml",
# ]
# ///
"""
Reflect Recall — hybrid retrieval from the global learnings KB.

Wraps the `reflect` CLI (reflect-kb, installed via `uv tool install reflect-kb`)
as a subprocess so we inherit GraphRAG + embeddings without pulling the
nano-graphrag dep chain into this plugin.

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
import concurrent.futures
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
# Canonical CLI name (reflect-kb). Resolved via `shutil.which("reflect")` so
# we honour whatever install path `uv tool install reflect-kb` produced
# (typically ~/.local/bin/reflect). Legacy `~/.learnings/cli/learnings` is
# retained as a last-ditch fallback ONLY so recall doesn't silently break
# on machines mid-migration; new installs should never hit it.
REFLECT_CLI_NAME = "reflect"
LEGACY_LEARNINGS_CLI = Path.home() / ".learnings" / "cli" / "learnings"

CONFIDENCE_WEIGHTS = {"HIGH": 1.0, "MEDIUM": 0.7, "LOW": 0.4}
CHUNK_SEPARATOR = "--New Chunk--"
ARCHIVE_HEADER_RE = re.compile(r"<!--\s*archived:\s*([0-9T:.+\-Z]+)\s*-->")

# R1: graph-expansion arm. The engine's `local` mode walks the entity
# neighborhood (nano-graphrag) — surfacing learnings that share entities with
# the lexical/vector hits but don't match the query text directly. Disable
# with RECALL_GRAPH_ARM=0.
GRAPH_ARM_ENABLED = os.environ.get("RECALL_GRAPH_ARM", "1") != "0"

# R7: OOD gate. Stopword-filtered query-term coverage of the top hit; below
# the threshold the whole result set is treated as out-of-domain noise and
# suppressed. 0.0 = gate off (library callers opt in; the SessionStart hook
# passes its configured threshold).
_STOPWORDS = frozenset(
    "a an the is are was were be been do does did to of in on at for with "
    "and or not no how what when where which who why our we i you it its "
    "this that these those there here from by as into over under again "
    "still now then than can could should would may might will shall am "
    "get got use used using my your".split()
)

# R4: token-budget retrieval. Rough estimate — 1 token ≈ 4 chars — matching
# Hindsight's budget-not-top-k contract (agents think in tokens).
def _est_tokens(text: str) -> int:
    return max(1, len(text) // 4)

# --- QMD fusion config ---------------------------------------------------
# QMD provides BM25 lexical search (fast, ~0.5s) as a complement to
# GraphRAG's vector path. Fusing the two via RRF gives hybrid lex+vec
# retrieval without changing the reflect CLI.
QMD_COLLECTION = "learnings"
QMD_DOCS_ROOT = Path.home() / ".learnings" / "documents"
QMD_PATH_RE = re.compile(r"qmd://" + re.escape(QMD_COLLECTION) + r"/(\S+?\.md)")
RRF_K = 60  # standard reciprocal-rank-fusion constant


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
        raw = self.frontmatter.get("confidence")
        if raw is None:
            return "MEDIUM"
        # Coerce numeric confidence (instinct-style 0.0-1.0) to tier.
        # Explicit None check above so `0`/`0.0` reach this branch, not the default.
        if isinstance(raw, bool):
            # bool is a subclass of int — treat as a tier string via str().upper()
            return str(raw).upper()
        if isinstance(raw, (int, float)):
            if raw >= 0.8:
                return "HIGH"
            if raw >= 0.5:
                return "MEDIUM"
            return "LOW"
        return str(raw).upper()

    @property
    def tags(self) -> list[str]:
        raw = self.frontmatter.get("tags") or []
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
    ood_gated: bool = False  # R7: True when the OOD gate suppressed results


# --- Helpers -------------------------------------------------------------

def find_learnings_cli() -> Path | None:
    """Locate the reflect-kb CLI. D1: subprocess wrapper.

    Resolution order:
      1. `shutil.which("reflect")` — canonical install via `uv tool install reflect-kb`.
         Resolves through $PATH so it picks up whatever the user's environment
         points at (usually ~/.local/bin/reflect).
      2. Legacy `~/.learnings/cli/learnings` — pre-migration install. Kept only
         so machines that haven't installed reflect-kb yet don't silently lose
         recall; new code paths should never hit this.

    Trust boundary: `$PATH` is only as trustworthy as the caller's environment,
    but this script only runs in the user's own session — a hostile `$PATH`
    would already compromise their shell.

    Returns None if neither is found. Caller surfaces a graceful empty result.
    """
    cli_on_path = shutil.which(REFLECT_CLI_NAME)
    if cli_on_path:
        return Path(cli_on_path)
    if LEGACY_LEARNINGS_CLI.exists() and os.access(LEGACY_LEARNINGS_CLI, os.X_OK):
        return LEGACY_LEARNINGS_CLI
    return None


CACHE_VERSION = "v3-hybrid-graph"  # bump when fusion semantics change


def cache_path(query: str, mode: str, limit: int) -> Path:
    """Per-query cache file. D4: 1-hour TTL.

    Limit is part of the key so a small-limit fetch can't poison a
    subsequent large-limit read with a truncated result set. Version tag
    invalidates old caches when the fusion pipeline changes.

    `query_tags` is intentionally NOT part of the key: tags only affect
    rerank ordering (applied after cache read) and the fetched raw set
    is tag-independent, so two calls with same (query, mode, limit) but
    different tags correctly share a cached fetch.
    """
    digest = hashlib.sha1(
        f"{CACHE_VERSION}|{query}|{mode}|{limit}".encode()
    ).hexdigest()[:16]
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
    except OSError as e:
        # Disk full / permission / path too long — non-fatal, but surface
        # in debug mode so silent cache-write failures don't hide real
        # issues (e.g. $HOME on a read-only volume).
        if os.environ.get("REFLECT_RECALL_DEBUG"):
            print(f"recall: cache write failed: {e}", file=sys.stderr)


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


def find_qmd_cli() -> Path | None:
    """Locate the `qmd` binary. Returns None if not installed."""
    cli_on_path = shutil.which("qmd")
    return Path(cli_on_path) if cli_on_path else None


def fetch_qmd(query: str, limit: int, timeout: int = 10) -> list[Learning]:
    """Fast BM25 retrieval via qmd. Complement to GraphRAG's vector path.

    Returns empty list on any failure (missing CLI, timeout, empty KB) — QMD
    is strictly a booster, never a blocker.
    """
    qmd = find_qmd_cli()
    if not qmd:
        return []
    try:
        proc = subprocess.run(
            [str(qmd), "search", query, "-c", QMD_COLLECTION,
             "--limit", str(limit)],
            capture_output=True, text=True, timeout=timeout, check=False,
        )
    except (subprocess.TimeoutExpired, OSError):
        return []
    if proc.returncode != 0 or not proc.stdout:
        return []
    return parse_qmd_output(proc.stdout)


def parse_qmd_output(text: str) -> list[Learning]:
    """Convert qmd's text output to Learning objects by reading each hit's file.

    qmd emits lines like `qmd://learnings/learnings/<file>.md:<line> #hash`
    for each result. We extract the relative path, resolve it under the QMD
    collection root, and parse frontmatter + body.
    """
    seen: set[str] = set()
    learnings: list[Learning] = []
    for m in QMD_PATH_RE.finditer(text):
        rel = m.group(1)
        if rel in seen:  # qmd can emit multiple line hits per file
            continue
        seen.add(rel)
        path = QMD_DOCS_ROOT / rel
        try:
            content = path.read_text()
        except OSError:
            continue
        fm, body = parse_frontmatter(content)
        archived = None
        am = ARCHIVE_HEADER_RE.search(body)
        if am:
            archived = am.group(1)
        learnings.append(Learning(chunk_text=content, frontmatter=fm, archived_at=archived))
    return learnings


def _learning_key(learning: Learning) -> str:
    """Dedup key stable across backends. Prefers frontmatter id, falls back to
    a hash of the chunk so distinct chunks don't collapse."""
    fid = learning.frontmatter.get("id") or learning.frontmatter.get("name")
    if fid:
        return str(fid)
    return hashlib.sha1(learning.chunk_text[:256].encode()).hexdigest()[:12]


def rrf_fuse(result_lists: list[list[Learning]], k: int = RRF_K) -> list[Learning]:
    """Reciprocal Rank Fusion. Standard hybrid-search technique.

    score(doc) = Σ 1 / (k + rank_in_each_source)

    Source-agnostic — doesn't need score normalization across backends.
    Docs appearing in both get summed scores → fused ranking.
    """
    scores: dict[str, float] = {}
    first_seen: dict[str, Learning] = {}
    for results in result_lists:
        for rank, learning in enumerate(results, start=1):
            key = _learning_key(learning)
            scores[key] = scores.get(key, 0.0) + 1.0 / (k + rank)
            # Keep the first occurrence (prefer full-chunk from learnings search
            # over file-read from qmd when both are present)
            if key not in first_seen:
                first_seen[key] = learning
    ordered_keys = sorted(scores, key=lambda key: scores[key], reverse=True)
    return [first_seen[key] for key in ordered_keys]


def parse_learnings_output(json_blob: str) -> list[Learning]:
    """Split a `reflect search --format json` response into Learning objects."""
    try:
        envelope = json.loads(json_blob)
    except json.JSONDecodeError:
        return []
    # Expected shape is {"context": "...chunks...--New Chunk--..."}.
    # Guard against list/string/other shapes so a CLI format change can't
    # crash us — it should just return zero results.
    if not isinstance(envelope, dict):
        return []
    context = envelope.get("context", "")
    if not isinstance(context, str) or not context:
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
            except (ValueError, TypeError):
                # TypeError: aware-vs-naive datetime subtraction (one side
                # has +00:00 offset). ValueError: malformed ISO string.
                # Either way, fall back to neutral recency rather than
                # crashing the entire rerank over one bad archive header.
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
    return [lrn for lrn in learnings if rank.get(lrn.confidence, 0) >= min_rank]


def _content_terms(text: str) -> set[str]:
    return {
        t for t in re.findall(r"[a-z0-9][a-z0-9_\-]{2,}", text.lower())
        if t not in _STOPWORDS
    }


def lexical_overlap(query: str, learning: Learning) -> float:
    """R7: fraction of the query's content terms present in the chunk.

    The engine always returns its nearest neighbours — even for a query about
    something the KB has never seen. Confidence/recency scores can't tell
    "relevant" from "nearest junk"; query-term coverage can (cheaply, stdlib).
    Hyphen/underscore-split variants count so `kill-server` matches `kill`+
    `server` phrasing and vice versa.
    """
    q_terms = _content_terms(query)
    if not q_terms:
        return 1.0  # vacuous query — never gate
    text = learning.chunk_text.lower()
    hits = 0
    for term in q_terms:
        if term in text:
            hits += 1
            continue
        parts = [p for p in re.split(r"[-_]", term) if len(p) >= 3]
        if parts and all(p in text for p in parts):
            hits += 1
    return hits / len(q_terms)


def apply_ood_gate(
    learnings: list[Learning], query: str, min_overlap: float
) -> tuple[list[Learning], bool]:
    """R7: suppress the result set when even the BEST hit barely mentions the
    query's terms. Returns (learnings, gated)."""
    if min_overlap <= 0 or not learnings:
        return learnings, False
    best = max(lexical_overlap(query, lrn) for lrn in learnings[:5])
    if best < min_overlap:
        return [], True
    return learnings, False


def filter_by_token_budget(
    learnings: list[Learning], max_tokens: int
) -> list[Learning]:
    """R4: return learnings until the token budget is spent (≥1 always kept,
    so a single long learning can't starve the caller)."""
    if max_tokens <= 0:
        return learnings
    out: list[Learning] = []
    spent = 0
    for lrn in learnings:
        cost = _est_tokens(lrn.chunk_text)
        if out and spent + cost > max_tokens:
            break
        out.append(lrn)
        spent += cost
    return out


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


def render_json(
    learnings: list[Learning], query: str, mode: str, ood_gated: bool = False
) -> str:
    return json.dumps(
        {
            "query": query,
            "mode": mode,
            "count": len(learnings),
            "ood_gated": ood_gated,
            "results": [
                {
                    "id": lrn.id,
                    "title": lrn.title,
                    "key_insight": lrn.key_insight,
                    "confidence": lrn.confidence,
                    "tags": lrn.tags,
                    "how_to_apply": lrn.how_to_apply,
                    "archived_at": lrn.archived_at,
                }
                for lrn in learnings
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
    max_tokens: int = 0,
    min_overlap: float = 0.0,
) -> RecallResult:
    """High-level API: query → ranked Learnings. Never raises on KB issues.

    R4: ``max_tokens`` > 0 bounds the result set by estimated tokens instead
    of count alone. R7: ``min_overlap`` > 0 suppresses out-of-domain results
    (top-hit query-term coverage below the threshold => empty set).
    """
    cli = find_learnings_cli()
    if not cli:
        return RecallResult(
            [], query, mode,
            error="reflect CLI not found on $PATH (install with `uv tool install reflect-kb`)",
        )

    fetched_limit = max(limit * 2, 10)
    cache_file = cache_path(query, mode, fetched_limit)
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
            learnings = filter_by_confidence(learnings, confidence.upper())
            learnings, gated = apply_ood_gate(learnings, query, min_overlap)  # R7
            learnings = learnings[:limit]
            learnings = filter_by_token_budget(learnings, max_tokens)  # R4
            log_recall(query, mode, len(learnings), cached=True)
            return RecallResult(learnings, query, mode, cache_hit=True, ood_gated=gated)

    # Fan out vector search (reflect CLI), QMD (BM25), and — R1 — the graph
    # arm (reflect `--mode local`, entity-neighborhood expansion) in parallel.
    # QMD and the graph arm are boosters, not blockers — each returns [] on
    # any failure and fusion still works.
    def _fetch_mode(search_mode: str) -> tuple[list[Learning], str | None]:
        try:
            proc = subprocess.run(
                [str(cli), "search", query, "--mode", search_mode,
                 "--format", "json", "--limit", str(fetched_limit)],
                capture_output=True, text=True, timeout=60, check=False,
            )
        except (subprocess.TimeoutExpired, OSError) as e:
            return [], f"subprocess failed: {e}"
        if proc.returncode != 0:
            return [], f"reflect search exit {proc.returncode}"
        return parse_learnings_output(proc.stdout), None

    # R1: only add the entity-graph arm when it differs from the primary mode
    # (an explicit `--mode local` call shouldn't fan out twice).
    graph_arm = GRAPH_ARM_ENABLED and mode != "local"
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        learnings_future = pool.submit(_fetch_mode, mode)
        qmd_future = pool.submit(fetch_qmd, query, fetched_limit)
        graph_future = pool.submit(_fetch_mode, "local") if graph_arm else None
        graph_results, graph_err = learnings_future.result()
        qmd_results = qmd_future.result()
        entity_results: list[Learning] = []
        if graph_future is not None:
            entity_results, _entity_err = graph_future.result()  # booster — errors ignored

    # If the primary path failed but a booster returned results, keep going.
    if graph_err and not qmd_results and not entity_results:
        return RecallResult([], query, mode, error=graph_err)

    learnings = rrf_fuse([graph_results, qmd_results, entity_results])
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
    learnings = filter_by_confidence(learnings, confidence.upper())
    learnings, gated = apply_ood_gate(learnings, query, min_overlap)  # R7
    learnings = learnings[:limit]
    learnings = filter_by_token_budget(learnings, max_tokens)  # R4
    log_recall(query, mode, len(learnings), cached=False)
    return RecallResult(learnings, query, mode, ood_gated=gated)


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
    ap.add_argument("--max-tokens", type=int, default=0,
                    help="R4: bound results by estimated tokens (0 = no budget)")
    ap.add_argument("--min-overlap", type=float, default=0.0,
                    help="R7: OOD gate — suppress results when the best hit's "
                         "query-term coverage is below this (0 = off)")
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
        max_tokens=args.max_tokens,
        min_overlap=args.min_overlap,
    )

    if result.error:
        # D9: silent no-op on KB absence; only print to stderr when diagnostic
        if os.environ.get("REFLECT_RECALL_DEBUG"):
            print(f"recall: {result.error}", file=sys.stderr)
        # Empty output, exit 0
        return 0

    if args.format == "json":
        print(render_json(result.learnings, query, args.mode, result.ood_gated))
    else:
        out = render_markdown(result.learnings, query, max_chars=args.max_chars)
        if out:
            print(out)

    return 0


if __name__ == "__main__":
    sys.exit(main())
