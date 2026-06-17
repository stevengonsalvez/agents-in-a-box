# ABOUTME: Behavioral proof for C5 — `reflect kb export`/`import` round-trips a
# ABOUTME: real reflect knowledge base (learning docs + sidecars + reflect.db
# ABOUTME: signal/learning rows) into a tarball and restores it byte/row-
# ABOUTME: identically into a FRESH empty target, with a deterministic tarball.
"""C5 KB export/import round-trip fidelity proof.

Port C5 (surface=consolidation/cli) is the Hindsight ``export-bank`` /
``import-bank`` port. It ships two NEW, directly-invokable plugin scripts —
``plugins/reflect/scripts/kb_export.py`` and ``kb_import.py`` — that snapshot
``~/.learnings/documents/`` + the ``reflect.db`` user tables into a single
git-friendly tarball, then restore them onto a fresh machine so a KB can move
between machines without re-running every drain.

We drive the REAL ``kb_export.export_kb`` and ``kb_import.import_kb`` against a
REAL ``reflect.db`` built via the production ``reflect_db`` module and REAL
on-disk learning notes + entity sidecars. No LLM, no embedding model, no
GraphRAG: export/import is pure file + SQLite plumbing, and the assertions are
byte-equality of documents and row-equality of DB tables — fully deterministic
and decided entirely by the bytes we seed, never by a model.

Invariant (the seed + the two functions fully determine every assertion):

  A. ROUND-TRIP FIDELITY (the C5 PROOF INVARIANT). Build a small real KB — a
     couple learning notes (.md) with their entity sidecars (.entities.yaml) and
     a reflect.db carrying learning rows + signal rows (``learning_signals`` +
     signal ``events``) — export it to a tarball, then import into a FRESH,
     genuinely-empty target dir. Every restored document is BYTE-identical to its
     source, and every restored DB table is ROW-identical (same rows, same
     values) to the source. Nothing is dropped, mutated, or reordered.

  B. SIGNAL ROWS SURVIVE (decisive sub-claim). The signal-bearing rows
     specifically — the ``learning_signals`` table and the signal-typed
     ``events`` — are present and value-identical after the round-trip. This is
     the half the port exists for: signals are the expensive drain output we
     refuse to recompute on the new machine.

  C. DETERMINISTIC, ABSOLUTE-PATH-FREE TARBALL (the git-friendly ACCEPTANCE
     bar). Exporting the SAME KB twice yields byte-identical tarballs (stable
     member ordering + normalized tar metadata), and NO archive member carries
     an absolute path or escapes the documents/ + reflect.db layout — so the
     tarball diffs cleanly under git and leaks no machine paths.

  D. REFUSE-TO-MERGE GUARD (hindsight "the target bank must not already
     exist"). Importing onto a target that already holds learnings is REFUSED
     (raises) unless force=True — import restores a whole KB, never a silent
     merge of two corpora.

Falsifiability: if export dropped a sidecar/table or import mutated a value,
arm A's byte/row-equality FAILS. If the signal rows were filtered out, arm B
FAILS. If tar ordering or metadata were nondeterministic, or an absolute path
leaked, arm C FAILS. If import silently merged into a populated target, arm D's
``pytest.raises`` FAILS.

PORT: C5
"""
from __future__ import annotations

import hashlib
import sqlite3
import sys
import tarfile
from pathlib import Path

import pytest

# The C5 scripts live in the reflect plugin; import the REAL modules directly so
# we exercise the shipped export/import, not a copy. Path resolution mirrors the
# sibling proofs: parents[3] is the repo root where plugins/ sits alongside
# reflect-kb/; the fallback handles a reflect-kb-as-root checkout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next(
    (p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0]
)
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import kb_export as E  # noqa: E402
import kb_import as I  # noqa: E402
import reflect_db  # noqa: E402


# --- helpers ---------------------------------------------------------------


