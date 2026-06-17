#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# ABOUTME: Skills index (port R20, hindsight mental_models shape) — keeps reflect.db's
# ABOUTME: skills table (name, path, tags[], summary, mtime, last_refreshed_at, is_stale)
# ABOUTME: in sync with installed SKILL.md files so retrieval can match queries without
# ABOUTME: a file scan. Stale skills (R13) are excluded from matching until regenerated.
"""Maintain the installed-skills index in ``reflect.db``.

Port R20. Tiered inject (R10), forced-grounding short-circuit (R11), and the
auto-skill-refresh trigger (R13) all need a fast 'is there a skill for this
query?' check. Before this port the only option was a full filesystem scan
and frontmatter parse of every ``~/.claude/skills/*/SKILL.md`` on every
query. This module mirrors the skills into a sqlite table once, then keeps
it fresh with a stat()-only staleness pass.

Clean-room reimplementation of the *shape* of Hindsight's ``mental_models``
table (name + content + tags + trigger + last_refreshed_at) — no source code
was copied (ELv2). Differences recorded deliberately:

- keyed by ``path`` (SKILL.md file), not name — skill names can collide
  across plugin namespaces;
- ``mtime`` column added so refresh is a pure stat comparison;
- ``summary`` is the first line of the frontmatter description (capped),
  not the full content — the index is a router, not a content store.

Two entry points:

- :func:`rebuild_index` — full rescan + upsert + prune. Run from
  ``reflect:ingest`` (Step 9b of the skill).
- :func:`refresh_if_stale` — cheap incremental refresh: stat every
  SKILL.md, re-parse ONLY new/changed files, prune deleted rows.
  Safe to call on the hot inject path.

Stdlib-only (plugins/reflect contract). The frontmatter parser is a
minimal hand-rolled subset (scalars, block scalars, string lists) —
enough for SKILL.md frontmatter without a yaml dependency.
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
from pathlib import Path
from typing import Any, Optional

import reflect_db

# Where Claude skills live by default. Overridable per-call (tests) and via
# the REFLECT_SKILLS_DIR env var (parallel installs).
DEFAULT_SKILLS_DIR = Path.home() / ".claude" / "skills"

# Router summaries stay short — the index matches queries, it doesn't inject
# skill content.
SUMMARY_MAX_CHARS = 240

_KEY_RE = re.compile(r"^([A-Za-z0-9_-]+):\s*(.*)$")
_LIST_ITEM_RE = re.compile(r"^\s+-\s+(.*)$")
_TOKEN_RE = re.compile(r"[a-z0-9][a-z0-9_-]+")


def skills_dir() -> Path:
    """Resolve the skills directory (env override > default)."""
    raw = os.environ.get("REFLECT_SKILLS_DIR", "")
    return Path(raw).expanduser() if raw else DEFAULT_SKILLS_DIR


# ---------------------------------------------------------------------------
# Frontmatter parsing (minimal, stdlib-only)
# ---------------------------------------------------------------------------


def parse_frontmatter(text: str) -> dict[str, Any]:
    """Parse the leading ``--- ... ---`` YAML frontmatter block of *text*.

    Supports the subset SKILL.md files actually use: top-level scalar
    values, ``|``/``>`` block scalars, and flat string lists. Unknown
    constructs degrade to raw strings rather than raising — a malformed
    skill file must never break the index.
    """
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return {}
    try:
        end = next(i for i, ln in enumerate(lines[1:], start=1) if ln.strip() == "---")
    except StopIteration:
        return {}

    result: dict[str, Any] = {}
    i = 1
    while i < end:
        match = _KEY_RE.match(lines[i])
        if not match:
            i += 1
            continue
        key, value = match.group(1), match.group(2).strip()
        if value in ("|", ">", "|-", ">-"):
            # Block scalar: consume following indented lines.
            block: list[str] = []
            i += 1
            while i < end and (not lines[i].strip() or lines[i].startswith((" ", "\t"))):
                if _KEY_RE.match(lines[i]):
                    break
                block.append(lines[i].strip())
                i += 1
            result[key] = "\n".join(block).strip()
            continue
        if value == "":
            # Possibly a list: consume following "- item" lines.
            items: list[str] = []
            j = i + 1
            while j < end:
                item = _LIST_ITEM_RE.match(lines[j])
                if not item:
                    break
                items.append(item.group(1).strip().strip("\"'"))
                j += 1
            if items:
                result[key] = items
                i = j
                continue
            result[key] = ""
            i += 1
            continue
        result[key] = value.strip("\"'")
        i += 1
    return result


def _summarize(description: Any) -> str:
    """First meaningful line of *description*, whitespace-collapsed, capped."""
    if not isinstance(description, str):
        return ""
    for line in description.splitlines():
        line = " ".join(line.split())
        if line:
            return line[:SUMMARY_MAX_CHARS]
    return ""


def _normalize_tags(meta: dict[str, Any]) -> list[str]:
    """tags + triggers from frontmatter → unique, lowercased, order-kept."""
    raw: list[str] = []
    for key in ("tags", "triggers"):
        value = meta.get(key)
        if isinstance(value, list):
            raw.extend(str(v) for v in value)
        elif isinstance(value, str) and value:
            raw.append(value)
    seen: set[str] = set()
    out: list[str] = []
    for tag in raw:
        tag = tag.strip().lower()
        if tag and tag not in seen:
            seen.add(tag)
            out.append(tag)
    return out


def parse_skill_md(path: Path) -> dict[str, Any]:
    """Extract the index record (name, tags, summary) from one SKILL.md.

    Name falls back to the containing directory when frontmatter omits it.
    Never raises on unreadable/malformed files — returns the dirname-only
    record so the skill still appears in the index.
    """
    try:
        meta = parse_frontmatter(path.read_text(encoding="utf-8", errors="replace"))
    except OSError:
        meta = {}
    name = str(meta.get("name") or path.parent.name)
    return {
        "name": name,
        "tags": _normalize_tags(meta),
        "summary": _summarize(meta.get("description")),
    }


# ---------------------------------------------------------------------------
# Filesystem scan
# ---------------------------------------------------------------------------


def scan_skill_files(base: Optional[Path] = None) -> dict[str, float]:
    """Map of SKILL.md path -> mtime for every installed skill.

    Looks one and two directory levels under *base* (``<skill>/SKILL.md``
    and ``<namespace>/<skill>/SKILL.md``). stat()-only — this is the cheap
    half of the staleness check, no file contents are read.
    """
    base = base or skills_dir()
    found: dict[str, float] = {}
    for pattern in ("*/SKILL.md", "*/*/SKILL.md"):
        for path in base.glob(pattern):
            try:
                found[str(path)] = path.stat().st_mtime
            except OSError:
                continue
    return found


# ---------------------------------------------------------------------------
# Index maintenance
# ---------------------------------------------------------------------------


def rebuild_index(
    base: Optional[Path] = None,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> dict[str, int]:
    """Full rebuild: parse every SKILL.md, upsert all, prune missing rows.

    The ``reflect:ingest`` entry point. Returns ``{"indexed", "removed",
    "total"}`` counts.
    """
    conn = conn or reflect_db.get_conn()
    files = scan_skill_files(base)
    for path_str, mtime in sorted(files.items()):
        record = parse_skill_md(Path(path_str))
        reflect_db.upsert_skill(
            record["name"],
            path_str,
            tags=record["tags"],
            summary=record["summary"],
            mtime=mtime,
            conn=conn,
        )
    stale_paths = [
        row["path"] for row in reflect_db.get_skills(conn=conn)
        if row["path"] not in files
    ]
    removed = reflect_db.remove_skills(stale_paths, conn=conn)
    return {"indexed": len(files), "removed": removed, "total": len(files)}


def refresh_if_stale(
    base: Optional[Path] = None,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> dict[str, int]:
    """Cheap incremental refresh: re-parse ONLY new/mtime-changed skills.

    The hot-path entry point (R10/R11/R13 call this before matching).
    Unchanged skills cost one stat() each — their SKILL.md is never read.
    Returns ``{"added", "changed", "removed", "unchanged"}`` counts.
    """
    conn = conn or reflect_db.get_conn()
    files = scan_skill_files(base)
    indexed = {row["path"]: row["mtime"] for row in reflect_db.get_skills(conn=conn)}

    added = [p for p in files if p not in indexed]
    changed = [p for p in files if p in indexed and files[p] != indexed[p]]
    removed_paths = [p for p in indexed if p not in files]

    for path_str in sorted(added) + sorted(changed):
        record = parse_skill_md(Path(path_str))
        reflect_db.upsert_skill(
            record["name"],
            path_str,
            tags=record["tags"],
            summary=record["summary"],
            mtime=files[path_str],
            conn=conn,
        )
    removed = reflect_db.remove_skills(removed_paths, conn=conn)
    return {
        "added": len(added),
        "changed": len(changed),
        "removed": removed,
        "unchanged": len(files) - len(added) - len(changed),
    }


# ---------------------------------------------------------------------------
# Query matching
# ---------------------------------------------------------------------------


def _tokenize(text: str) -> set[str]:
    return set(_TOKEN_RE.findall(text.lower()))


def match_skills(
    query: str,
    *,
    limit: int = 5,
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Rank indexed skills against *query* by token overlap.

    Name/tag hits weigh double summary hits (a query naming the skill or
    one of its triggers is a stronger routing signal than prose overlap).
    Only rows scoring > 0 are returned, best first, each annotated with a
    ``score`` key. This is the 'is there a skill for this query?' check
    R10/R11 build on — deliberately deterministic and stdlib-only.

    R13: skills flagged ``is_stale`` (a backing learning was revised after
    the SKILL.md was written) never match — a stale skill must not win the
    inject tier with possibly-outdated guidance. The hierarchy falls
    through to the raw-learnings tier until the refresh task regenerates
    the skill (mtime change clears the flag in ``upsert_skill``).
    """
    query_tokens = _tokenize(query)
    if not query_tokens:
        return []
    scored: list[tuple[float, dict[str, Any]]] = []
    # compute_stale=True so the R14 on-read recompute gates the inject tier, not
    # just the stored R13 flag — a learning revised via add_learning_proof /
    # update_learning_status / contradiction-demote doesn't fire the R13 trigger
    # and would otherwise let a stale skill win the tier (and short-circuit).
    for row in reflect_db.get_skills(compute_stale=True, conn=conn):
        if row.get("is_stale"):
            continue
        strong = _tokenize(row["name"]) | _tokenize(" ".join(row["tags"]))
        weak = _tokenize(row["summary"]) - strong
        score = 2.0 * len(query_tokens & strong) + 1.0 * len(query_tokens & weak)
        if score > 0:
            scored.append((score, {**row, "score": score}))
    scored.sort(key=lambda pair: (-pair[0], pair[1]["name"]))
    return [row for _, row in scored[:limit]]


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="Reflect skills index (R20)")
    parser.add_argument(
        "command",
        choices=["rebuild", "refresh", "list", "match"],
        help="Action to perform",
    )
    parser.add_argument("query", nargs="?", default="", help="Query for `match`")
    parser.add_argument("--skills-dir", default="", help="Override the skills directory")
    parser.add_argument("--limit", type=int, default=5)
    args = parser.parse_args()

    base = Path(args.skills_dir).expanduser() if args.skills_dir else None
    conn = reflect_db.get_conn()

    if args.command == "rebuild":
        print(json.dumps(rebuild_index(base, conn=conn)))

    elif args.command == "refresh":
        print(json.dumps(refresh_if_stale(base, conn=conn)))

    elif args.command == "list":
        for row in reflect_db.get_skills(conn=conn):
            stale = "  [STALE]" if row.get("is_stale") else ""
            print(
                f"  {row['name']}  tags={','.join(row['tags']) or '-'}  "
                f"refreshed={row['last_refreshed_at']}  {row['path']}{stale}"
            )

    elif args.command == "match":
        for row in match_skills(args.query, limit=args.limit, conn=conn):
            print(f"  {row['score']:5.1f}  {row['name']}  {row['path']}")


if __name__ == "__main__":
    main()
