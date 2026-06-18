"""Shared Postgres connection + value-encoding helpers for the nano-graphrag
storage adapters.

The adapters run inside the CLIENT process (where reflect / nano-graphrag run);
they persist to a shared Postgres. The database itself does no embedding and no
LLM work — these helpers only open a connection, scope every statement by
``workspace_id``, and encode jsonb / pgvector parameters. There is deliberately
nothing here that imports or calls an LLM or embedding provider.

Connection + tenant come from nano-graphrag's ``global_config["addon_params"]``
(the same channel the bundled ``Neo4jStorage`` uses for ``neo4j_url``):

    GraphRAG(..., addon_params={"pg_dsn": "...", "workspace_id": "...",
                                "embedding_model": "all-mpnet-base-v2"})

``pg_dsn`` falls back to ``$DATABASE_URL``. ``workspace_id`` is mandatory — it is
the hard tenant boundary, scoped into every query exactly like Phase 1's
MemoryStore.
"""

from __future__ import annotations

import os
import threading
from typing import Any, Iterable, Optional, Sequence, Tuple

__all__ = ["resolve_config", "PgBackend", "vector_literal"]


def resolve_config(global_config: Optional[dict]) -> Tuple[str, str, str]:
    """Return ``(dsn, workspace_id, embedding_model)`` from nano-graphrag config.

    Raises ``ValueError`` if the dsn or workspace id is missing — a tenant-less
    query must never be built.
    """
    addon = (global_config or {}).get("addon_params") or {}
    dsn = addon.get("pg_dsn") or os.environ.get("DATABASE_URL")
    workspace_id = addon.get("workspace_id")
    model = addon.get("embedding_model", "unknown")
    if not dsn:
        raise ValueError("no Postgres DSN: set addon_params['pg_dsn'] or $DATABASE_URL")
    if not workspace_id:
        raise ValueError("addon_params['workspace_id'] is required (tenant scope)")
    return dsn, str(workspace_id), str(model)


def vector_literal(vec: Iterable[float]) -> str:
    """Encode a vector as a pgvector text literal: ``[0.1,0.2,...]``.

    Used with ``%s::vector`` so we depend only on psycopg, not pgvector-python.
    """
    return "[" + ",".join(repr(float(x)) for x in vec) + "]"


class PgBackend:
    """A lazily-opened, lock-guarded psycopg connection shared by every adapter
    for one ``(dsn, workspace_id)``.

    psycopg connections are not safe across concurrent threads; nano-graphrag
    drives the adapters from an asyncio loop, so a single connection guarded by
    a lock serializes DB access (correct, low-contention — the parallel work is
    client-side embedding/LLM, not the DB writes). ``autocommit`` so each upsert
    is durable for the next reader / the next machine.
    """

    _shared: dict[Tuple[str, str], "PgBackend"] = {}
    _shared_lock = threading.Lock()

    def __init__(self, dsn: str, workspace_id: str) -> None:
        self.dsn = dsn
        self.workspace_id = workspace_id
        self._conn: Any = None
        self._lock = threading.Lock()

    @classmethod
    def shared(cls, dsn: str, workspace_id: str) -> "PgBackend":
        key = (dsn, workspace_id)
        with cls._shared_lock:
            inst = cls._shared.get(key)
            if inst is None:
                inst = cls(dsn, workspace_id)
                cls._shared[key] = inst
            return inst

    def _conn_open(self):
        import psycopg
        from psycopg.rows import dict_row

        if self._conn is None or self._conn.closed:
            self._conn = psycopg.connect(self.dsn, autocommit=True, row_factory=dict_row)
        return self._conn

    def fetchall(self, sql: str, params: Sequence[Any] = ()) -> list[dict]:
        with self._lock:
            with self._conn_open().cursor() as cur:
                cur.execute(sql, params)
                return list(cur.fetchall())

    def fetchone(self, sql: str, params: Sequence[Any] = ()) -> Optional[dict]:
        with self._lock:
            with self._conn_open().cursor() as cur:
                cur.execute(sql, params)
                return cur.fetchone()

    def execute(self, sql: str, params: Sequence[Any] = ()) -> None:
        with self._lock:
            with self._conn_open().cursor() as cur:
                cur.execute(sql, params)

    def executemany(self, sql: str, rows: Sequence[Sequence[Any]]) -> None:
        if not rows:
            return
        with self._lock:
            with self._conn_open().cursor() as cur:
                cur.executemany(sql, rows)
