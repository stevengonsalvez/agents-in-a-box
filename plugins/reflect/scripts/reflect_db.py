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

Threading contract
------------------
This module caches one ``sqlite3.Connection`` per resolved DB path in
``_CONN_CACHE`` (process-global). ``sqlite3`` connections default to
``check_same_thread=True``, which means: the *first* thread that calls
``init_db`` for a given path owns that connection forever. Other threads
hitting the cached connection raise ``ProgrammingError``.

The reflect callers today are single-threaded (CLI invocations, hooks
shelling out, headless ``claude -p`` drains). If a future caller needs
multi-threaded access, either pass an explicit ``conn=`` argument and
manage lifecycle locally, or switch the cache to ``threading.local``.

Tests reset cached connections via the public ``close_all()`` helper.
"""

from __future__ import annotations

import hashlib
import json
import sqlite3
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

from domain.enums import (
    ArtifactStatus,
    ArtifactType,
    IndexBackend,
    IndexJobStatus,
    LearningStatus,
    PrivacyLevel,
    ProposalStatus,
    ProposalType,
    SourceStatus,
)
from reflect_config import get_config, resolve_path


def _quoted_csv(values: tuple[str, ...]) -> str:
    """Concatenate *values* into a single-quoted CSV string for inline DDL.

    WARNING: callers are responsible for ensuring *values* contains only
    trusted constants (enum members defined in this codebase). Output is
    interpolated directly into ``CREATE TABLE`` statements and does NOT
    escape embedded quotes — never feed it user-controlled strings.
    """
    return ", ".join(f"'{value}'" for value in values)


LEARNING_STATUS_VALUES = tuple(status.value for status in LearningStatus)
PROPOSAL_STATUS_VALUES = tuple(status.value for status in ProposalStatus)
SOURCE_STATUS_VALUES = tuple(status.value for status in SourceStatus)
PRIVACY_LEVEL_VALUES = tuple(level.value for level in PrivacyLevel)
INDEX_JOB_STATUS_VALUES = tuple(status.value for status in IndexJobStatus)
INDEX_BACKEND_VALUES = tuple(backend.value for backend in IndexBackend)
ARTIFACT_TYPE_VALUES = tuple(artifact_type.value for artifact_type in ArtifactType)
ARTIFACT_STATUS_VALUES = tuple(status.value for status in ArtifactStatus)

_LEARNING_COLUMNS = (
    "id",
    "title",
    "category",
    "confidence",
    "status",
    "scope",
    "source_tool",
    "source_provider",
    "source_kind",
    "source_path",
    "source_quote",
    "source_quote_hash",
    "content_hash",
    "source_memory_ids",
    "proof_count",
    "session_id",
    "thread_id",
    "privacy_level",
    "artifact_path",
    "sidecar_path",
    "commit_hash",
    "supersedes_learning_id",
    "superseded_by_learning_id",
    "created_at",
    "approved_at",
    "indexed_at",
    "reverted_at",
    "revert_reason",
    "last_recalled_at",
    "recall_count",
    "helpful_count",
    "ignored_count",
    "stale_count",
    "is_latest",
)

_PROPOSAL_COLUMNS = (
    "id",
    "learning_id",
    "proposal_type",
    "target_kind",
    "target_path",
    "agent_file",
    "diff",
    "status",
    "decision_actor",
    "rationale_json",
    "created_at",
    "decided_at",
    "materialized_at",
    "materialization_error",
)

# ---------------------------------------------------------------------------
# Schema
# ---------------------------------------------------------------------------

_LEARNINGS_DDL = f"""
CREATE TABLE IF NOT EXISTS learnings (
    id                      TEXT PRIMARY KEY,
    title                   TEXT NOT NULL,
    category                TEXT NOT NULL DEFAULT 'Unknown',
    confidence              TEXT NOT NULL DEFAULT 'LOW',
    status                  TEXT NOT NULL DEFAULT '{LearningStatus.PENDING.value}'
                            CHECK (status IN ({_quoted_csv(LEARNING_STATUS_VALUES)})),
    scope                   TEXT NOT NULL DEFAULT 'project',
    source_tool             TEXT NOT NULL DEFAULT '',
    source_provider         TEXT NOT NULL DEFAULT '',
    source_kind             TEXT NOT NULL DEFAULT '',
    source_path             TEXT NOT NULL DEFAULT '',
    source_quote            TEXT NOT NULL DEFAULT '',
    source_quote_hash       TEXT NOT NULL DEFAULT '',
    content_hash            TEXT NOT NULL DEFAULT '',
    source_memory_ids       TEXT NOT NULL DEFAULT '[]',
    proof_count             INTEGER NOT NULL DEFAULT 1,
    session_id              TEXT NOT NULL DEFAULT '',
    thread_id               TEXT NOT NULL DEFAULT '',
    privacy_level           TEXT NOT NULL DEFAULT '{PrivacyLevel.INTERNAL.value}'
                            CHECK (privacy_level IN ({_quoted_csv(PRIVACY_LEVEL_VALUES)})),
    artifact_path           TEXT NOT NULL DEFAULT '',
    sidecar_path            TEXT NOT NULL DEFAULT '',
    commit_hash             TEXT,
    supersedes_learning_id  TEXT,
    superseded_by_learning_id TEXT,
    created_at              TEXT NOT NULL,
    approved_at             TEXT,
    indexed_at              TEXT,
    reverted_at             TEXT,
    revert_reason           TEXT,
    last_recalled_at        TEXT,
    recall_count            INTEGER NOT NULL DEFAULT 0,
    helpful_count           INTEGER NOT NULL DEFAULT 0,
    ignored_count           INTEGER NOT NULL DEFAULT 0,
    stale_count             INTEGER NOT NULL DEFAULT 0,
    is_latest               INTEGER NOT NULL DEFAULT 1,
    FOREIGN KEY (supersedes_learning_id) REFERENCES learnings(id),
    FOREIGN KEY (superseded_by_learning_id) REFERENCES learnings(id)
);
"""

_PROPOSALS_DDL = f"""
CREATE TABLE IF NOT EXISTS proposals (
    id                      TEXT PRIMARY KEY,
    learning_id             TEXT NOT NULL REFERENCES learnings(id),
    proposal_type           TEXT NOT NULL DEFAULT '{ProposalType.LEARNING.value}',
    target_kind             TEXT NOT NULL DEFAULT '',
    target_path             TEXT NOT NULL DEFAULT '',
    agent_file              TEXT NOT NULL DEFAULT '',
    diff                    TEXT NOT NULL DEFAULT '',
    status                  TEXT NOT NULL DEFAULT '{ProposalStatus.PENDING.value}'
                            CHECK (status IN ({_quoted_csv(PROPOSAL_STATUS_VALUES)})),
    decision_actor          TEXT NOT NULL DEFAULT '',
    rationale_json          TEXT NOT NULL DEFAULT '{{}}',
    created_at              TEXT NOT NULL,
    decided_at              TEXT,
    materialized_at         TEXT,
    materialization_error   TEXT
);
"""

_METRICS_DDL = """
CREATE TABLE IF NOT EXISTS metrics (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
"""

_EVENTS_DDL = """
CREATE TABLE IF NOT EXISTS events (
    id              TEXT PRIMARY KEY,
    type            TEXT NOT NULL,
    learning_id     TEXT,
    actor           TEXT NOT NULL DEFAULT '',
    parent_event_id TEXT,
    idempotency_key TEXT NOT NULL DEFAULT '',
    details_json    TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL
);
"""

_SOURCES_DDL = f"""
CREATE TABLE IF NOT EXISTS sources (
    id                      TEXT PRIMARY KEY,
    provider                TEXT NOT NULL,
    path                    TEXT NOT NULL,
    project_name            TEXT NOT NULL DEFAULT '',
    source_kind             TEXT NOT NULL DEFAULT '',
    provider_id             TEXT NOT NULL DEFAULT '',
    canonical_project_id    TEXT NOT NULL DEFAULT '',
    content_hash            TEXT NOT NULL DEFAULT '',
    first_seen              TEXT NOT NULL DEFAULT '',
    last_seen               TEXT NOT NULL,
    archived_at             TEXT,
    ingest_state            TEXT NOT NULL DEFAULT 'discovered',
    status                  TEXT NOT NULL DEFAULT '{SourceStatus.ACTIVE.value}'
                            CHECK (status IN ({_quoted_csv(SOURCE_STATUS_VALUES)}))
);
"""

_INDEX_JOBS_DDL = f"""
CREATE TABLE IF NOT EXISTS index_jobs (
    id              TEXT PRIMARY KEY,
    learning_id     TEXT NOT NULL REFERENCES learnings(id),
    backend         TEXT NOT NULL
                    CHECK (backend IN ({_quoted_csv(INDEX_BACKEND_VALUES)})),
    status          TEXT NOT NULL DEFAULT '{IndexJobStatus.PENDING.value}'
                    CHECK (status IN ({_quoted_csv(INDEX_JOB_STATUS_VALUES)})),
    idempotency_key TEXT NOT NULL DEFAULT '',
    attempt_count   INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    started_at      TEXT,
    finished_at     TEXT
);
"""

_RECALL_EVENTS_DDL = """
CREATE TABLE IF NOT EXISTS recall_events (
    id              TEXT PRIMARY KEY,
    learning_id     TEXT NOT NULL REFERENCES learnings(id),
    query           TEXT NOT NULL,
    query_hash      TEXT NOT NULL DEFAULT '',
    source_context  TEXT NOT NULL DEFAULT '',
    rank            INTEGER,
    feedback        TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL
);
"""

_ARTIFACTS_DDL = f"""
CREATE TABLE IF NOT EXISTS artifacts (
    id              TEXT PRIMARY KEY,
    learning_id     TEXT NOT NULL REFERENCES learnings(id),
    artifact_type   TEXT NOT NULL
                    CHECK (artifact_type IN ({_quoted_csv(ARTIFACT_TYPE_VALUES)})),
    path            TEXT NOT NULL,
    content_hash    TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT '{ArtifactStatus.CREATED.value}'
                    CHECK (status IN ({_quoted_csv(ARTIFACT_STATUS_VALUES)})),
    metadata_json   TEXT NOT NULL DEFAULT '{{}}',
    created_at      TEXT NOT NULL
);
"""

_LEARNING_HISTORY_DDL = """
CREATE TABLE IF NOT EXISTS learning_history (
    id              TEXT PRIMARY KEY,
    learning_id     TEXT NOT NULL REFERENCES learnings(id),
    change_type     TEXT NOT NULL DEFAULT 'update',
    changed_fields  TEXT NOT NULL DEFAULT '[]',
    snapshot_json   TEXT NOT NULL DEFAULT '{}',
    reason          TEXT NOT NULL DEFAULT '',
    actor           TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL
);
"""

_CONCEPT_INDEX_DDL = """
CREATE TABLE IF NOT EXISTS concept_index (
    concept         TEXT NOT NULL,
    learning_id     TEXT NOT NULL REFERENCES learnings(id),
    created_at      TEXT NOT NULL,
    PRIMARY KEY (concept, learning_id)
);
"""

_INDEXES_SQL = """
CREATE INDEX IF NOT EXISTS idx_learnings_status ON learnings(status);
CREATE INDEX IF NOT EXISTS idx_learnings_source_tool ON learnings(source_tool);
CREATE INDEX IF NOT EXISTS idx_learnings_source_provider ON learnings(source_provider);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(type);
CREATE INDEX IF NOT EXISTS idx_events_learning_id ON events(learning_id);
CREATE INDEX IF NOT EXISTS idx_sources_provider ON sources(provider);
CREATE INDEX IF NOT EXISTS idx_sources_status ON sources(status);
CREATE INDEX IF NOT EXISTS idx_index_jobs_learning_id ON index_jobs(learning_id);
CREATE INDEX IF NOT EXISTS idx_index_jobs_status ON index_jobs(status);
CREATE INDEX IF NOT EXISTS idx_recall_events_learning_id ON recall_events(learning_id);
CREATE INDEX IF NOT EXISTS idx_artifacts_learning_id ON artifacts(learning_id);
CREATE INDEX IF NOT EXISTS idx_learning_history_learning_id
    ON learning_history(learning_id);
CREATE INDEX IF NOT EXISTS idx_concept_index_learning_id
    ON concept_index(learning_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_idempotency_key
    ON events(idempotency_key)
    WHERE idempotency_key != '';
"""

_SCHEMA_DDL = (
    _LEARNINGS_DDL
    + _PROPOSALS_DDL
    + _METRICS_DDL
    + _EVENTS_DDL
    + _SOURCES_DDL
    + _INDEX_JOBS_DDL
    + _RECALL_EVENTS_DDL
    + _ARTIFACTS_DDL
    + _LEARNING_HISTORY_DDL
    + _CONCEPT_INDEX_DDL
)

# ---------------------------------------------------------------------------
# Legacy v2 state paths (relative to Path.home())
# ---------------------------------------------------------------------------

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
    """Return True if any legacy v2 YAML state or a non-empty reflections dir exists."""
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


def _stable_text_hash(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()[:16]


def _db_path() -> Path:
    cfg = get_config()
    raw = cfg.get("storage", {}).get("db_path", "~/.reflect/reflect.db")
    return resolve_path(raw)


def db_path() -> Path:
    """Public accessor for the resolved DB path."""
    return _db_path()


def init_db(path: Optional[Path] = None) -> sqlite3.Connection:
    """Create tables if they don't exist and return a connection."""
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
    conn.executescript(_SCHEMA_DDL)
    _migrate_schema(conn)
    _ensure_indexes(conn)

    _CONN_CACHE[key] = conn
    return conn


def _warn_if_legacy_state_exists() -> None:
    """Print the legacy-state reminder to stderr, if any is found."""
    try:
        msg = get_legacy_state_summary()
        if msg is None:
            return
        import sys as _sys

        print(msg, file=_sys.stderr)
    except Exception:
        return


def _table_columns(conn: sqlite3.Connection, table: str) -> set[str]:
    return {row[1] for row in conn.execute(f"PRAGMA table_info({table})").fetchall()}


def _table_sql(conn: sqlite3.Connection, table: str) -> str:
    row = conn.execute(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name=?",
        (table,),
    ).fetchone()
    return row[0] if row and row[0] else ""


def _ensure_indexes(conn: sqlite3.Connection) -> None:
    with conn:
        conn.executescript(_INDEXES_SQL)


def _rebuild_table(
    conn: sqlite3.Connection,
    table: str,
    create_sql: str,
    columns: tuple[str, ...],
) -> None:
    temp_table = f"{table}_new"
    column_list = ", ".join(columns)
    # Build the replacement under a temp name, then drop-and-rename. The
    # naive rename-old-first approach makes ALTER TABLE rewrite every other
    # table's FOREIGN KEY clause to point at the doomed *_old name (sqlite
    # rewrites FK references on RENAME), leaving learning_history, artifacts,
    # index_jobs, and recall_events with dangling FKs once the temp table is
    # dropped ("no such table: main.learnings_old" on their next insert).
    # Renaming the *_new table instead rewrites nothing — no FK references
    # it. foreign_keys must be OFF so dropping the referenced table and the
    # transient name swap don't trip enforcement (no-op inside a transaction,
    # so it is toggled outside the ``with conn:`` block).
    temp_create_sql = create_sql.replace(
        f"CREATE TABLE IF NOT EXISTS {table}", f"CREATE TABLE {temp_table}", 1
    )
    conn.execute("PRAGMA foreign_keys=OFF")
    try:
        with conn:
            conn.execute(temp_create_sql)
            conn.execute(
                f"INSERT INTO {temp_table} ({column_list}) "
                f"SELECT {column_list} FROM {table}"
            )
            conn.execute(f"DROP TABLE {table}")
            conn.execute(f"ALTER TABLE {temp_table} RENAME TO {table}")
    finally:
        conn.execute("PRAGMA foreign_keys=ON")


def _migrate_schema(conn: sqlite3.Connection) -> None:
    """Idempotent migrations for existing DBs."""
    learning_columns = _table_columns(conn, "learnings")
    learning_alters = [
        ("scope", "ALTER TABLE learnings ADD COLUMN scope TEXT NOT NULL DEFAULT 'project'"),
        (
            "source_provider",
            "ALTER TABLE learnings ADD COLUMN source_provider TEXT NOT NULL DEFAULT ''",
        ),
        ("source_kind", "ALTER TABLE learnings ADD COLUMN source_kind TEXT NOT NULL DEFAULT ''"),
        ("source_quote", "ALTER TABLE learnings ADD COLUMN source_quote TEXT NOT NULL DEFAULT ''"),
        (
            "source_quote_hash",
            "ALTER TABLE learnings ADD COLUMN source_quote_hash TEXT NOT NULL DEFAULT ''",
        ),
        (
            "source_memory_ids",
            "ALTER TABLE learnings ADD COLUMN source_memory_ids TEXT NOT NULL DEFAULT '[]'",
        ),
        (
            "proof_count",
            "ALTER TABLE learnings ADD COLUMN proof_count INTEGER NOT NULL DEFAULT 1",
        ),
        ("session_id", "ALTER TABLE learnings ADD COLUMN session_id TEXT NOT NULL DEFAULT ''"),
        ("thread_id", "ALTER TABLE learnings ADD COLUMN thread_id TEXT NOT NULL DEFAULT ''"),
        (
            "privacy_level",
            "ALTER TABLE learnings ADD COLUMN privacy_level TEXT NOT NULL DEFAULT 'internal'",
        ),
        ("artifact_path", "ALTER TABLE learnings ADD COLUMN artifact_path TEXT NOT NULL DEFAULT ''"),
        ("sidecar_path", "ALTER TABLE learnings ADD COLUMN sidecar_path TEXT NOT NULL DEFAULT ''"),
        ("commit_hash", "ALTER TABLE learnings ADD COLUMN commit_hash TEXT"),
        (
            "supersedes_learning_id",
            "ALTER TABLE learnings ADD COLUMN supersedes_learning_id TEXT",
        ),
        (
            "superseded_by_learning_id",
            "ALTER TABLE learnings ADD COLUMN superseded_by_learning_id TEXT",
        ),
        ("reverted_at", "ALTER TABLE learnings ADD COLUMN reverted_at TEXT"),
        ("revert_reason", "ALTER TABLE learnings ADD COLUMN revert_reason TEXT"),
        ("last_recalled_at", "ALTER TABLE learnings ADD COLUMN last_recalled_at TEXT"),
        (
            "recall_count",
            "ALTER TABLE learnings ADD COLUMN recall_count INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "helpful_count",
            "ALTER TABLE learnings ADD COLUMN helpful_count INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "ignored_count",
            "ALTER TABLE learnings ADD COLUMN ignored_count INTEGER NOT NULL DEFAULT 0",
        ),
        ("stale_count", "ALTER TABLE learnings ADD COLUMN stale_count INTEGER NOT NULL DEFAULT 0"),
        ("is_latest", "ALTER TABLE learnings ADD COLUMN is_latest INTEGER NOT NULL DEFAULT 1"),
    ]
    with conn:
        for column, sql in learning_alters:
            if column not in learning_columns:
                conn.execute(sql)

    learning_sql = _table_sql(conn, "learnings")
    if learning_sql and not all(f"'{status}'" in learning_sql for status in LEARNING_STATUS_VALUES):
        _rebuild_table(conn, "learnings", _LEARNINGS_DDL, _LEARNING_COLUMNS)

    proposal_columns = _table_columns(conn, "proposals")
    proposal_alters = [
        (
            "proposal_type",
            "ALTER TABLE proposals ADD COLUMN proposal_type TEXT NOT NULL DEFAULT 'learning'",
        ),
        ("target_kind", "ALTER TABLE proposals ADD COLUMN target_kind TEXT NOT NULL DEFAULT ''"),
        ("target_path", "ALTER TABLE proposals ADD COLUMN target_path TEXT NOT NULL DEFAULT ''"),
        (
            "decision_actor",
            "ALTER TABLE proposals ADD COLUMN decision_actor TEXT NOT NULL DEFAULT ''",
        ),
        (
            "rationale_json",
            "ALTER TABLE proposals ADD COLUMN rationale_json TEXT NOT NULL DEFAULT '{}'",
        ),
        ("decided_at", "ALTER TABLE proposals ADD COLUMN decided_at TEXT"),
        ("materialized_at", "ALTER TABLE proposals ADD COLUMN materialized_at TEXT"),
        (
            "materialization_error",
            "ALTER TABLE proposals ADD COLUMN materialization_error TEXT",
        ),
    ]
    with conn:
        for column, sql in proposal_alters:
            if column not in proposal_columns:
                conn.execute(sql)

    proposal_sql = _table_sql(conn, "proposals")
    if proposal_sql and not all(f"'{status}'" in proposal_sql for status in PROPOSAL_STATUS_VALUES):
        _rebuild_table(conn, "proposals", _PROPOSALS_DDL, _PROPOSAL_COLUMNS)

    event_columns = _table_columns(conn, "events")
    event_alters = [
        ("actor", "ALTER TABLE events ADD COLUMN actor TEXT NOT NULL DEFAULT ''"),
        ("parent_event_id", "ALTER TABLE events ADD COLUMN parent_event_id TEXT"),
        (
            "idempotency_key",
            "ALTER TABLE events ADD COLUMN idempotency_key TEXT NOT NULL DEFAULT ''",
        ),
    ]
    with conn:
        for column, sql in event_alters:
            if column not in event_columns:
                conn.execute(sql)

    source_columns = _table_columns(conn, "sources")
    source_alters = [
        ("source_kind", "ALTER TABLE sources ADD COLUMN source_kind TEXT NOT NULL DEFAULT ''"),
        ("provider_id", "ALTER TABLE sources ADD COLUMN provider_id TEXT NOT NULL DEFAULT ''"),
        (
            "canonical_project_id",
            "ALTER TABLE sources ADD COLUMN canonical_project_id TEXT NOT NULL DEFAULT ''",
        ),
        ("first_seen", "ALTER TABLE sources ADD COLUMN first_seen TEXT NOT NULL DEFAULT ''"),
        ("archived_at", "ALTER TABLE sources ADD COLUMN archived_at TEXT"),
        (
            "ingest_state",
            "ALTER TABLE sources ADD COLUMN ingest_state TEXT NOT NULL DEFAULT 'discovered'",
        ),
    ]
    with conn:
        for column, sql in source_alters:
            if column not in source_columns:
                conn.execute(sql)
        conn.execute(
            "UPDATE sources SET first_seen = last_seen WHERE first_seen = '' OR first_seen IS NULL"
        )

    with conn:
        conn.execute(_INDEX_JOBS_DDL)
        conn.execute(_RECALL_EVENTS_DDL)
        conn.execute(_ARTIFACTS_DDL)
        conn.execute(_LEARNING_HISTORY_DDL)
        conn.execute(_CONCEPT_INDEX_DDL)

    _backfill_concept_index(conn)


def get_conn(path: Optional[Path] = None) -> sqlite3.Connection:
    """Return (and lazily create) the database connection."""
    return init_db(path)


def close_all() -> None:
    """Close every cached connection and clear the cache.

    Primarily for tests that need to swap DB files between cases. In
    production code there's no need to call this — connection lifetime
    matches process lifetime by design.
    """
    for conn in _CONN_CACHE.values():
        try:
            conn.close()
        except Exception:
            pass
    _CONN_CACHE.clear()


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


def compute_content_hash(payload: dict[str, Any]) -> str:
    """Stable 16-hex-char SHA-256 prefix over canonical JSON of *payload*."""
    blob = json.dumps(payload, sort_keys=True, default=str).encode("utf-8")
    return hashlib.sha256(blob).hexdigest()[:16]


def get_known_content_hashes(*, conn: Optional[sqlite3.Connection] = None) -> set[str]:
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


def _dedupe_source_ids(values: Any) -> list[str]:
    """Normalize a source_memory_ids payload to a unique, order-preserving
    list of non-empty strings. Tolerates None / scalars / JSON strings."""
    if values is None:
        return []
    if isinstance(values, str):
        try:
            parsed = json.loads(values)
            values = parsed if isinstance(parsed, list) else [values]
        except (json.JSONDecodeError, TypeError):
            values = [values]
    if not isinstance(values, (list, tuple, set)):
        values = [values]
    seen: set[str] = set()
    out: list[str] = []
    for value in values:
        text = str(value).strip()
        if text and text not in seen:
            seen.add(text)
            out.append(text)
    return out


def add_learning(
    title: str,
    category: str = "Unknown",
    confidence: str = "LOW",
    source_tool: str = "",
    source_path: str = "",
    content_hash: str = "",
    *,
    status: str = LearningStatus.PENDING.value,
    scope: str = "project",
    source_provider: str = "",
    source_kind: str = "",
    source_quote: str = "",
    source_quote_hash: str = "",
    source_memory_ids: Optional[list[str]] = None,
    proof_count: int = 1,
    session_id: str = "",
    thread_id: str = "",
    privacy_level: str = PrivacyLevel.INTERNAL.value,
    artifact_path: str = "",
    sidecar_path: str = "",
    commit_hash: Optional[str] = None,
    supersedes_learning_id: Optional[str] = None,
    superseded_by_learning_id: Optional[str] = None,
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Insert a new learning row. Returns the generated id.

    ``source_quote`` is captured raw regardless of ``privacy_level`` —
    redaction is applied at read-time by callers that surface the quote
    to the user (see ``RESTRICTED`` / ``SECRET_REDACTED`` in
    ``PrivacyLevel``). Storing raw lets future queries re-evaluate the
    redaction policy without losing the original evidence.

    S4 provenance: CREATE always starts ``proof_count`` at 1 (or higher
    when the caller already aggregated evidence) and stores
    ``source_memory_ids`` as a unique, order-preserving JSON list. The
    UPDATE half of the contract lives in :func:`add_learning_proof`.
    """
    conn = conn or get_conn()
    lid = _new_id()
    provider = source_provider or source_tool
    quote_hash = source_quote_hash or (_stable_text_hash(source_quote) if source_quote else "")
    memory_ids = _dedupe_source_ids(source_memory_ids)
    effective_proof_count = max(1, int(proof_count))
    now = _now_iso()
    with conn:
        conn.execute(
            """INSERT INTO learnings
               (id, title, category, confidence, status, scope, source_tool,
                source_provider, source_kind, source_path, source_quote,
                source_quote_hash, content_hash, source_memory_ids,
                proof_count, session_id, thread_id,
                privacy_level, artifact_path, sidecar_path, commit_hash,
                supersedes_learning_id, superseded_by_learning_id, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                lid,
                title,
                category,
                confidence,
                status,
                scope,
                source_tool,
                provider,
                source_kind,
                source_path,
                source_quote,
                quote_hash,
                content_hash,
                json.dumps(memory_ids),
                effective_proof_count,
                session_id,
                thread_id,
                privacy_level,
                artifact_path,
                sidecar_path,
                commit_hash,
                supersedes_learning_id,
                superseded_by_learning_id,
                now,
            ),
        )
        add_event(
            "learning_added",
            lid,
            {"title": title, "status": status, "scope": scope},
            conn=conn,
            autocommit=False,
        )
    # SG1 post-write hook: concept-index the new title, then demote any
    # recent in-scope learning this write contradicts (negation-stripped
    # Jaccard > 0.9 with opposite negation polarity). Best-effort — a
    # failure here must never break the write itself (silent-fail shaped).
    try:
        _index_learning_concepts(conn, lid, title, created_at=now)
        detect_and_resolve_contradictions(lid, title, scope=scope, conn=conn)
    except Exception:
        pass
    return lid


def update_learning_status(
    learning_id: str,
    status: str,
    *,
    revert_reason: Optional[str] = None,
    commit_hash: Optional[str] = None,
    conn: Optional[sqlite3.Connection] = None,
) -> None:
    """Transition a learning to *status* and write a matching audit event."""
    conn = conn or get_conn()
    now = _now_iso()
    extras: dict[str, Any] = {}
    if status == LearningStatus.APPROVED.value:
        extras["approved_at"] = now
    elif status == LearningStatus.INDEXED.value:
        extras["indexed_at"] = now
    elif status == LearningStatus.REVERTED.value:
        extras["reverted_at"] = now
        if revert_reason is not None:
            extras["revert_reason"] = revert_reason
    elif status == LearningStatus.RECALLED.value:
        extras["last_recalled_at"] = now
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
        # S6: archive the old form before mutating it.
        snapshot_learning_history(
            learning_id,
            change_type="status_change",
            changed_fields=["status", *extras.keys()],
            reason=revert_reason or f"status -> {status}",
            conn=conn,
            autocommit=False,
        )
        conn.execute(
            f"UPDATE learnings SET {', '.join(set_parts)} WHERE id = ?",
            params,
        )
        add_event(
            "status_change",
            learning_id,
            details,
            conn=conn,
            autocommit=False,
        )


def get_pending_learnings(
    *, conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Return all learnings with status='pending'."""
    conn = conn or get_conn()
    rows = conn.execute(
        "SELECT * FROM learnings WHERE status = ? ORDER BY created_at",
        (LearningStatus.PENDING.value,),
    ).fetchall()
    return [dict(r) for r in rows]


def get_learning(
    learning_id: str,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> Optional[dict[str, Any]]:
    """Fetch a single learning by id."""
    conn = conn or get_conn()
    row = conn.execute(
        "SELECT * FROM learnings WHERE id = ?",
        (learning_id,),
    ).fetchone()
    return dict(row) if row else None


def get_learnings_by_content_hash(
    content_hash: str,
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Return every learning row carrying *content_hash* (oldest first)."""
    if not content_hash:
        return []
    conn = conn or get_conn()
    rows = conn.execute(
        "SELECT * FROM learnings WHERE content_hash = ? ORDER BY created_at",
        (content_hash,),
    ).fetchall()
    return [dict(r) for r in rows]


def add_learning_proof(
    learning_id: str,
    source_memory_id: str = "",
    *,
    conn: Optional[sqlite3.Connection] = None,
) -> bool:
    """S4 UPDATE path: append *source_memory_id* and bump ``proof_count``.

    Re-observing evidence for an existing learning strengthens it instead
    of duplicating it. Semantics:

    - new (non-empty) source id → appended to ``source_memory_ids``
      (uniqueness preserved) and ``proof_count`` incremented;
    - already-recorded source id → idempotent no-op (returns False), so
      re-ingesting the same transcript can never inflate evidence;
    - empty source id → anonymous evidence: ``proof_count`` is bumped but
      the list is untouched (callers without a stable source identifier).

    Returns True when the row was updated.
    """
    conn = conn or get_conn()
    row = get_learning(learning_id, conn=conn)
    if row is None:
        return False

    sources = _dedupe_source_ids(row.get("source_memory_ids"))
    sid = str(source_memory_id).strip()
    if sid and sid in sources:
        return False
    if sid:
        sources.append(sid)

    new_proof_count = max(1, int(row.get("proof_count") or 1)) + 1
    with conn:
        # S6: archive the old form before mutating it.
        snapshot_learning_history(
            learning_id,
            change_type="proof_added",
            changed_fields=["source_memory_ids", "proof_count"],
            reason=f"proof_count {row.get('proof_count') or 1} -> {new_proof_count}",
            conn=conn,
            autocommit=False,
        )
        conn.execute(
            "UPDATE learnings SET source_memory_ids = ?, proof_count = ? WHERE id = ?",
            (json.dumps(sources), new_proof_count, learning_id),
        )
        add_event(
            "proof_added",
            learning_id,
            {
                "source_memory_id": sid,
                "proof_count": new_proof_count,
                "source_count": len(sources),
            },
            conn=conn,
            autocommit=False,
        )
    return True


# ---------------------------------------------------------------------------
# Learning history (S6: non-destructive belief revision)
# ---------------------------------------------------------------------------


def snapshot_learning_history(
    learning_id: str,
    *,
    change_type: str = "update",
    changed_fields: Optional[list[str]] = None,
    reason: str = "",
    actor: str = "",
    conn: Optional[sqlite3.Connection] = None,
    autocommit: bool = True,
) -> Optional[str]:
    """S6: snapshot the CURRENT form of a learning before it is mutated.

    Called at the top of every UPDATE path so belief revision is
    non-destructive — 'why did we change this rule?' stays answerable
    from the ``learning_history`` audit trail. The whole row is stored
    as canonical JSON in ``snapshot_json``; ``changed_fields`` records
    which columns the caller is about to touch.

    Returns the history row id, or None when *learning_id* doesn't
    exist (nothing to snapshot — mirrors the no-op semantics of the
    UPDATE paths themselves).

    Like :func:`add_event`, pass ``autocommit=False`` when calling from
    inside an open ``with conn:`` block so the snapshot commits (or
    rolls back) atomically with the mutation it precedes.
    """
    conn = conn or get_conn()
    row = get_learning(learning_id, conn=conn)
    if row is None:
        return None

    hid = _new_id()
    sql = """
        INSERT INTO learning_history
            (id, learning_id, change_type, changed_fields, snapshot_json,
             reason, actor, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    """
    params = (
        hid,
        learning_id,
        change_type,
        json.dumps(sorted(changed_fields or [])),
        json.dumps(row, sort_keys=True, default=str),
        reason,
        actor,
        _now_iso(),
    )
    if autocommit:
        with conn:
            conn.execute(sql, params)
    else:
        conn.execute(sql, params)
    return hid


def get_learning_history(
    learning_id: str,
    *,
    limit: int = 100,
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Return history snapshots for a learning, newest first."""
    conn = conn or get_conn()
    rows = conn.execute(
        """SELECT * FROM learning_history
           WHERE learning_id = ?
           ORDER BY created_at DESC
           LIMIT ?""",
        (learning_id, limit),
    ).fetchall()
    return [dict(r) for r in rows]


def get_update_counts(
    *,
    limit: int = 100,
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """Per-learning update counts from the history table (most-updated first).

    Powers the reflect-status 'update count per learning' view. Titles
    come from a LEFT JOIN so snapshots survive even if the learning row
    is later removed.
    """
    conn = conn or get_conn()
    rows = conn.execute(
        """SELECT h.learning_id AS learning_id,
                  COUNT(*) AS update_count,
                  MAX(h.created_at) AS last_updated_at,
                  COALESCE(l.title, '') AS title
           FROM learning_history h
           LEFT JOIN learnings l ON l.id = h.learning_id
           GROUP BY h.learning_id
           ORDER BY update_count DESC, last_updated_at DESC
           LIMIT ?""",
        (limit,),
    ).fetchall()
    return [dict(r) for r in rows]


# ---------------------------------------------------------------------------
# Concept index + contradictions (SG1: cross-turn contradiction detection)
# ---------------------------------------------------------------------------

CONTRADICTION_EVENT_TYPE = "contradiction_detected"

# Statuses that never resurface as contradiction candidates — retired beliefs
# (same set the S5 revision recall excludes).
_CONTRADICTION_RETIRED_STATUSES = (
    LearningStatus.REVERTED.value,
    LearningStatus.SUPERSEDED.value,
    LearningStatus.REJECTED.value,
)


def _load_contradiction_detector():
    """Lazy import so reflect_db keeps working if the module is absent."""
    import contradiction_detector

    return contradiction_detector


def _index_learning_concepts(
    conn: sqlite3.Connection,
    learning_id: str,
    title: str,
    *,
    created_at: str,
    autocommit: bool = True,
) -> int:
    """Write *title*'s concept tags into ``concept_index``.

    The concept index is the candidate-pruning structure for contradiction
    detection: only learnings sharing >= 1 concept with a new write are
    ever compared (agentmemory's concept-index shape). Returns the number
    of concepts indexed.
    """
    detector = _load_contradiction_detector()
    rows = [
        (concept, learning_id, created_at)
        for concept in sorted(detector.extract_concepts(title))
    ]
    if not rows:
        return 0
    sql = (
        "INSERT OR IGNORE INTO concept_index (concept, learning_id, created_at) "
        "VALUES (?, ?, ?)"
    )
    if autocommit:
        with conn:
            conn.executemany(sql, rows)
    else:
        conn.executemany(sql, rows)
    return len(rows)


def _backfill_concept_index(conn: sqlite3.Connection) -> None:
    """One-shot migration: concept-index pre-existing learnings.

    Without this, learnings written before SG1 shipped would never be
    contradiction candidates. Only runs when the index is empty and
    learnings exist; bounded to the newest scan-cap rows (older rows
    fall outside the recency window anyway). Best-effort — a failure
    here must never block schema migration.
    """
    try:
        detector = _load_contradiction_detector()
        existing = conn.execute("SELECT COUNT(*) FROM concept_index").fetchone()[0]
        if existing:
            return
        rows = conn.execute(
            "SELECT id, title, created_at FROM learnings "
            "ORDER BY created_at DESC LIMIT ?",
            (detector.CANDIDATE_SCAN_CAP,),
        ).fetchall()
        with conn:
            for row in rows:
                _index_learning_concepts(
                    conn, row["id"], row["title"],
                    created_at=row["created_at"], autocommit=False,
                )
    except Exception:
        return


def _conn_state_dir(conn: sqlite3.Connection) -> Optional[Path]:
    """Directory holding this connection's DB file (None for :memory:)."""
    try:
        for row in conn.execute("PRAGMA database_list").fetchall():
            if row[1] == "main" and row[2]:
                return Path(row[2]).resolve().parent
    except Exception:
        pass
    return None


def _append_contradiction_jsonl(
    conn: sqlite3.Connection, payload: dict[str, Any],
) -> None:
    """Append a contradiction record to ``events.jsonl`` beside the DB.

    With the default DB path this is ``~/.reflect/events.jsonl`` — the
    append-only audit file the SG1 acceptance contract pins. Best-effort:
    the sqlite event row is the source of truth; this mirror is for
    grep-ability and never raises.
    """
    try:
        state = _conn_state_dir(conn)
        if state is None:
            return
        state.mkdir(parents=True, exist_ok=True)
        with open(state / "events.jsonl", "a", encoding="utf-8") as fh:
            fh.write(json.dumps(payload, sort_keys=True) + "\n")
    except Exception:
        return


def detect_and_resolve_contradictions(
    learning_id: str,
    title: str,
    *,
    scope: str = "project",
    conn: Optional[sqlite3.Connection] = None,
) -> list[dict[str, Any]]:
    """SG1 post-write hook body: demote learnings the new write contradicts.

    Candidate pruning: recent (newest scan-cap) in-scope learnings sharing
    >= 1 concept tag with *title*, still ``is_latest`` and not retired.
    A candidate contradicts the new write when the negation-stripped
    Jaccard similarity is > 0.9 AND a negation marker appears in exactly
    one of the two titles (see ``contradiction_detector``).

    For each hit the OLDER side (the candidate — the caller is the row
    that was just written) is demoted non-destructively: history snapshot
    first (S6), then ``is_latest = 0`` + ``superseded_by_learning_id``,
    plus a ``contradiction_detected`` audit event in sqlite mirrored to
    ``events.jsonl``. Returns the resolved pairs.
    """
    conn = conn or get_conn()
    detector = _load_contradiction_detector()
    concepts = sorted(detector.extract_concepts(title))
    if not concepts:
        return []

    concept_marks = ", ".join("?" for _ in concepts)
    retired_marks = ", ".join("?" for _ in _CONTRADICTION_RETIRED_STATUSES)
    candidates = conn.execute(
        f"""SELECT DISTINCT l.id, l.title, l.created_at
            FROM concept_index ci
            JOIN learnings l ON l.id = ci.learning_id
            WHERE ci.concept IN ({concept_marks})
              AND l.id != ?
              AND l.scope = ?
              AND l.is_latest = 1
              AND l.status NOT IN ({retired_marks})
            ORDER BY l.created_at DESC
            LIMIT ?""",
        (
            *concepts,
            learning_id,
            scope,
            *_CONTRADICTION_RETIRED_STATUSES,
            detector.CANDIDATE_SCAN_CAP,
        ),
    ).fetchall()

    resolved: list[dict[str, Any]] = []
    for candidate in candidates:
        similarity = detector.detect_contradiction(title, candidate["title"])
        if similarity is None:
            continue
        older_id = candidate["id"]
        details = {
            "older_id": older_id,
            "older_title": candidate["title"],
            "newer_id": learning_id,
            "newer_title": title,
            "similarity": round(similarity, 4),
        }
        with conn:
            # S6: archive the old form before mutating it.
            snapshot_learning_history(
                older_id,
                change_type="contradiction",
                changed_fields=["is_latest", "superseded_by_learning_id"],
                reason=(
                    f"contradicted by {learning_id} "
                    f"(jaccard {similarity:.2f}): {title}"
                ),
                conn=conn,
                autocommit=False,
            )
            conn.execute(
                """UPDATE learnings
                   SET is_latest = 0, superseded_by_learning_id = ?
                   WHERE id = ?""",
                (learning_id, older_id),
            )
            add_event(
                CONTRADICTION_EVENT_TYPE,
                older_id,
                details,
                conn=conn,
                autocommit=False,
            )
        _append_contradiction_jsonl(
            conn, {"type": CONTRADICTION_EVENT_TYPE, "created_at": _now_iso(), **details},
        )
        resolved.append(details)
    return resolved


def get_contradiction_count(*, conn: Optional[sqlite3.Connection] = None) -> int:
    """Total contradiction audit events on file (powers /reflect:status)."""
    conn = conn or get_conn()
    row = conn.execute(
        "SELECT COUNT(*) FROM events WHERE type = ?",
        (CONTRADICTION_EVENT_TYPE,),
    ).fetchone()
    return int(row[0])


# ---------------------------------------------------------------------------
# Proposals
# ---------------------------------------------------------------------------


def add_proposal(
    learning_id: str,
    agent_file: str = "",
    diff: str = "",
    *,
    proposal_type: str = ProposalType.LEARNING.value,
    target_kind: str = "",
    target_path: str = "",
    status: str = ProposalStatus.PENDING.value,
    decision_actor: str = "",
    rationale_json: Optional[dict[str, Any] | str] = None,
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Insert a new proposal. Returns the generated id."""
    conn = conn or get_conn()
    pid = _new_id()
    serialized_rationale = (
        rationale_json
        if isinstance(rationale_json, str)
        else json.dumps(rationale_json or {})
    )
    with conn:
        conn.execute(
            """INSERT INTO proposals
               (id, learning_id, proposal_type, target_kind, target_path,
                agent_file, diff, status, decision_actor, rationale_json,
                created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                pid,
                learning_id,
                proposal_type,
                target_kind,
                target_path,
                agent_file,
                diff,
                status,
                decision_actor,
                serialized_rationale,
                _now_iso(),
            ),
        )
    return pid


# ---------------------------------------------------------------------------
# Metrics
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
        "SELECT value FROM metrics WHERE key = ?",
        (key,),
    ).fetchone()
    if row is None:
        return default
    raw = row["value"]
    try:
        return json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return raw


def get_metrics(*, conn: Optional[sqlite3.Connection] = None) -> dict[str, Any]:
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
    """Atomically increment an integer metric. Returns new value.

    Resolved in a single SQL upsert so concurrent writers on the same
    connection cannot lose increments. Non-integer existing values
    coerce to 0 before the addition (matches the prior behaviour of the
    Python read-modify-write path).
    """
    conn = conn or get_conn()
    now = _now_iso()
    # The CASE picks an integer-coercible representation of the existing value:
    # numeric typeof passes through, text that round-trips cleanly through
    # CAST→printf is an integer-shaped string (e.g. '5', '-3'), and anything
    # else (timestamps like '2026-04-28T...', JSON blobs) falls back to 0 to
    # match the legacy Python read-modify-write semantics.
    sql = """
        INSERT INTO metrics (key, value, updated_at)
        VALUES (?, ?, ?)
        ON CONFLICT(key) DO UPDATE SET
            value = CAST(
                CAST(
                    CASE
                        WHEN typeof(value) IN ('integer', 'real') THEN value
                        WHEN CAST(value AS TEXT) =
                             printf('%d', CAST(value AS INTEGER)) THEN value
                        ELSE '0'
                    END AS INTEGER
                ) + excluded.value AS TEXT
            ),
            updated_at = excluded.updated_at
    """
    with conn:
        row = conn.execute(
            sql + " RETURNING value",
            (key, str(delta), now),
        ).fetchone()
    return int(row["value"])


# ---------------------------------------------------------------------------
# Events
# ---------------------------------------------------------------------------


def add_event(
    event_type: str,
    learning_id: Optional[str] = None,
    details: Optional[dict[str, Any]] = None,
    *,
    actor: str = "",
    parent_event_id: Optional[str] = None,
    idempotency_key: str = "",
    conn: Optional[sqlite3.Connection] = None,
    autocommit: bool = True,
) -> str:
    """Insert an audit event. Returns the event id."""
    conn = conn or get_conn()
    if idempotency_key:
        existing = conn.execute(
            "SELECT id FROM events WHERE idempotency_key = ?",
            (idempotency_key,),
        ).fetchone()
        if existing:
            return existing["id"]

    eid = _new_id()
    sql = """
        INSERT INTO events
            (id, type, learning_id, actor, parent_event_id, idempotency_key,
             details_json, created_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    """
    params = (
        eid,
        event_type,
        learning_id,
        actor,
        parent_event_id,
        idempotency_key,
        json.dumps(details or {}),
        _now_iso(),
    )
    try:
        if autocommit:
            with conn:
                conn.execute(sql, params)
        else:
            conn.execute(sql, params)
    except sqlite3.IntegrityError:
        if idempotency_key:
            existing = conn.execute(
                "SELECT id FROM events WHERE idempotency_key = ?",
                (idempotency_key,),
            ).fetchone()
            if existing:
                return existing["id"]
        raise
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
    source_kind: str = "",
    provider_id: str = "",
    canonical_project_id: str = "",
    ingest_state: str = "discovered",
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Insert or update a discovered source. Returns the source id."""
    conn = conn or get_conn()
    now = _now_iso()

    existing = conn.execute(
        "SELECT id, first_seen FROM sources WHERE provider = ? AND path = ?",
        (provider, path),
    ).fetchone()

    if existing:
        sid = existing["id"]
        with conn:
            conn.execute(
                """UPDATE sources
                   SET content_hash = ?, last_seen = ?, status = ?, project_name = ?,
                       source_kind = ?, provider_id = ?, canonical_project_id = ?,
                       ingest_state = ?, archived_at = NULL
                   WHERE id = ?""",
                (
                    content_hash,
                    now,
                    SourceStatus.ACTIVE.value,
                    project_name,
                    source_kind,
                    provider_id,
                    canonical_project_id,
                    ingest_state,
                    sid,
                ),
            )
        return sid

    sid = _new_id()
    with conn:
        conn.execute(
            """INSERT INTO sources
               (id, provider, path, project_name, source_kind, provider_id,
                canonical_project_id, content_hash, first_seen, last_seen,
                ingest_state, status)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                sid,
                provider,
                path,
                project_name,
                source_kind,
                provider_id,
                canonical_project_id,
                content_hash,
                now,
                now,
                ingest_state,
                SourceStatus.ACTIVE.value,
            ),
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
            """UPDATE sources SET status = ?
               WHERE status = ?
                 AND julianday('now') - julianday(last_seen) > ?""",
            (SourceStatus.STALE.value, SourceStatus.ACTIVE.value, days),
        )
    return cur.rowcount


# ---------------------------------------------------------------------------
# Index jobs, recall, and artifacts
# ---------------------------------------------------------------------------


def add_index_job(
    learning_id: str,
    backend: str,
    *,
    status: str = IndexJobStatus.PENDING.value,
    idempotency_key: str = "",
    attempt_count: int = 0,
    last_error: str = "",
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Insert an index job or return the existing row for an idempotency key."""
    conn = conn or get_conn()
    if idempotency_key:
        existing = conn.execute(
            "SELECT id FROM index_jobs WHERE idempotency_key = ?",
            (idempotency_key,),
        ).fetchone()
        if existing:
            return existing["id"]

    jid = _new_id()
    with conn:
        conn.execute(
            """INSERT INTO index_jobs
               (id, learning_id, backend, status, idempotency_key, attempt_count,
                last_error, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                jid,
                learning_id,
                backend,
                status,
                idempotency_key,
                attempt_count,
                last_error,
                _now_iso(),
            ),
        )
    return jid


def add_recall_event(
    learning_id: str,
    query: str,
    *,
    source_context: str = "",
    rank: Optional[int] = None,
    feedback: str = "",
    query_hash: str = "",
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Record a recall hit and update recall telemetry on the learning row."""
    conn = conn or get_conn()
    rid = _new_id()
    now = _now_iso()
    effective_query_hash = query_hash or _stable_text_hash(query)
    feedback = feedback.strip().lower()

    update_parts = [
        "last_recalled_at = ?",
        "recall_count = recall_count + 1",
    ]
    params: list[Any] = [now]
    if feedback == "helpful":
        update_parts.append("helpful_count = helpful_count + 1")
    elif feedback == "ignored":
        update_parts.append("ignored_count = ignored_count + 1")
    elif feedback == "stale":
        update_parts.append("stale_count = stale_count + 1")
    params.append(learning_id)

    with conn:
        conn.execute(
            """INSERT INTO recall_events
               (id, learning_id, query, query_hash, source_context, rank, feedback, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                rid,
                learning_id,
                query,
                effective_query_hash,
                source_context,
                rank,
                feedback,
                now,
            ),
        )
        conn.execute(
            f"UPDATE learnings SET {', '.join(update_parts)} WHERE id = ?",
            params,
        )
        add_event(
            "learning_recalled",
            learning_id,
            {
                "query_hash": effective_query_hash,
                "rank": rank,
                "feedback": feedback,
            },
            conn=conn,
            autocommit=False,
        )
    return rid


def add_artifact(
    learning_id: str,
    artifact_type: str,
    path: str,
    *,
    content_hash: str = "",
    status: str = ArtifactStatus.CREATED.value,
    metadata: Optional[dict[str, Any] | str] = None,
    conn: Optional[sqlite3.Connection] = None,
) -> str:
    """Record a generated artifact for a learning."""
    conn = conn or get_conn()
    aid = _new_id()
    metadata_json = metadata if isinstance(metadata, str) else json.dumps(metadata or {})
    with conn:
        conn.execute(
            """INSERT INTO artifacts
               (id, learning_id, artifact_type, path, content_hash, status,
                metadata_json, created_at)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                aid,
                learning_id,
                artifact_type,
                path,
                content_hash,
                status,
                metadata_json,
                _now_iso(),
            ),
        )
    return aid


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="Reflect SQLite manager")
    parser.add_argument(
        "command",
        choices=["init", "stats", "events", "history", "contradictions", "doctor"],
        help="Action to perform",
    )
    parser.add_argument("--limit", type=int, default=20)
    args = parser.parse_args()

    conn = init_db()

    if args.command == "init":
        print(f"Database initialized at {_db_path()}")

    elif args.command == "stats":
        for table in (
            "learnings",
            "proposals",
            "metrics",
            "events",
            "sources",
            "index_jobs",
            "recall_events",
            "artifacts",
            "learning_history",
            "concept_index",
        ):
            count = conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
            print(f"  {table}: {count} rows")

    elif args.command == "events":
        for ev in get_events(limit=args.limit, conn=conn):
            print(
                f"  [{ev['created_at']}] {ev['type']}  "
                f"learning={ev['learning_id'] or '-'}  "
                f"{ev['details_json']}"
            )

    elif args.command == "history":
        counts = get_update_counts(limit=args.limit, conn=conn)
        if not counts:
            print("  no learning updates recorded")
        for row in counts:
            print(
                f"  {row['learning_id']}  updates={row['update_count']}  "
                f"last={row['last_updated_at']}  {row['title']}"
            )

    elif args.command == "contradictions":
        total = get_contradiction_count(conn=conn)
        print(f"  contradictions detected: {total}")
        for ev in get_events_by_type(
            CONTRADICTION_EVENT_TYPE, limit=args.limit, conn=conn,
        ):
            details = json.loads(ev["details_json"] or "{}")
            print(
                f"  [{ev['created_at']}] "
                f"older={details.get('older_id', ev['learning_id'] or '-')} "
                f"newer={details.get('newer_id', '-')} "
                f"jaccard={details.get('similarity', '-')}  "
                f"{details.get('older_title', '')!r} -> "
                f"{details.get('newer_title', '')!r}"
            )

    elif args.command == "doctor":
        msg = get_legacy_state_summary()
        if msg is None:
            print("[reflect] no legacy v2 state found")
        else:
            print(msg)


if __name__ == "__main__":
    main()
