#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# ABOUTME: `reflect kb import <tarball>` — restores a kb_export.py snapshot onto
# ABOUTME: a FRESH machine: validates the manifest, unpacks documents + the
# ABOUTME: filtered reflect.db, then (optionally) rebuilds the GraphRAG index so
# ABOUTME: recall works without re-running every drain.
"""
KB import (C5, Hindsight admin import-bank port).

Restores a tarball produced by ``kb_export.py``:

1. **Validate.** Read ``manifest.json``; reject wrong ``kind`` / unsupported
   ``format_version``. Cross-check that every document listed in the manifest is
   present in the archive and that the embedded ``reflect.db`` row counts match.
2. **Refuse to merge.** Like hindsight's ``import-bank`` ("the target bank must
   not already exist"), import restores a *whole* KB, not a delta. If the target
   learnings dir already holds documents or the target DB already holds
   learnings, we abort unless ``--force`` is given. This prevents silently
   interleaving two corpora.
3. **Restore.** Unpack ``documents/`` under ``<target>/documents`` and write
   ``reflect.db`` to the target DB path. Paths in the tar are relative, so a
   path-traversal guard rejects any member that escapes the target.
4. **Reindex.** GraphRAG / vector state is derived and was *not* exported, so we
   rebuild it via the ``reflect`` CLI. Reindex is best-effort and skippable
   (``--no-reindex``) — the corpus + DB are already round-trip-faithful without
   it, and offline/test environments have no embedding model.

Invoke directly:

    python kb_import.py kb.tar
    python kb_import.py kb.tar --db /tmp/t/reflect.db --learnings /tmp/t/.learnings --no-reindex
"""

from __future__ import annotations

import argparse
import json
import shutil
import sqlite3
import subprocess
import sys
import tarfile
from pathlib import Path
from typing import Optional

# Keep in lockstep with kb_export.EXPORT_FORMAT_VERSION.
SUPPORTED_FORMAT_VERSIONS = frozenset({1})


class ImportError_(Exception):
    """Raised on a malformed/incompatible/conflicting import."""


def _default_db_path() -> Path:
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import reflect_db  # type: ignore

        return reflect_db.db_path()
    except Exception:
        return Path.home() / ".reflect" / "reflect.db"


def _default_learnings_home() -> Path:
    return Path.home() / ".learnings"


def _read_manifest(tar: tarfile.TarFile) -> dict:
    try:
        member = tar.getmember("manifest.json")
    except KeyError as exc:
        raise ImportError_("archive has no manifest.json — not a reflect-kb export") from exc
    fh = tar.extractfile(member)
    if fh is None:
        raise ImportError_("manifest.json is not a regular file")
    manifest = json.loads(fh.read().decode("utf-8"))
    if manifest.get("kind") != "reflect-kb-export":
        raise ImportError_(
            f"unexpected archive kind {manifest.get('kind')!r} "
            "(expected 'reflect-kb-export')"
        )
    version = manifest.get("format_version")
    if version not in SUPPORTED_FORMAT_VERSIONS:
        raise ImportError_(
            f"unsupported export format_version {version!r} "
            f"(supported: {sorted(SUPPORTED_FORMAT_VERSIONS)})"
        )
    return manifest


def _safe_extract_path(target_root: Path, arcname: str) -> Path:
    """Resolve *arcname* under *target_root*, rejecting traversal escapes."""
    dest = (target_root / arcname).resolve()
    root = target_root.resolve()
    if root != dest and root not in dest.parents:
        raise ImportError_(f"unsafe path in archive: {arcname!r}")
    return dest


def _target_is_populated(db_path: Path, learnings_home: Path) -> bool:
    """True if the target already holds a KB we'd risk merging into."""
    docs_dir = learnings_home / "documents"
    if docs_dir.is_dir() and any(
        p.is_file() and p.name.endswith((".md", ".entities.yaml"))
        for p in docs_dir.rglob("*")
    ):
        return True
    if db_path.exists():
        try:
            conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
            try:
                row = conn.execute(
                    "SELECT name FROM sqlite_master WHERE type='table' AND name='learnings'"
                ).fetchone()
                if row:
                    count = conn.execute("SELECT COUNT(*) FROM learnings").fetchone()[0]
                    if count > 0:
                        return True
            finally:
                conn.close()
        except sqlite3.Error:
            return True  # unreadable but present — treat as populated, be safe
    return False


