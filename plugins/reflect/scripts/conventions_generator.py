#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# ABOUTME: Conventions doc generator (port O2, hindsight mental_models refresh
# ABOUTME: shape) — renders the living per-project CONVENTIONS.md from the O1
# ABOUTME: observations layer, keeps the conventions_docs row and the on-disk
# ABOUTME: doc in sync, and regenerates whenever in-scope observations change.
"""Generate and refresh the per-project CONVENTIONS.md.

Port O2. The agent previously had no single place to look up "what does this
project generally do" — every open-domain question meant a recall plus an
in-context synthesis of N raw corrections. This module pre-synthesizes the
O1 observations layer into one living markdown doc per project:

- the doc row lives in ``reflect.db.conventions_docs`` (project_id, query,
  content, scope_tags, doc_path, last_refreshed_at — see reflect_db);
- the doc FILE is materialized under ``<state>/conventions/<project>/
  CONVENTIONS.md`` (state = REFLECT_STATE_DIR or ~/.reflect) so the agent
  can read it like any regular file, costing zero boot tokens;
- regeneration is deterministic markdown over the observations table — no
  LLM sits on the refresh path (unlike R13 skill refreshes, which need the
  drain), so the cascade regenerates inline whenever an observation action
  lands (``reflect_cascade.trigger_conventions_refresh``);
- SessionStart (R10 hierarchy, Tier-1 ambient) injects a 1-line summary +
  path via :func:`session_inject_line` — never the doc body — and a stale
  doc (R14-shaped check in ``reflect_db.compute_conventions_is_stale``)
  does not inject at all.

Clean-room reimplementation of the *shape* of Hindsight's mental-model
refresh loop (``refresh_mental_model`` re-runs the model's source_query and
stores fresh content; ``trigger.refresh_after_consolidation`` re-runs it
after consolidation) — no source code was copied (ELv2). Differences
recorded deliberately:

- the "source query" here is a curated scope query over the observations
  table, not a reflect/LLM call — generation is deterministic and cheap;
- keyed by project_id (the per-codebase analog), not a UUID per model;
- the doc is also materialized on disk, optionally symlinked into the
  project root (:func:`symlink_into_project`, opt-in — never clobbers).

Stdlib-only (plugins/reflect contract).
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

import reflect_db

DOC_FILENAME = "CONVENTIONS.md"

# The observe CLI's default scope bucket (reflect_cascade observes with
# scope='project' when the drain doesn't pass a project id). A project-keyed
# doc aggregates this generic bucket too, and SessionStart falls back to the
# generic doc when no project-keyed one exists.
GENERIC_PROJECT_ID = "project"

# Hard cap on observations folded into one doc — the doc is a digest, not a
# dump (and generation stays O(small) on the cascade's write path).
DOC_OBSERVATION_LIMIT = 200

_SAFE_ID_RE = re.compile(r"[^A-Za-z0-9._-]+")


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def state_dir() -> Path:
    """Reflect state directory (REFLECT_STATE_DIR override > ~/.reflect)."""
    raw = os.environ.get("REFLECT_STATE_DIR", "")
    return Path(raw).expanduser() if raw else Path.home() / ".reflect"


def conventions_dir() -> Path:
    """Root directory the per-project conventions docs live under."""
    return state_dir() / "conventions"


def safe_project_dirname(project_id: str) -> str:
    """Filesystem-safe directory name for *project_id* (never empty)."""
    cleaned = _SAFE_ID_RE.sub("_", str(project_id or "").strip()).strip("._")
    return cleaned or "default"


def doc_path_for(project_id: str) -> Path:
    """Where *project_id*'s CONVENTIONS.md is materialized on disk."""
    return conventions_dir() / safe_project_dirname(project_id) / DOC_FILENAME


def curated_query(project_id: str) -> str:
    """The doc's source query (stored on the row for provenance — the
    hindsight ``source_query`` analog, answered by the observations table)."""
    return f"conventions preferences style — what does {project_id} generally do"


def _normalize_scopes(project_id: str, scopes: Optional[list[str]]) -> list[str]:
    """The observation scopes a doc aggregates, deduped and order-kept.

    Defaults to [project_id, 'project'] — the project's own bucket plus the
    observe CLI's generic default bucket — so the doc stays alive whether
    the drain scopes observations by project id or leaves the default.
    ``get_observations`` already folds ``global`` rows in per scope.
    """
    raw = list(scopes) if scopes else [project_id, GENERIC_PROJECT_ID]
    seen: set[str] = set()
    out: list[str] = []
    for scope in raw:
        scope = str(scope or "").strip()
        if scope and scope not in seen:
            seen.add(scope)
            out.append(scope)
    return out or [GENERIC_PROJECT_ID]


