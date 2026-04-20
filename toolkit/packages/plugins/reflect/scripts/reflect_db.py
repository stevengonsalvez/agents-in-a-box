#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
SQLite State Manager for Reflect.

Provides a single-file database (~/.reflect/reflect.db by default) that
replaces the previous YAML-based state, metrics, and learnings files.

All public write functions use ``with conn:`` for transactional safety.
"""

import hashlib
import json
import sqlite3
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from reflect_config import get_config, resolve_path

# ---------------------------------------------------------------------------
# Schema
# ---------------------------------------------------------------------------

# DDL for the learnings table kept as a constant so the CHECK-constraint
# rebuild migration can recreate it identically without duplicating the SQL.
_LEARNINGS_DDL = """
CREATE TABLE IF NOT EXISTS learnings (
    id              TEXT PRIMARY KEY,
    title           TEXT NOT NULL,
    category        TEXT NOT NULL DEFAULT 'Unknown',
    confidence      TEXT NOT NULL DEFAULT 'LOW',
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'approved', 'rejected', 'indexed', 'reverted')),
    source_tool     TEXT NOT NULL DEFAULT '',
    source_path     TEXT NOT NULL DEFAULT '',
    content_hash    TEXT NOT NULL DEFAULT '',
    commit_hash     TEXT,
    created_at      TEXT NOT NULL,
    approved_at     TEXT,
    indexed_at      TEXT,
    reverted_at     TEXT,
    revert_reason   TEXT
);
"""

_SCHEMA_SQL = _LEARNINGS_DDL + """
CREATE TABLE IF NOT EXISTS proposals (
    id              TEXT PRIMARY KEY,
    learning_id     TEXT NOT NULL REFERENCES learnings(id),
    agent_file      TEXT NOT NULL DEFAULT '',
    diff            TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'accepted', 'rejected')),
    created_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS metrics (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id              TEXT PRIMARY KEY,
    type            TEXT NOT NULL,
    learning_id     TEXT,
    details_json    TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sources (
    id              TEXT PRIMARY KEY,
    provider        TEXT NOT NULL,
    path            TEXT NOT NULL,
    project_name    TEXT NOT NULL DEFAULT '',
    content_hash    TEXT NOT NULL DEFAULT '',
    last_seen       TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'active'
                    CHECK (status IN ('active', 'stale', 'archived'))
);

CREATE INDEX IF NOT EXISTS idx_learnings_status ON learnings(status);
CREATE INDEX IF NOT EXISTS idx_learnings_source_tool ON learnings(source_tool);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(type);
CREATE INDEX IF NOT EXISTS idx_sources_provider ON sources(provider);
CREATE INDEX IF NOT EXISTS idx_sources_status ON sources(status);
"""

# ---------------------------------------------------------------------------
# Legacy v2 state paths (relative to Path.home())
# ---------------------------------------------------------------------------

# Exposed so migrate_v2 and the doctor CLI agree on the canonical list.
LEGACY_V2_PATHS: tuple[Path, ...] = (
    Path(".claude") / "session" / "reflect-state.yaml",
    Path(".claude") / "session" / "reflect-metrics.yaml",
    Path(".claude") / "session" / "learnings.yaml",
    Path(".reflect") / "reflect-state.yaml",
    Path(".reflect") / "reflect-metrics.yaml",
    Path(".reflect") / "learnings.yaml",
    Path(".claude") / "reflections",
)


def has_legacy_state() -> bool:
    """Return True if any legacy v2 YAML state or a non-empty reflections dir exists.

    Shallow-only: we don't rglob the reflections directory because a legacy
    tree with thousands of nested files would make this O(files) on every
    call; a top-level iterdir is enough to answer "is there anything here".
    """
    home = Path.home()
    for rel in LEGACY_V2_PATHS:
        p = home / rel
        if p.is_file():
            return True
        if p.is_dir():
            try:
                if any(p.iterdir()):
                    return True
            except OSError:
                continue
    return False


def get_legacy_state_summary() -> Optional[str]:
    """Return the one-line doctor message, or None when nothing is found."""
    home = Path.home()
    yaml_found: list[Path] = []
    reflections_present = False
    for rel in LEGACY_V2_PATHS:
        p = home / rel
        if p.is_file():
            yaml_found.append(p)
        elif p.is_dir():
            try:
                if any(p.iterdir()):
                    reflections_present = True
            except OSError:
                continue
    if not yaml_found and not reflections_present:
        return None
    script = Path(__file__).resolve().parent / "migrate_v2.py"
    return (
        "[reflect] Legacy v2 state detected ("
        f"{len(yaml_found)} YAML file(s)"
        + (", reflections/ present" if reflections_present else "")
        + "). "
        "Run: python3 " + str(script) + " --execute"
    )


# ---------------------------------------------------------------------------
# Connection management
# ---------------------------------------------------------------------------

_CONN_CACHE: dict[str, sqlite3.Connection] = {}


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _new_id() -> str:
    return uuid.uuid4().hex[:16]


def _db_path() -> Path:
    cfg = get_config()
    raw = cfg.get("storage", {}).get("db_path", "~/.reflect/reflect.db")
    return resolve_path(raw)


def db_path() -> Path:
    """Public accessor for the resolved DB path."""
    return _db_path()


def init_db(path: Optional[Path] = None) -> sqlite3.Connection:
    """Create tables if they don't exist and return a connection.

    The connection is cached per path so callers can call ``init_db()``
    repeatedly without opening duplicate handles. Legacy-state warning
    logic lives in the CLI doctor command; keeping init_db free of IO
    beyond the DB itself avoids per-call home-dir scans.
    """
    if path is None:
        path = _db_path()

    key = str(path)
    if key in _CONN_CACHE:
        return _CONN_CACHE[key]

    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(path))
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA foreign_keys=ON")
    conn.executescript(_SCHEMA_SQL)
    _migrate_schema(conn)

    _CONN_CACHE[key] = conn
    return conn


def _warn_if_legacy_state_exists() -> None:
    """Print the legacy-state reminder to stderr, if any is found.

    Callable only from the CLI doctor command; no longer invoked from
    init_db. Any IO error is swallowed because warning logic must never
    escalate into a user-facing failure.
    """
    try:
        msg = get_legacy_state_summary()
        if msg is None:
            return
        import sys as _sys
        print(msg, file=_sys.stderr)
    except Exception:  # noqa: BLE001 - warnings must never raise
        return


def _migrate_schema(conn: sqlite3.Connection) -> None:
    """Idempotent migrations for existing DBs.

    Two phases:

    1. Additive ALTERs for columns added after v3.0: ``commit_hash``,
       ``reverted_at``, ``revert_reason``.
    2. CHECK-constraint rebuild when the existing ``learnings`` table was
       created before ``'reverted'`` was added to the status CHECK. SQLite's
       ``CREATE TABLE IF NOT EXISTS`` is a no-op for existing tables, so
       without this rebuild an old DB will reject UPDATE status='reverted'.
    """
    cols = {row[1] for row in conn.execute("PRAGMA table_info(learnings)").fetchall()}
    alters = []
    if "commit_hash" not in cols:
        alters.append("ALTER TABLE learnings ADD COLUMN commit_hash TEXT")
    if "reverted_at" not in cols:
        alters.append("ALTER TABLE learnings ADD COLUMN reverted_at TEXT")
    if "revert_reason" not in cols:
        alters.append("ALTER TABLE learnings ADD COLUMN revert_reason TEXT")
    if alters:
        with conn:
            for sql in alters:
                conn.execute(sql)

    # CHECK-constraint rebuild: detect a stale CHECK that predates 'reverted'.
    row = conn.execute(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='learnings'"
    ).fetchone()
    table_sql = row[0] if row else ""
    if table_sql and "'reverted'" not in table_sql:
        # Indexes reference the table name, so SQLite drops them automatically
        # on rename. We re-create them via _SCHEMA_SQL after the rebuild.
        with conn:
            conn.execute("DROP INDEX IF EXISTS idx_learnings_status")
            conn.execute("DROP INDEX IF EXISTS idx_learnings_source_tool")
            conn.execute("ALTER TABLE learnings RENAME TO learnings_old")
            conn.executescript(_LEARNINGS_DDL)
            # Column set is stable between old and new — additive ALTERs above
            # already brought pre-rebuild DBs up to the current column set.
            conn.execute(
                "INSERT INTO learnings SELECT "
                "id, title, category, confidence, status, source_tool, "
                "source_path, content_hash, commit_hash, created_at, "
                "approved_at, indexed_at, reverted_at, revert_reason "
                "FROM learnings_old"
            )
            conn.execute("DROP TABLE learnings_old")
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_learnings_status ON learnings(status)"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_learnings_source_tool ON learnings(source_tool)"
            )


def get_conn(path: Optional[Path] = None) -> sqlite3.Connection:
    """Return (and lazily create) the database connection."""
    return init_db(path)


# ---------------------------------------------------------------------------
# Shared helpers (used by reflect_db and migrate_v2)
# ---------------------------------------------------------------------------


def compute_content_hash(payload: dict[str, Any]) -> str:
    """Stable 16-hex-char SHA-256 prefix over canonical JSON of *payload*.

    Pure: performs no DB access. The 16-char prefix matches the width used
    by other ids in this module and keeps content_hash small in the row.
    """
    blob = json.dumps(payload, sort_keys=True, default=str).encode("utf-8")
    return hashlib.sha256(blob).hexdigest()[:16]


def get_known_content_hashes(
    *, conn: Optional[sqlite3.Connection] = None,
) -> set[str]:
    """Return the set of distinct non-empty content_hash values in learnings."""
    conn = conn or get_conn()
    rows = conn.execute(
        "SELECT DISTINCT content_hash FROM learnings WHERE content_hash != ''"
    ).fetchall()
    return {r["content_hash"] for r in rows}


def get_events_by_type(
    event_type: str,
    *,
    limit: int = 10_000,
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Thin wrapper around ``get_events`` scoped to a single event type."""
    return get_events(event_type=event_type, limit=limit, conn=conn)


# ---------------------------------------------------------------------------
# Learnings
# ---------------------------------------------------------------------------


def add_learning(
    title: str,
    category: str = "Unknown",
    confidence: str = "LOW",
    source_tool: str = "",
    source_path: str = "",
    content_hash: str = "",
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Insert a new learning row. Returns the generated id.

    The insert and the accompanying ``learning_added`` audit event are
    written in a single transaction so a crash between them cannot leave
    an un-audited row.
    """
    conn = conn or get_conn()
    lid = _new_id()
    with conn:
        conn.execute(
            """INSERT INTO learnings
               (id, title, category, confidence, status, source_tool,
                source_path, content_hash, created_at)
               VALUES (?, ?, ?, ?, 'pending', ?, ?, ?, ?)""",
            (lid, title, category, confidence, source_tool,
             source_path, content_hash, _now_iso()),
        )
        add_event(
            "learning_added", lid, {"title": title},
            conn=conn, autocommit=False,
        )
    return lid


def update_learning_status(
    learning_id: str,
    status: str,
    *,
    revert_reason: Optional[str] = None,
    commit_hash: Optional[str] = None,
    conn: Optional[sqlite3.Connection] = None,
) -> None:
    """Transition a learning to *status*.

    Valid statuses: pending, approved, rejected, indexed, reverted.

    For status='reverted', pass *revert_reason* (short explanation) and
    optionally *commit_hash* (the commit where the learning was backed out).

    The UPDATE and the ``status_change`` audit event commit atomically.
    """
    conn = conn or get_conn()
    now = _now_iso()
    extras: dict[str, Any] = {}
    if status == "approved":
        extras["approved_at"] = now
    elif status == "indexed":
        extras["indexed_at"] = now
    elif status == "reverted":
        extras["reverted_at"] = now
        if revert_reason is not None:
            extras["revert_reason"] = revert_reason
    if commit_hash is not None:
        extras["commit_hash"] = commit_hash

    set_parts = ["status = ?"]
    params: list[Any] = [status]
    for col, val in extras.items():
        set_parts.append(f"{col} = ?")
        params.append(val)
    params.append(learning_id)

    details: dict[str, Any] = {"new_status": status}
    if revert_reason:
        details["revert_reason"] = revert_reason
    if commit_hash:
        details["commit_hash"] = commit_hash

    with conn:
        conn.execute(
            f"UPDATE learnings SET {', '.join(set_parts)} WHERE id = ?",
            params,
        )
        add_event(
            "status_change", learning_id, details,
            conn=conn, autocommit=False,
        )


def get_pending_learnings(
    *, conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Return all learnings with status='pending'."""
    conn = conn or get_conn()
    rows = conn.execute(
        "SELECT * FROM learnings WHERE status = 'pending' ORDER BY created_at"
    ).fetchall()
    return [dict(r) for r in rows]


def get_learning(
    learning_id: str, *, conn: Optional[sqlite3.Connection] = None,
) -> Optional[dict[str, Any]]:
    """Fetch a single learning by id."""
    conn = conn or get_conn()
    row = conn.execute(
        "SELECT * FROM learnings WHERE id = ?", (learning_id,)
    ).fetchone()
    return dict(row) if row else None


# ---------------------------------------------------------------------------
# Proposals
# ---------------------------------------------------------------------------


def add_proposal(
    learning_id: str,
    agent_file: str = "",
    diff: str = "",
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Insert a new proposal. Returns the generated id."""
    conn = conn or get_conn()
    pid = _new_id()
    with conn:
        conn.execute(
            """INSERT INTO proposals
               (id, learning_id, agent_file, diff, status, created_at)
               VALUES (?, ?, ?, ?, 'pending', ?)""",
            (pid, learning_id, agent_file, diff, _now_iso()),
        )
    return pid


# ---------------------------------------------------------------------------
# Metrics (key-value store)
# ---------------------------------------------------------------------------


def set_metric(
    key: str,
    value: Any,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> None:
    """Upsert a metric value (stored as JSON string)."""
    conn = conn or get_conn()
    serialized = json.dumps(value) if not isinstance(value, str) else value
    with conn:
        conn.execute(
            """INSERT INTO metrics (key, value, updated_at)
               VALUES (?, ?, ?)
               ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                              updated_at = excluded.updated_at""",
            (key, serialized, _now_iso()),
        )


def get_metric(
    key: str,
    default: Any = None,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> Any:
    """Read a single metric value. Returns *default* if not found."""
    conn = conn or get_conn()
    row = conn.execute(
        "SELECT value FROM metrics WHERE key = ?", (key,)
    ).fetchone()
    if row is None:
        return default
    raw = row["value"]
    try:
        return json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return raw


def get_metrics(
    *, conn: Optional[sqlite3.Connection] = None,
) -> dict[str, Any]:
    """Return all metrics as a flat dict."""
    conn = conn or get_conn()
    rows = conn.execute("SELECT key, value FROM metrics").fetchall()
    result: dict[str, Any] = {}
    for r in rows:
        try:
            result[r["key"]] = json.loads(r["value"])
        except (json.JSONDecodeError, TypeError):
            result[r["key"]] = r["value"]
    return result


def increment_metric(
    key: str,
    delta: int = 1,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> int:
    """Atomically increment an integer metric. Returns new value."""
    conn = conn or get_conn()
    current = get_metric(key, 0, conn=conn)
    if not isinstance(current, (int, float)):
        current = 0
    new_val = int(current) + delta
    set_metric(key, new_val, conn=conn)
    return new_val


# ---------------------------------------------------------------------------
# Events (audit trail)
# ---------------------------------------------------------------------------


def add_event(
    event_type: str,
    learning_id: Optional[str] = None,
    details: Optional[dict[str, Any]] = None,
    *,
    conn: Optional[sqlite3.Connection] = None,
    autocommit: bool = True,
) -> str:
    """Insert an audit event. Returns the event id.

    ``autocommit=False`` lets callers nest the insert inside an outer
    ``with conn:`` so the event commits atomically with the row that
    produced it. Default behaviour is unchanged for external callers.
    """
    conn = conn or get_conn()
    eid = _new_id()
    sql = """INSERT INTO events (id, type, learning_id, details_json, created_at)
             VALUES (?, ?, ?, ?, ?)"""
    params = (
        eid, event_type, learning_id,
        json.dumps(details or {}), _now_iso(),
    )
    if autocommit:
        with conn:
            conn.execute(sql, params)
    else:
        conn.execute(sql, params)
    return eid


def get_events(
    event_type: Optional[str] = None,
    limit: int = 100,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Fetch recent events, optionally filtered by type."""
    conn = conn or get_conn()
    if event_type:
        rows = conn.execute(
            "SELECT * FROM events WHERE type = ? ORDER BY created_at DESC LIMIT ?",
            (event_type, limit),
        ).fetchall()
    else:
        rows = conn.execute(
            "SELECT * FROM events ORDER BY created_at DESC LIMIT ?",
            (limit,),
        ).fetchall()
    return [dict(r) for r in rows]


# ---------------------------------------------------------------------------
# Sources
# ---------------------------------------------------------------------------


def upsert_source(
    provider: str,
    path: str,
    project_name: str = "",
    content_hash: str = "",
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Insert or update a discovered source. Returns the source id."""
    conn = conn or get_conn()
    now = _now_iso()

    existing = conn.execute(
        "SELECT id FROM sources WHERE provider = ? AND path = ?",
        (provider, path),
    ).fetchone()

    if existing:
        sid = existing["id"]
        with conn:
            conn.execute(
                """UPDATE sources
                   SET content_hash = ?, last_seen = ?, status = 'active',
                       project_name = ?
                   WHERE id = ?""",
                (content_hash, now, project_name, sid),
            )
        return sid

    sid = _new_id()
    with conn:
        conn.execute(
            """INSERT INTO sources
               (id, provider, path, project_name, content_hash, last_seen, status)
               VALUES (?, ?, ?, ?, ?, ?, 'active')""",
            (sid, provider, path, project_name, content_hash, now),
        )
    return sid


def get_stale_sources(
    days: int = 30,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Return sources not seen within *days*."""
    conn = conn or get_conn()
    rows = conn.execute(
        """SELECT * FROM sources
           WHERE julianday('now') - julianday(last_seen) > ?
           ORDER BY last_seen""",
        (days,),
    ).fetchall()
    return [dict(r) for r in rows]


def mark_sources_stale(
    days: int = 30,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> int:
    """Mark sources not seen within *days* as stale. Returns count affected."""
    conn = conn or get_conn()
    with conn:
        cur = conn.execute(
            """UPDATE sources SET status = 'stale'
               WHERE status = 'active'
                 AND julianday('now') - julianday(last_seen) > ?""",
            (days,),
        )
    return cur.rowcount


# ---------------------------------------------------------------------------
# CLI — quick diagnostics
# ---------------------------------------------------------------------------


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="Reflect SQLite manager")
    parser.add_argument(
        "command",
        choices=["init", "stats", "events", "doctor"],
        help="Action to perform",
    )
    parser.add_argument("--limit", type=int, default=20)
    args = parser.parse_args()

    conn = init_db()

    if args.command == "init":
        print(f"Database initialized at {_db_path()}")

    elif args.command == "stats":
        for table in ("learnings", "proposals", "metrics", "events", "sources"):
            count = conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
            print(f"  {table}: {count} rows")

    elif args.command == "events":
        for ev in get_events(limit=args.limit, conn=conn):
            print(f"  [{ev['created_at']}] {ev['type']}  "
                  f"learning={ev['learning_id'] or '-'}  "
                  f"{ev['details_json']}")

    elif args.command == "doctor":
        msg = get_legacy_state_summary()
        if msg is None:
            print("[reflect] no legacy v2 state found")
        else:
            print(msg)


if __name__ == "__main__":
    main()
