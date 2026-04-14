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

_SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS learnings (
    id              TEXT PRIMARY KEY,
    title           TEXT NOT NULL,
    category        TEXT NOT NULL DEFAULT 'Unknown',
    confidence      TEXT NOT NULL DEFAULT 'LOW',
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'approved', 'rejected', 'indexed')),
    source_tool     TEXT NOT NULL DEFAULT '',
    source_path     TEXT NOT NULL DEFAULT '',
    content_hash    TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    approved_at     TEXT,
    indexed_at      TEXT
);

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


def init_db(path: Optional[Path] = None) -> sqlite3.Connection:
    """
    Create tables if they don't exist and return a connection.

    The connection is cached per path so callers can simply call
    ``init_db()`` repeatedly without opening duplicate handles.
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

    _CONN_CACHE[key] = conn
    return conn


def get_conn(path: Optional[Path] = None) -> sqlite3.Connection:
    """Return (and lazily create) the database connection."""
    return init_db(path)


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
    """Insert a new learning row. Returns the generated id."""
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
    add_event("learning_added", lid, {"title": title}, conn=conn)
    return lid


def update_learning_status(
    learning_id: str,
    status: str,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> None:
    """Transition a learning to *status* (pending/approved/rejected/indexed)."""
    conn = conn or get_conn()
    now = _now_iso()
    extras: dict[str, Any] = {}
    if status == "approved":
        extras["approved_at"] = now
    elif status == "indexed":
        extras["indexed_at"] = now

    set_parts = ["status = ?"]
    params: list[Any] = [status]
    for col, val in extras.items():
        set_parts.append(f"{col} = ?")
        params.append(val)
    params.append(learning_id)

    with conn:
        conn.execute(
            f"UPDATE learnings SET {', '.join(set_parts)} WHERE id = ?",
            params,
        )
    add_event("status_change", learning_id, {"new_status": status}, conn=conn)


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
) -> str:
    """Insert an audit event. Returns the event id."""
    conn = conn or get_conn()
    eid = _new_id()
    with conn:
        conn.execute(
            """INSERT INTO events (id, type, learning_id, details_json, created_at)
               VALUES (?, ?, ?, ?, ?)""",
            (eid, event_type, learning_id,
             json.dumps(details or {}), _now_iso()),
        )
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
    parser.add_argument("command", choices=["init", "stats", "events"],
                        help="Action to perform")
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


if __name__ == "__main__":
    main()
