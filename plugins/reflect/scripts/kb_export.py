#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# ABOUTME: `reflect kb export <tarball>` — snapshots the reflect-kb learnings
# ABOUTME: corpus (~/.learnings/documents/ + sidecars) and the reflect.db user
# ABOUTME: tables into a single deterministic, git-friendly tarball that
# ABOUTME: kb_import.py can restore onto a fresh machine without re-draining.
"""
KB export (C5, Hindsight admin export-bank port).

Produces a portable, *deterministic* snapshot of a reflect knowledge base:

    documents/<file>.md                  learning notes
    documents/<file>.entities.yaml       entity sidecars (GraphRAG seed)
    reflect.db                           filtered SQLite of user-data tables
    manifest.json                        format version + table/row inventory

Design (mirrors hindsight ``export-bank``):

* **Embeddings / index artifacts are NOT exported.** The nano-graphrag cache,
  ``.vectors``, ``.graph`` are derived state — ``kb_import`` rebuilds them via
  GraphRAG reindex. This keeps the tarball small *and* lets the KB move between
  machines whose embedding models differ.
* **The DB snapshot is schema-agnostic.** We copy every *base* table verbatim
  (``SELECT *`` per table, column names read from the live schema) into a fresh
  SQLite file. Hardcoding column lists would silently drop rows after the next
  ``reflect_db`` migration, so we never do that — the export survives schema
  drift by construction.

Git-friendliness / determinism (the C5 ACCEPTANCE bar):

* Tar members are emitted in **sorted path order**.
* Every tar member's metadata is normalized: fixed mtime (0), uid/gid 0, mode
  0o644, owner/group cleared. Two exports of the same KB are byte-identical.
* Paths inside the tar are **relative** (``documents/...``, ``reflect.db``) —
  no absolute machine paths leak in.
* The embedded DB is rewritten row-by-row into a fresh file (no WAL, no free
  pages, deterministic page layout), so the bytes are reproducible too.

Invoke directly (no central CLI dispatcher in the reflect plugin):

    python kb_export.py /path/to/kb.tar
    python kb_export.py kb.tar --db ~/.reflect/reflect.db --learnings ~/.learnings
"""

from __future__ import annotations

import argparse
import io
import json
import sqlite3
import sys
import tarfile
from pathlib import Path
from typing import Optional

# Bump only on a breaking change to the tarball layout / manifest shape.
EXPORT_FORMAT_VERSION = 1

# Files inside ``<learnings_home>/documents`` we carry. Learning notes and their
# entity sidecars are the durable corpus; the GraphRAG cache is derived and is
# rebuilt on import, so it is deliberately excluded.
_DOC_SUFFIXES = (".md", ".entities.yaml")

# Deterministic tar member metadata — see module docstring.
_FIXED_MTIME = 0
_FIXED_MODE = 0o644
_FIXED_DIR_MODE = 0o755


def _default_db_path() -> Path:
    """Resolve the reflect.db path from reflect_config, falling back to the
    documented default. Imported lazily so the script stays runnable even if the
    config module's optional deps are unavailable."""
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import reflect_db  # type: ignore

        return reflect_db.db_path()
    except Exception:
        return Path.home() / ".reflect" / "reflect.db"


def _default_learnings_home() -> Path:
    return Path.home() / ".learnings"


def _list_documents(learnings_home: Path) -> list[Path]:
    """Sorted list of learning-note + sidecar files under documents/.

    Sorted by relative POSIX path so tar ordering is deterministic and stable
    across machines/filesystems."""
    docs_dir = learnings_home / "documents"
    if not docs_dir.is_dir():
        return []
    out: list[Path] = []
    for p in docs_dir.rglob("*"):
        if p.is_file() and p.name.endswith(_DOC_SUFFIXES):
            out.append(p)
    out.sort(key=lambda p: p.relative_to(docs_dir).as_posix())
    return out