def _dump_all_tables(db_path: Path) -> dict[str, list[tuple]]:
    """Return {table: sorted(rows)} for every base table — the canonical,
    order-independent fingerprint of a DB's user data."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        tables = [
            r[0]
            for r in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' "
                "AND name NOT LIKE 'sqlite_%' ORDER BY name"
            ).fetchall()
        ]
        out: dict[str, list[tuple]] = {}
        for t in tables:
            rows = conn.execute(f'SELECT * FROM "{t}"').fetchall()
            out[t] = sorted(tuple(r) for r in rows)
        return out
    finally:
        conn.close()


def _build_real_kb(root: Path) -> tuple[Path, Path]:
    """Construct a real KB: learning docs + sidecars on disk, learnings +
    signal rows in a real reflect.db. Returns (learnings_home, db_path)."""
    learnings_home = root / ".learnings"
    docs = learnings_home / "documents"
    docs.mkdir(parents=True)
    db_path = root / ".reflect" / "reflect.db"
    db_path.parent.mkdir(parents=True)

    # Two learning notes + entity sidecars (the durable corpus). Bytes are
    # arbitrary but fixed so byte-equality is meaningful.
    (docs / "fix-keychain.md").write_text(
        "# fastlane match needs the keychain unlocked\n\n"
        "Body with a UTF-8 char: café.\n"
    )
    (docs / "fix-keychain.entities.yaml").write_text(
        "entities:\n  - name: fastlane\n    type: tool\n"
    )
    (docs / "k8s-evict.md").write_text(
        "# kubernetes pods evict under memory pressure\n"
    )

    # Real DB rows via the production module: two learnings, plus signal-bearing
    # rows (events typed as signal detections). ``add_learning`` also writes a
    # learning_signals row in this schema, which the export must carry verbatim.
    conn = reflect_db.init_db(db_path)
    l1 = reflect_db.add_learning(
        "fastlane match needs the keychain unlocked",
        category="bug",
        conn=conn,
    )
    l2 = reflect_db.add_learning(
        "kubernetes pods evict under memory pressure",
        category="ops",
        conn=conn,
    )
    reflect_db.add_event("signal_detected", learning_id=l1, conn=conn)
    reflect_db.add_event("signal_detected", learning_id=l2, conn=conn)
    reflect_db.set_metric("signals_total", "2", conn=conn)
    reflect_db.close_all()

    return learnings_home, db_path


# --- arm A + B: round-trip fidelity incl. signal rows ----------------------


def test_roundtrip_documents_and_db_byte_row_identical(tmp_path):
    """Export a real KB, import into a FRESH empty target, and assert every
    document is byte-identical and every DB table is row-identical — including
    the signal-bearing rows the port exists to preserve."""
    src_home, src_db = _build_real_kb(tmp_path / "src")

    # Fingerprint the source BEFORE export.
    src_docs = {
        p.relative_to(src_home / "documents").as_posix(): p.read_bytes()
        for p in (src_home / "documents").rglob("*")
        if p.is_file()
    }
    src_tables = _dump_all_tables(src_db)

    # Export → tarball.
    tarball = tmp_path / "kb.tar"
    manifest = E.export_kb(tarball, db_path=src_db, learnings_home=src_home)
    assert manifest["document_count"] == 3
    assert tarball.exists()

    # Import into a genuinely fresh, empty target (reindex off — no model here).
    tgt_home = tmp_path / "dst" / ".learnings"
    tgt_db = tmp_path / "dst" / ".reflect" / "reflect.db"
    assert not tgt_home.exists() and not tgt_db.exists()
    I.import_kb(tarball, db_path=tgt_db, learnings_home=tgt_home, reindex=False)

    # A: documents byte-identical, exact same set (nothing added/dropped).
    tgt_docs = {
        p.relative_to(tgt_home / "documents").as_posix(): p.read_bytes()
        for p in (tgt_home / "documents").rglob("*")
        if p.is_file()
    }
    assert tgt_docs == src_docs, "restored documents are not byte-identical"

    # A: every table row-identical (order-independent).
    tgt_tables = _dump_all_tables(tgt_db)
    assert tgt_tables == src_tables, "restored DB tables are not row-identical"

    # B: the signal-bearing rows specifically survived, value-identical.
    assert src_tables["events"], "fixture bug: no events seeded"
    signal_events = [r for r in tgt_tables["events"] if "signal_detected" in r]
    assert len(signal_events) == 2, "signal events were not preserved"
    if "learning_signals" in src_tables:
        assert tgt_tables["learning_signals"] == src_tables["learning_signals"], (
            "learning_signals rows were not preserved"
        )
    assert ("signals_total", "2") in [
        (r[0], r[1]) for r in tgt_tables.get("metrics", [])
    ], "signal metric row was not preserved"


# --- arm C: deterministic, absolute-path-free tarball ----------------------


def test_export_is_deterministic_and_path_safe(tmp_path):
    """The SAME KB exported twice yields byte-identical tarballs, and no member
    carries an absolute path or escapes the documents/ + reflect.db layout —
    the git-friendly acceptance bar."""
    src_home, src_db = _build_real_kb(tmp_path / "src")

    t1 = tmp_path / "a.tar"
    t2 = tmp_path / "b.tar"
    E.export_kb(t1, db_path=src_db, learnings_home=src_home)
    E.export_kb(t2, db_path=src_db, learnings_home=src_home)

    h1 = hashlib.sha256(t1.read_bytes()).hexdigest()
    h2 = hashlib.sha256(t2.read_bytes()).hexdigest()
    assert h1 == h2, "two exports of the same KB are not byte-identical"

    with tarfile.open(t1, "r") as tar:
        names = tar.getnames()
        infos = tar.getmembers()

    # Members are sorted (stable diffs) and relative (no machine paths leak).
    assert names == sorted(names), "tar members are not in sorted order"
    for n in names:
        assert not n.startswith("/"), f"absolute path leaked into archive: {n!r}"
        assert ".." not in Path(n).parts, f"parent-escape path in archive: {n!r}"
        assert n in {"manifest.json", "reflect.db"} or n.startswith("documents/"), (
            f"unexpected archive member outside the C5 layout: {n!r}"
        )

    # Tar metadata is normalized → reproducible bytes.
    for info in infos:
        assert info.mtime == 0, f"{info.name}: non-zero mtime breaks determinism"
        assert info.uid == 0 and info.gid == 0
        assert info.uname == "" and info.gname == ""


# --- arm D: refuse-to-merge guard ------------------------------------------


def test_import_refuses_populated_target(tmp_path):
    """Importing onto a target that already holds learnings is refused unless
    force=True — import restores a whole KB, never a silent merge."""
    src_home, src_db = _build_real_kb(tmp_path / "src")
    tarball = tmp_path / "kb.tar"
    E.export_kb(tarball, db_path=src_db, learnings_home=src_home)

    # Pre-populate the target with a different KB.
    tgt_home, tgt_db = _build_real_kb(tmp_path / "dst")

    with pytest.raises(I.ImportError_):
        I.import_kb(tarball, db_path=tgt_db, learnings_home=tgt_home, reindex=False)

    # force=True overrides the guard and completes the restore.
    I.import_kb(
        tarball, db_path=tgt_db, learnings_home=tgt_home, reindex=False, force=True
    )
    restored = _dump_all_tables(tgt_db)
    assert restored == _dump_all_tables(src_db), "force import did not restore source"
