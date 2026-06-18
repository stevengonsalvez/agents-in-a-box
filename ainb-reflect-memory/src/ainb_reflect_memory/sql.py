"""Pure SQL builders — the only place query text is constructed.

Every function returns ``(sql, params)`` with ``%s`` placeholders, ready for
``cursor.execute``. Nothing here imports a database driver, so these builders
are fully unit-testable WITHOUT a database or live credentials.

Two invariants every builder upholds and the tests pin down:

1. **Tenant scoping.** ``workspace_id`` is always a bound parameter and is
   always the first one. Reads either filter ``WHERE workspace_id = %s`` inline
   or pass it as the first argument to a tenant-scoped SQL function. Writes
   always set ``workspace_id`` from the tenant.
2. **No interpolation.** All caller-supplied values are parameters, never
   f-string-spliced into the SQL. The only interpolated tokens are fixed schema
   and column identifiers defined in this module.

The ranked-search and graph-traversal logic lives in SQL functions defined by
the migration (``reflect_memory.search_memory``, ``search_entities``,
``entity_neighborhood``). The builders here call those functions so ranking and
recursion have a single source of truth that is also reachable directly from
psql / PostgREST (the "dumb searchable server" surface). Writes and id-hydration
reads are simple enough to inline.
"""

from __future__ import annotations

import json
from typing import Any, List, Sequence, Tuple

from .models import (
    InsertMemoryInput,
    SearchMemoryInput,
    Tenant,
    UpsertEdgeInput,
    UpsertEntityInput,
)
from .normalize import content_hash

__all__ = [
    "SCHEMA",
    "insert_memory",
    "search_memory",
    "upsert_entity",
    "upsert_edge",
    "search_entities",
    "entity_neighborhood",
    "memory_by_ids",
    "entities_by_ids",
]

SCHEMA = "reflect_memory"

_MEMORY_COLS = (
    "id, workspace_id, agent_id, source_session_id, user_id, source_type, "
    "source_uri, content, content_hash, metadata, confidence, "
    "created_at, updated_at"
)
_ENTITY_COLS = (
    "id, workspace_id, canonical_name, entity_type, aliases, metadata, created_at, updated_at"
)
_EDGE_COLS = (
    "id, workspace_id, source_entity_id, target_entity_id, relation_type, "
    "evidence_memory_id, weight, metadata, created_at, updated_at"
)

SqlAndParams = Tuple[str, List[Any]]


# --------------------------------------------------------------------------- #
# Writes
# --------------------------------------------------------------------------- #


def insert_memory(inp: InsertMemoryInput) -> SqlAndParams:
    """Insert a memory item, idempotent per (workspace, normalized content).

    Re-inserting identical normalized content in the same tenant updates the
    existing row (metadata/confidence/source refreshed, ``updated_at`` bumped)
    rather than creating a duplicate — the Phase 3 idempotency contract, set up
    now by the ``unique (workspace_id, content_hash)`` constraint.
    """
    t = inp.tenant
    sql = (
        f"INSERT INTO {SCHEMA}.memory_items "
        "(workspace_id, agent_id, source_session_id, user_id, source_type, "
        " source_uri, content, content_hash, metadata, confidence) "
        "VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s::jsonb, %s) "
        "ON CONFLICT (workspace_id, content_hash) DO UPDATE SET "
        "  source_type = EXCLUDED.source_type, "
        "  source_uri = EXCLUDED.source_uri, "
        "  metadata = EXCLUDED.metadata, "
        "  confidence = EXCLUDED.confidence, "
        "  agent_id = COALESCE(EXCLUDED.agent_id, "
        f"    {SCHEMA}.memory_items.agent_id), "
        "  updated_at = now() "
        f"RETURNING {_MEMORY_COLS}"
    )
    params: List[Any] = [
        t.workspace_id,
        t.agent_id,
        t.source_session_id,
        t.user_id,
        inp.source_type,
        inp.source_uri,
        inp.content,
        content_hash(inp.content),
        json.dumps(dict(inp.metadata)),
        float(inp.confidence),
    ]
    return sql, params


