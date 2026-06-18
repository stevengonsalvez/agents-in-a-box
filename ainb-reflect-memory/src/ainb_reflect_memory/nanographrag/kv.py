"""PgKVStorage — nano-graphrag ``BaseKVStorage`` backed by Postgres ``ng_kv``.

Stores nano-graphrag's KV namespaces (full_docs, text_chunks,
community_reports, llm_response_cache) as one jsonb row per (tenant, namespace,
key). No LLM/embedding work — pure key/value persistence, tenant-scoped.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Union

from nano_graphrag.base import BaseKVStorage

from ._conn import PgBackend, resolve_config

__all__ = ["PgKVStorage"]

_TABLE = "reflect_memory.ng_kv"


# @dataclass is required: BaseKVStorage is a dataclass but defines no
# __post_init__, so its generated __init__ won't call ours unless this subclass
# is itself a dataclass (re-generating __init__ to invoke __post_init__).
@dataclass
class PgKVStorage(BaseKVStorage):
    def __post_init__(self) -> None:
        dsn, workspace_id, _model = resolve_config(self.global_config)
        self._pg = PgBackend.shared(dsn, workspace_id)
        self._ws = workspace_id

    async def all_keys(self) -> list[str]:
        rows = self._pg.fetchall(
            f"select key from {_TABLE} where workspace_id=%s and namespace=%s",
            (self._ws, self.namespace),
        )
        return [r["key"] for r in rows]

    async def get_by_id(self, id: str) -> Union[dict, None]:
        row = self._pg.fetchone(
            f"select value from {_TABLE} where workspace_id=%s and namespace=%s and key=%s",
            (self._ws, self.namespace, id),
        )
        return row["value"] if row else None

    async def get_by_ids(
        self, ids: list[str], fields: Union[set[str], None] = None
    ) -> list[Union[dict, None]]:
        if not ids:
            return []
        rows = self._pg.fetchall(
            f"select key, value from {_TABLE} "
            "where workspace_id=%s and namespace=%s and key = any(%s)",
            (self._ws, self.namespace, list(ids)),
        )
        by_key = {r["key"]: r["value"] for r in rows}
        out: list[Union[dict, None]] = []
        for k in ids:
            v = by_key.get(k)
            if v is None:
                out.append(None)
            elif fields is None or not isinstance(v, dict):
                # Field projection only applies to dict values; a scalar/list
                # JSON value is returned whole rather than raising on .items().
                out.append(v)
            else:
                out.append({fk: fv for fk, fv in v.items() if fk in fields})
        return out

    async def filter_keys(self, data: list[str]) -> set[str]:
        """Return the keys in ``data`` that do NOT already exist."""
        if not data:
            return set()
        rows = self._pg.fetchall(
            f"select key from {_TABLE} where workspace_id=%s and namespace=%s and key = any(%s)",
            (self._ws, self.namespace, list(data)),
        )
        existing = {r["key"] for r in rows}
        return {k for k in data if k not in existing}

    async def upsert(self, data: dict[str, dict]) -> None:
        from psycopg.types.json import Jsonb

        rows = [(self._ws, self.namespace, key, Jsonb(value)) for key, value in data.items()]
        self._pg.executemany(
            f"insert into {_TABLE} (workspace_id, namespace, key, value) "
            "values (%s,%s,%s,%s) "
            "on conflict (workspace_id, namespace, key) "
            "do update set value = excluded.value, updated_at = now()",
            rows,
        )

    async def drop(self) -> None:
        self._pg.execute(
            f"delete from {_TABLE} where workspace_id=%s and namespace=%s",
            (self._ws, self.namespace),
        )