def _validate_archive(tar: tarfile.TarFile, manifest: dict) -> None:
    """Cross-check the manifest against the archive's actual contents."""
    names = set(tar.getnames())
    db_file = manifest.get("db_file", "reflect.db")
    if db_file not in names:
        raise ImportError_(f"manifest lists db_file {db_file!r} but it is absent from the archive")
    for rel in manifest.get("documents", []):
        arc = "documents/" + rel
        if arc not in names:
            raise ImportError_(f"manifest lists document {rel!r} but it is absent from the archive")

    # Embedded DB row counts must match the manifest inventory.
    member = tar.getmember(db_file)
    fh = tar.extractfile(member)
    if fh is None:
        raise ImportError_("reflect.db member is not a regular file")
    db_bytes = fh.read()
    if db_bytes:
        snap = sqlite3.connect(":memory:")
        try:
            snap.deserialize(db_bytes)  # type: ignore[attr-defined]
            for table, expected in manifest.get("tables", {}).items():
                got = snap.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0]
                if got != expected:
                    raise ImportError_(
                        f"table {table!r} row count mismatch: "
                        f"manifest={expected} archive={got}"
                    )
        finally:
            snap.close()


def _reindex(learnings_home: Path) -> bool:
    """Rebuild GraphRAG/vector index via the reflect CLI. Best-effort."""
    cli = shutil.which("reflect")
    if not cli:
        print("reindex skipped: 'reflect' CLI not on PATH", file=sys.stderr)
        return False
    try:
        subprocess.run(
            [cli, "kb", "reindex"],
            check=True,
            cwd=str(learnings_home),
            timeout=600,
        )
        return True
    except (subprocess.SubprocessError, OSError) as exc:
        print(f"reindex skipped: {exc}", file=sys.stderr)
        return False


def import_kb(
    tarball: Path,
    *,
    db_path: Optional[Path] = None,
    learnings_home: Optional[Path] = None,
    reindex: bool = True,
    force: bool = False,
) -> dict:
    """Restore *tarball* into (*db_path*, *learnings_home*).

    Returns the manifest. Raises ``ImportError_`` on a malformed archive or a
    populated target (unless *force*).
    """
    tarball = Path(tarball).expanduser()
    db_path = (db_path or _default_db_path()).expanduser()
    learnings_home = (learnings_home or _default_learnings_home()).expanduser()

    if not tarball.exists():
        raise ImportError_(f"tarball not found: {tarball}")

    with tarfile.open(tarball, "r:*") as tar:
        manifest = _read_manifest(tar)
        _validate_archive(tar, manifest)

        if not force and _target_is_populated(db_path, learnings_home):
            raise ImportError_(
                "target KB is not empty (import restores a whole KB, not a "
                "merge). Pass force=True / --force to overwrite."
            )

        docs_dir = learnings_home / "documents"
        docs_dir.mkdir(parents=True, exist_ok=True)
        db_path.parent.mkdir(parents=True, exist_ok=True)

        # Restore the DB snapshot.
        db_member = tar.getmember(manifest.get("db_file", "reflect.db"))
        db_fh = tar.extractfile(db_member)
        if db_fh is None:
            raise ImportError_("reflect.db member is not a regular file")
        db_path.write_bytes(db_fh.read())

        # Restore documents (path-traversal guarded, relative paths only).
        for rel in manifest.get("documents", []):
            arc = "documents/" + rel
            dest = _safe_extract_path(learnings_home, arc)
            member = tar.getmember(arc)
            src_fh = tar.extractfile(member)
            if src_fh is None:
                raise ImportError_(f"document {rel!r} is not a regular file")
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(src_fh.read())

    if reindex:
        _reindex(learnings_home)

    return manifest


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="kb_import.py",
        description="Restore a reflect-kb export tarball onto this machine.",
    )
    parser.add_argument("tarball", help="Input tarball produced by kb_export.py")
    parser.add_argument("--db", dest="db", default=None, help="Target reflect.db path")
    parser.add_argument(
        "--learnings", dest="learnings", default=None, help="Target learnings home dir"
    )
    parser.add_argument(
        "--no-reindex",
        dest="reindex",
        action="store_false",
        help="Skip GraphRAG reindex after restore",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Overwrite a non-empty target (default: refuse)",
    )
    args = parser.parse_args(argv)

    try:
        manifest = import_kb(
            Path(args.tarball),
            db_path=Path(args.db) if args.db else None,
            learnings_home=Path(args.learnings) if args.learnings else None,
            reindex=args.reindex,
            force=args.force,
        )
    except ImportError_ as exc:
        print(f"import failed: {exc}", file=sys.stderr)
        return 1

    print(
        f"Imported {manifest['document_count']} documents and "
        f"{sum(manifest['tables'].values())} DB rows across "
        f"{len(manifest['tables'])} tables"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