def upsert_entity(inp: UpsertEntityInput) -> SqlAndParams:
    """Upsert a canonical entity, keyed by (workspace, type, canonical_name)."""
    t = inp.tenant
    sql = (
        f"INSERT INTO {SCHEMA}.entities "
        "(workspace_id, canonical_name, entity_type, aliases, metadata) "
        "VALUES (%s, %s, %s, %s, %s::jsonb) "
        "ON CONFLICT (workspace_id, entity_type, canonical_name) DO UPDATE SET "
        "  aliases = EXCLUDED.aliases, "
        "  metadata = EXCLUDED.metadata, "
        "  updated_at = now() "
        f"RETURNING {_ENTITY_COLS}"
    )
    params: List[Any] = [
        t.workspace_id,
        inp.canonical_name,
        inp.entity_type,
        list(inp.aliases),
        json.dumps(dict(inp.metadata)),
    ]
    return sql, params


def upsert_edge(inp: UpsertEdgeInput) -> SqlAndParams:
    """Upsert an edge, keyed by (workspace, source, target, relation_type).

    Both endpoint entities must already exist in the same workspace — enforced
    by the foreign keys and the per-row ``workspace_id`` match in the migration.
    """
    t = inp.tenant
    sql = (
        f"INSERT INTO {SCHEMA}.edges "
        "(workspace_id, source_entity_id, target_entity_id, relation_type, "
        " evidence_memory_id, weight, metadata) "
        "VALUES (%s, %s, %s, %s, %s, %s, %s::jsonb) "
        "ON CONFLICT (workspace_id, source_entity_id, target_entity_id, "
        "             relation_type) DO UPDATE SET "
        "  evidence_memory_id = EXCLUDED.evidence_memory_id, "
        "  weight = EXCLUDED.weight, "
        "  metadata = EXCLUDED.metadata, "
        "  updated_at = now() "
        f"RETURNING {_EDGE_COLS}"
    )
    params: List[Any] = [
        t.workspace_id,
        inp.source_entity_id,
        inp.target_entity_id,
        inp.relation_type,
        inp.evidence_memory_id,
        float(inp.weight),
        json.dumps(dict(inp.metadata)),
    ]
    return sql, params


# --------------------------------------------------------------------------- #
# Reads — ranked search + graph traversal via tenant-scoped SQL functions
# --------------------------------------------------------------------------- #


def search_memory(inp: SearchMemoryInput) -> SqlAndParams:
    """Full-text ranked search within one tenant.

    Delegates ranking/snippeting to ``reflect_memory.search_memory(...)`` whose
    first argument is the workspace id, so the tenant filter is applied before
    ranking inside the function.
    """
    t = inp.tenant
    sql = f"SELECT * FROM {SCHEMA}.search_memory(%s, %s, %s, %s, %s)"
    params: List[Any] = [
        t.workspace_id,
        inp.query,
        int(inp.limit),
        inp.agent_id,
        inp.min_rank,
    ]
    return sql, params


def search_entities(tenant: Tenant, query: str, limit: int) -> SqlAndParams:
    """Fuzzy lookup of entities by canonical name or alias within one tenant."""
    sql = f"SELECT * FROM {SCHEMA}.search_entities(%s, %s, %s)"
    params: List[Any] = [tenant.workspace_id, query, int(limit)]
    return sql, params


def entity_neighborhood(tenant: Tenant, entity_id: str, depth: int) -> SqlAndParams:
    """Edges reachable from ``entity_id`` up to ``depth`` hops, same tenant only.

    The recursive traversal lives in ``reflect_memory.entity_neighborhood`` so
    there is exactly one implementation of the walk, and it filters every hop by
    the workspace id passed as its first argument.
    """
    sql = f"SELECT * FROM {SCHEMA}.entity_neighborhood(%s, %s, %s)"
    params: List[Any] = [tenant.workspace_id, entity_id, int(depth)]
    return sql, params


# --------------------------------------------------------------------------- #
# Reads — id hydration (tenant-scoped inline filters)
# --------------------------------------------------------------------------- #


def memory_by_ids(tenant: Tenant, ids: Sequence[str]) -> SqlAndParams:
    """Fetch memory items by id, scoped to the tenant (for citation hydration)."""
    sql = (
        f"SELECT {_MEMORY_COLS} FROM {SCHEMA}.memory_items WHERE workspace_id = %s AND id = ANY(%s)"
    )
    params: List[Any] = [tenant.workspace_id, list(ids)]
    return sql, params


def entities_by_ids(tenant: Tenant, ids: Sequence[str]) -> SqlAndParams:
    """Fetch entities by id, scoped to the tenant (for neighborhood hydration)."""
    sql = f"SELECT {_ENTITY_COLS} FROM {SCHEMA}.entities WHERE workspace_id = %s AND id = ANY(%s)"
    params: List[Any] = [tenant.workspace_id, list(ids)]
    return sql, params