def collect_observations(
    scopes: list[str],
    *,
    limit: int = DOC_OBSERVATION_LIMIT,
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Active observations across *scopes* (+ global), deduped by id,
    strongest evidence first — the doc's curated source rows."""
    conn = conn or reflect_db.get_conn()
    merged: dict[str, dict[str, Any]] = {}
    for scope in scopes:
        for row in reflect_db.get_observations(scope=scope, limit=limit, conn=conn):
            merged.setdefault(row["id"], row)
    ranked = sorted(
        merged.values(),
        key=lambda row: (-int(row.get("proof_count") or 1), row.get("created_at") or ""),
    )
    return ranked[:limit]


def render_conventions_md(
    project_id: str,
    observations: list[dict[str, Any]],
    *,
    refreshed_at: str,
) -> str:
    """Deterministic markdown body of the doc: header + provenance note +
    conventions grouped by category, proof-ranked within each group."""
    lines = [
        f"# Conventions — {project_id}",
        "",
        "> Auto-generated by reflect (O2) from the consolidated observations",
        "> layer. Do not edit by hand — the doc regenerates whenever an",
        "> in-scope observation changes.",
        "",
        f"_Last refreshed: {refreshed_at} · {len(observations)} convention(s)_",
    ]
    if not observations:
        lines += [
            "",
            "_No conventions recorded yet — the drain's observation pass",
            "populates this doc as evidence accumulates._",
        ]
        return "\n".join(lines) + "\n"

    by_category: dict[str, list[dict[str, Any]]] = {}
    for obs in observations:
        by_category.setdefault(str(obs.get("category") or "Unknown"), []).append(obs)
    for category in sorted(by_category):
        lines += ["", f"## {category}"]
        for obs in by_category[category]:
            proof = int(obs.get("proof_count") or 1)
            lines.append(f"- {obs['content']} _(evidence ×{proof})_")
    return "\n".join(lines) + "\n"


def generate_conventions_doc(
    project_id: str,
    *,
    scopes: Optional[list[str]] = None,
    limit: int = DOC_OBSERVATION_LIMIT,
    conn: Optional[sqlite3.Connection] = None,
) -> dict[str, Any]:
    """(Re)generate *project_id*'s conventions doc: file + DB row.

    The hindsight ``refresh_mental_model`` analog: re-runs the doc's source
    query (a curated scope query over O1 observations), renders fresh
    markdown, materializes it at :func:`doc_path_for`, and upserts the
    ``conventions_docs`` row — which moves ``last_refreshed_at`` and clears
    the stored staleness flag. Returns a summary dict.
    """
    conn = conn or reflect_db.get_conn()
    pid = str(project_id or "").strip() or GENERIC_PROJECT_ID
    scope_list = _normalize_scopes(pid, scopes)
    observations = collect_observations(scope_list, limit=limit, conn=conn)
    refreshed_at = _now_iso()
    content = render_conventions_md(pid, observations, refreshed_at=refreshed_at)

    path = doc_path_for(pid)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")

    reflect_db.upsert_conventions_doc(
        pid,
        query=curated_query(pid),
        content=content,
        scope_tags=scope_list,
        doc_path=str(path),
        observation_count=len(observations),
        conn=conn,
    )
    return {
        "project_id": pid,
        "doc_path": str(path),
        "observation_count": len(observations),
        "scope_tags": scope_list,
    }


def refresh_if_stale(
    project_id: str,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> bool:
    """Regenerate *project_id*'s doc iff the R14-shaped check says stale.

    Returns True when a regeneration happened. A missing row is NOT
    auto-created here — registration happens via :func:`generate_conventions_doc`
    or the cascade trigger (:func:`refresh_for_scope`), so a read path can
    never spawn docs for arbitrary directories.
    """
    conn = conn or reflect_db.get_conn()
    stale = reflect_db.compute_conventions_is_stale(project_id, conn=conn)
    if not stale:
        return False
    row = reflect_db.get_conventions_doc(project_id, conn=conn)
    generate_conventions_doc(
        project_id,
        scopes=(row or {}).get("scope_tags") or None,
        conn=conn,
    )
    return True


def refresh_for_scope(
    scope: str,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> int:
    """O2 trigger body: regenerate every doc covering *scope*.

    Called by ``reflect_cascade`` after observation actions land (the R13
    trigger pointed at the conventions layer instead of skills). Matching:
    a doc covers *scope* when the scope appears in its ``scope_tags`` or
    equals its project_id; ``scope='global'`` covers EVERY doc (a global
    convention applies everywhere). When nothing matches a non-global
    scope, the doc is bootstrapped keyed by that scope — the first observe
    pass for a project brings its conventions doc into existence. Returns
    the number of docs regenerated.
    """
    conn = conn or reflect_db.get_conn()
    scope = str(scope or "").strip()
    if not scope:
        return 0
    refreshed = 0
    for doc in reflect_db.get_conventions_docs(conn=conn):
        covered = set(doc.get("scope_tags") or []) | {doc["project_id"]}
        if scope == "global" or scope in covered:
            generate_conventions_doc(
                doc["project_id"],
                scopes=doc.get("scope_tags") or None,
                conn=conn,
            )
            refreshed += 1
    if refreshed == 0 and scope != "global":
        generate_conventions_doc(scope, conn=conn)
        refreshed = 1
    return refreshed


def session_inject_line(
    project_id: str,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """The SessionStart block: a 1-line summary + path — never the doc body.

    Lookup falls back from the project-keyed doc to the generic
    ``'project'`` doc (the observe CLI's default bucket). Suppressed ("")
    when: no doc is registered, the doc aggregates zero observations, the
    on-disk file is missing, or the doc is STALE per the R14-shaped check
    (``reflect_db.compute_conventions_is_stale``) — a wrong pointer is
    worse than no pointer.
    """
    conn = conn or reflect_db.get_conn()
    for pid in dict.fromkeys([str(project_id or "").strip(), GENERIC_PROJECT_ID]):
        if not pid:
            continue
        doc = reflect_db.get_conventions_doc(pid, conn=conn)
        if doc is None:
            continue
        if not int(doc.get("observation_count") or 0):
            return ""
        if reflect_db.compute_conventions_is_stale(pid, conn=conn):
            return ""
        path = str(doc.get("doc_path") or "")
        if not path or not Path(path).is_file():
            return ""
        count = int(doc["observation_count"])
        return (
            "## Project conventions (pre-synthesized)\n"
            f"- {pid}: {count} convention(s) on record — read {path}"
        )
    return ""


def symlink_into_project(
    project_id: str,
    project_root: Path | str,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> bool:
    """Best-effort symlink ``<project_root>/CONVENTIONS.md`` → the state doc.

    Lets the agent read the doc as a regular file in the repo. Conservative
    by design: never clobbers an existing file or foreign symlink, returns
    True only when the link exists and points at this project's doc. The
    caller gates this behind an opt-in flag (REFLECT_CONVENTIONS_SYMLINK in
    the SessionStart hook) because writing into user repos is intrusive.
    """
    conn = conn or reflect_db.get_conn()
    doc = reflect_db.get_conventions_doc(project_id, conn=conn)
    target = Path(str(doc.get("doc_path"))) if doc and doc.get("doc_path") else doc_path_for(project_id)
    if not target.is_file():
        return False
    link = Path(project_root) / DOC_FILENAME
    try:
        if link.is_symlink():
            return link.resolve() == target.resolve()
        if link.exists():
            return False  # a real file lives there — never clobber it
        os.symlink(target, link)
        return True
    except OSError:
        return False


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> None:
    import argparse
    import sys

    parser = argparse.ArgumentParser(
        description="Reflect conventions doc generator (O2)"
    )
    parser.add_argument(
        "command",
        choices=["generate", "refresh", "refresh-scope", "show", "inject"],
        help="Action to perform",
    )
    parser.add_argument(
        "--project",
        default=None,
        help="Project id (default: derived from cwd git remote/basename)",
    )
    parser.add_argument(
        "--scope",
        default="",
        help="Observation scope (refresh-scope only)",
    )
    args = parser.parse_args()

    conn = reflect_db.get_conn()
    project_id = (
        args.project
        if args.project is not None
        else reflect_db.derive_slot_project_id()
    )

    if args.command == "generate":
        print(json.dumps(generate_conventions_doc(project_id, conn=conn)))
    elif args.command == "refresh":
        refreshed = refresh_if_stale(project_id, conn=conn)
        print(json.dumps({"project_id": project_id, "refreshed": refreshed}))
    elif args.command == "refresh-scope":
        count = refresh_for_scope(args.scope or project_id, conn=conn)
        print(json.dumps({"scope": args.scope or project_id, "refreshed": count}))
    elif args.command == "show":
        doc = reflect_db.get_conventions_doc(project_id, conn=conn)
        if doc is None:
            print(f"no conventions doc for {project_id!r}", file=sys.stderr)
            raise SystemExit(1)
        print(doc["content"])
    elif args.command == "inject":
        print(session_inject_line(project_id, conn=conn))


if __name__ == "__main__":
    main()
