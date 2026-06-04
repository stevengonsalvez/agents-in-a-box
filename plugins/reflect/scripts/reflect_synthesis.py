#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
reflect_synthesis.py — weekly Opus synthesis pass (W5 / decision: cascade + weekly Opus).

Per-transcript reflection runs cheap (cascade + Sonnet). Opus earns its keep in
a PERIODIC batch instead: scan recent learnings, find near-duplicate / mergeable
clusters and cross-cutting meta-patterns, and propose merges. This is the one
place a big model is worth it — bounded, weekly, high-leverage — not per
transcript.

Two layers:
  * cluster()   — deterministic near-dupe grouping by title/tag token overlap
                  (no LLM, fully testable). This is the candidate finder.
  * synthesize  — hand the clusters to one bounded Opus call to propose merges.
                  Optional; --dry-run reports clusters without calling a model.

CLI:
    reflect_synthesis.py [--docs-dir DIR] [--since 7d] [--threshold 0.5]
                         [--dry-run] [--model opus]
"""

from __future__ import annotations

import re
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

_DOC_GLOBS = [
    "~/.learnings/documents",
    "~/.claude/global-learnings/documents",
]
_STOP = {"the", "a", "an", "to", "of", "in", "on", "for", "and", "or", "is",
         "via", "with", "when", "not", "use", "vs", "but"}


@dataclass
class Doc:
    path: Path
    title: str
    tags: list[str] = field(default_factory=list)
    mtime: float = 0.0


def _parse_frontmatter(text: str) -> dict:
    if not text.startswith("---"):
        return {}
    end = text.find("\n---", 3)
    if end == -1:
        return {}
    fm: dict = {}
    for line in text[3:end].splitlines():
        if ":" in line:
            k, _, v = line.partition(":")
            fm[k.strip()] = v.strip().strip('"')
    return fm


def load_docs(docs_dir: Optional[str], since_seconds: float) -> list[Doc]:
    roots = [Path(docs_dir).expanduser()] if docs_dir else \
        [Path(p).expanduser() for p in _DOC_GLOBS]
    cutoff = _now() - since_seconds if since_seconds else 0
    docs: list[Doc] = []
    for root in roots:
        if not root.is_dir():
            continue
        for md in root.glob("**/*.md"):
            try:
                mtime = md.stat().st_mtime
            except OSError:
                continue
            if cutoff and mtime < cutoff:
                continue
            try:
                text = md.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            fm = _parse_frontmatter(text)
            title = fm.get("title") or md.stem
            tags = []
            raw_tags = fm.get("tags", "")
            if raw_tags:
                tags = [t.strip().strip("[]") for t in raw_tags.split(",") if t.strip()]
            docs.append(Doc(md, title, tags, mtime))
    return docs


def _tokens(s: str) -> set[str]:
    return {w for w in re.findall(r"[a-z0-9]+", s.lower()) if w not in _STOP and len(w) > 2}


def _similarity(a: Doc, b: Doc) -> float:
    ta, tb = _tokens(a.title) | set(a.tags), _tokens(b.title) | set(b.tags)
    if not ta or not tb:
        return 0.0
    return len(ta & tb) / len(ta | tb)  # Jaccard


def cluster(docs: list[Doc], threshold: float = 0.5) -> list[list[Doc]]:
    """Group docs whose title/tag token sets overlap >= threshold (Jaccard).
    Simple union-find style single-linkage clustering."""
    n = len(docs)
    parent = list(range(n))

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]
            x = parent[x]
        return x

    def union(x, y):
        parent[find(x)] = find(y)

    for i in range(n):
        for j in range(i + 1, n):
            if _similarity(docs[i], docs[j]) >= threshold:
                union(i, j)

    groups: dict[int, list[Doc]] = {}
    for i in range(n):
        groups.setdefault(find(i), []).append(docs[i])
    # Only clusters with >1 member are interesting (merge candidates).
    return [g for g in groups.values() if len(g) > 1]


def _now() -> float:
    return time.time()


def _since_seconds(s: str) -> float:
    s = (s or "7d").strip().lower()
    try:
        if s.endswith("d"):
            return int(s[:-1]) * 86400
        if s.endswith("w"):
            return int(s[:-1]) * 604800
        if s.endswith("h"):
            return int(s[:-1]) * 3600
        return int(s) * 86400
    except ValueError:
        return 7 * 86400


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Weekly Opus synthesis of learnings")
    ap.add_argument("--docs-dir", default=None)
    ap.add_argument("--since", default="7d")
    ap.add_argument("--threshold", type=float, default=0.5)
    ap.add_argument("--model", default="opus")
    ap.add_argument("--dry-run", action="store_true",
                    help="report merge-candidate clusters; do not call a model")
    args = ap.parse_args()

    docs = load_docs(args.docs_dir, _since_seconds(args.since))
    clusters = cluster(docs, args.threshold)
    print(f"reflect synthesis: {len(docs)} learnings in window, "
          f"{len(clusters)} merge-candidate cluster(s)")
    for i, group in enumerate(clusters, 1):
        print(f"  cluster {i} ({len(group)}):")
        for d in group:
            print(f"    - {d.title}  [{d.path.name}]")

    if args.dry_run or not clusters:
        return

    # Live synthesis: one bounded Opus call per cluster to propose a merge.
    # Kept intentionally small — this is where the big model is justified.
    try:
        import subprocess
        for group in clusters:
            titles = "\n".join(f"- {d.title}" for d in group)
            prompt = (
                "These learning notes look like near-duplicates. Propose ONE "
                "merged note (or say 'keep separate' with a one-line reason):\n"
                f"{titles}"
            )
            subprocess.run(
                ["claude", "-p", prompt, "--model", args.model,
                 "--output-format", "json", "--max-turns", "3"],
                capture_output=True, text=True, timeout=180,
            )
    except Exception as exc:  # noqa: BLE001
        print(f"  (synthesis model call skipped: {exc})", file=sys.stderr)


if __name__ == "__main__":
    main()