def _base_tables(conn: sqlite3.Connection) -> list[str]:
    """User-data base tables, sorted. Excludes sqlite internals and indexes."""
    rows = conn.execute(
        "SELECT name FROM sqlite_master WHERE type='table' "
        "AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ).fetchall()
    return [r[0] for r in rows]


def _table_ddl(conn: sqlite3.Connection, table: str) -> str:
    row = conn.execute(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name=?", (table,)
    ).fetchone()
    return row[0] if row and row[0] else ""


def _index_ddls(conn: sqlite3.Connection) -> list[str]:
    rows = conn.execute(
        "SELECT sql FROM sqlite_master WHERE type='index' "
        "AND sql IS NOT NULL ORDER BY name"
    ).fetchall()
    return [r[0] for r in rows]


def _column_names(conn: sqlite3.Connection, table: str) -> list[str]:
    return [r[1] for r in conn.execute(f"PRAGMA table_info({table})").fetchall()]


def _ordered_rows(conn: sqlite3.Connection, table: str, columns: list[str]) -> list[tuple]:
    """All rows of *table*, ordered deterministically.

    Ordered by the table's declared columns (left-to-right) so two exports of
    the same logical content serialize identically regardless of insert order
    or sqlite rowid reuse."""
    col_list = ", ".join(f'"{c}"' for c in columns)
    order_by = ", ".join(f'"{c}"' for c in columns)
    sql = f'SELECT {col_list} FROM "{table}" ORDER BY {order_by}'
    return [tuple(r) for r in conn.execute(sql).fetchall()]


def build_db_snapshot(src_db: Path) -> bytes:
    """Serialize a fresh, deterministic SQLite snapshot of all base tables.

    Returns the raw bytes of a standalone ``.db`` file (no WAL). Schema and row
    data are copied verbatim; the snapshot is rebuilt from scratch so its page
    layout is reproducible and free of stale/free pages."""
    if not src_db.exists():
        # No DB yet — emit a valid empty SQLite file so import has a target.
        mem = sqlite3.connect(":memory:")
        try:
            return mem.serialize()  # type: ignore[attr-defined]
        finally:
            mem.close()

    src = sqlite3.connect(f"file:{src_db}?mode=ro", uri=True)
    src.row_factory = None
    dst = sqlite3.connect(":memory:")
    try:
        tables = _base_tables(src)
        with dst:
            for table in tables:
                ddl = _table_ddl(src, table)
                if not ddl:
                    continue
                dst.execute(ddl)
                cols = _column_names(src, table)
                if not cols:
                    continue
                rows = _ordered_rows(src, table, cols)
                if rows:
                    placeholders = ", ".join("?" for _ in cols)
                    col_list = ", ".join(f'"{c}"' for c in cols)
                    dst.executemany(
                        f'INSERT INTO "{table}" ({col_list}) VALUES ({placeholders})',
                        rows,
                    )
            # Recreate indexes so the restored DB matches the source schema.
            for idx_sql in _index_ddls(src):
                try:
                    dst.execute(idx_sql)
                except sqlite3.OperationalError:
                    # Index over a column we didn't recreate — skip rather than
                    # abort the whole export.
                    pass
        return dst.serialize()  # type: ignore[attr-defined]
    finally:
        src.close()
        dst.close()


def _add_bytes(tar: tarfile.TarFile, arcname: str, payload: bytes) -> None:
    """Append *payload* under *arcname* with fully-normalized metadata."""
    info = tarfile.TarInfo(name=arcname)
    info.size = len(payload)
    info.mtime = _FIXED_MTIME
    info.mode = _FIXED_MODE
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    info.type = tarfile.REGTYPE
    tar.addfile(info, io.BytesIO(payload))


def export_kb(
    tarball: Path,
    *,
    db_path: Optional[Path] = None,
    learnings_home: Optional[Path] = None,
) -> dict:
    """Export the KB at (*db_path*, *learnings_home*) to *tarball*.

    Returns a manifest dict (also embedded in the tar as ``manifest.json``).
    The tarball is deterministic: same KB → byte-identical tar.
    """
    db_path = (db_path or _default_db_path()).expanduser()
    learnings_home = (learnings_home or _default_learnings_home()).expanduser()
    tarball = Path(tarball).expanduser()
    tarball.parent.mkdir(parents=True, exist_ok=True)

    docs = _list_documents(learnings_home)
    docs_dir = learnings_home / "documents"
    db_bytes = build_db_snapshot(db_path)

    # Per-table row counts for the manifest (import validates against these).
    table_counts: dict[str, int] = {}
    if db_bytes:
        snap = sqlite3.connect(":memory:")
        try:
            snap.deserialize(db_bytes)  # type: ignore[attr-defined]
            for table in _base_tables(snap):
                table_counts[table] = snap.execute(
                    f'SELECT COUNT(*) FROM "{table}"'
                ).fetchone()[0]
        finally:
            snap.close()

    manifest = {
        "format_version": EXPORT_FORMAT_VERSION,
        "kind": "reflect-kb-export",
        "documents": [d.relative_to(docs_dir).as_posix() for d in docs],
        "document_count": len(docs),
        "db_file": "reflect.db",
        "tables": table_counts,
    }

    # Collect every member as (arcname, payload), then emit in globally-sorted
    # arcname order so the tar diffs cleanly under git (stable member layout).
    members: list[tuple[str, bytes]] = [
        (
            "manifest.json",
            json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8") + b"\n",
        ),
        ("reflect.db", db_bytes),
    ]
    for d in docs:
        arc = "documents/" + d.relative_to(docs_dir).as_posix()
        members.append((arc, d.read_bytes()))
    members.sort(key=lambda m: m[0])

    # gzip carries its own mtime; use an uncompressed tar so the bytes are fully
    # deterministic and the tar diffs cleanly under git. (Callers wanting
    # compression can gzip with mtime=0 downstream.)
    with tarfile.open(tarball, "w") as tar:
        for arc, payload in members:
            _add_bytes(tar, arc, payload)

    return manifest


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="kb_export.py",
        description="Snapshot a reflect-kb (learnings + reflect.db) to a "
        "deterministic, git-friendly tarball.",
    )
    parser.add_argument("tarball", help="Output tarball path (e.g. kb.tar)")
    parser.add_argument(
        "--db", dest="db", default=None, help="Path to reflect.db (default: config)"
    )
    parser.add_argument(
        "--learnings",
        dest="learnings",
        default=None,
        help="Learnings home dir (default: ~/.learnings)",
    )
    args = parser.parse_args(argv)

    manifest = export_kb(
        Path(args.tarball),
        db_path=Path(args.db) if args.db else None,
        learnings_home=Path(args.learnings) if args.learnings else None,
    )
    print(
        f"Exported {manifest['document_count']} documents and "
        f"{sum(manifest['tables'].values())} DB rows across "
        f"{len(manifest['tables'])} tables to {args.tarball}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
