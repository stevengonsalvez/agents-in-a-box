"""MemoryStore — the typed helper layer over a Postgres connection.

This is the trusted server/worker path. It talks to Postgres through a DB-API
style connection (psycopg 3 in production) and returns typed records. It does
**no** LLM work: no embeddings, no extraction, no answer synthesis. It stores,
scopes, and retrieves — the brain stays in the client.

Safety model on this path is *explicit tenant scoping*: every query carries the
workspace id as a bound parameter (see ``sql.py``; the tests pin this down).
Row-Level Security is the independent guard for the *other* access path —
direct Supabase/PostgREST clients authenticating with a JWT — and is defined in
the migration. The two are defense in depth, not a single point of failure.

Connection contract
-------------------
``conn`` must yield **mapping rows** (e.g. psycopg's ``dict_row`` factory) so
``MemoryItem.from_row`` and friends can read columns by name::

    import psycopg
    from psycopg.rows import dict_row
    conn = psycopg.connect(DATABASE_URL, row_factory=dict_row)
    store = MemoryStore(conn)

The store calls ``conn.commit()`` after writes when the connection exposes it
(no-op under autocommit). It never closes the connection — the caller owns its
lifecycle.
"""

from __future__ import annotations

from typing import Any, List, Optional, Sequence

from . import sql
from .models import (
    Citation,
    Edge,
    Entity,
    EntityHit,
    EvidenceHit,
    EvidencePack,
    EvidencePackQuery,
    GraphNeighborhood,
    InsertMemoryInput,
    MemoryItem,
    SearchMemoryInput,
    SearchResult,
    UpsertEdgeInput,
    UpsertEntityInput,
)

__all__ = ["MemoryStore"]


class MemoryStore:
    """Typed CRUD + search over the reflect memory substrate."""

    def __init__(self, conn: Any) -> None:
        self._conn = conn

    # ----------------------------------------------------------------- #
    # low-level execution helpers
    # ----------------------------------------------------------------- #

    def _fetchone(self, sql_text: str, params: Sequence[Any]) -> Optional[dict]:
        with self._conn.cursor() as cur:
            cur.execute(sql_text, params)
            return cur.fetchone()

    def _fetchall(self, sql_text: str, params: Sequence[Any]) -> List[dict]:
        with self._conn.cursor() as cur:
            cur.execute(sql_text, params)
            return list(cur.fetchall())

    def _commit(self) -> None:
        commit = getattr(self._conn, "commit", None)
        if callable(commit):
            commit()

    # ----------------------------------------------------------------- #
    # writes
    # ----------------------------------------------------------------- #

    def insert_memory(self, inp: InsertMemoryInput) -> MemoryItem:
        """Insert (or idempotently refresh) a memory item; return the row."""
        sql_text, params = sql.insert_memory(inp)
        row = self._fetchone(sql_text, params)
        self._commit()
        assert row is not None  # RETURNING always yields a row
        return MemoryItem.from_row(row)

    def upsert_entity(self, inp: UpsertEntityInput) -> Entity:
        sql_text, params = sql.upsert_entity(inp)
        row = self._fetchone(sql_text, params)
        self._commit()
        assert row is not None
        return Entity.from_row(row)

    def upsert_edge(self, inp: UpsertEdgeInput) -> Edge:
        sql_text, params = sql.upsert_edge(inp)
        row = self._fetchone(sql_text, params)
        self._commit()
        assert row is not None
        return Edge.from_row(row)

    # ----------------------------------------------------------------- #
    # reads
    # ----------------------------------------------------------------- #

    def search_memory(self, inp: SearchMemoryInput) -> List[SearchResult]:
        """Ranked full-text search within the tenant."""
        sql_text, params = sql.search_memory(inp)
        rows = self._fetchall(sql_text, params)
        return [
            SearchResult(
                item=MemoryItem.from_row(r),
                rank=float(r["rank"]),
                snippet=r["snippet"],
            )
            for r in rows
        ]

    def lookup_entities(self, tenant, query: str, limit: int = 10) -> List[EntityHit]:
        """Fuzzy entity lookup by canonical name / alias within the tenant."""
        sql_text, params = sql.search_entities(tenant, query, limit)
        rows = self._fetchall(sql_text, params)
        return [
            EntityHit(
                entity_id=str(r["id"]),
                canonical_name=r["canonical_name"],
                entity_type=r["entity_type"],
                matched_alias=r.get("matched_alias"),
            )
            for r in rows
        ]

    def neighborhood(self, tenant, entity_id: str, depth: int = 1) -> GraphNeighborhood:
        """Entities + edges within ``depth`` hops of ``entity_id`` (same tenant)."""
        sql_text, params = sql.entity_neighborhood(tenant, entity_id, depth)
        edge_rows = self._fetchall(sql_text, params)
        edges = [Edge.from_row(r) for r in edge_rows]

        # Hydrate the entities touched by those edges (plus the seed), all
        # tenant-scoped, so the caller gets full entity records not just ids.
        entity_ids = {entity_id}
        for e in edges:
            entity_ids.add(e.source_entity_id)
            entity_ids.add(e.target_entity_id)
        entities: List[Entity] = []
        if entity_ids:
            ent_sql, ent_params = sql.entities_by_ids(tenant, sorted(entity_ids))
            entities = [Entity.from_row(r) for r in self._fetchall(ent_sql, ent_params)]

        return GraphNeighborhood(entities=entities, edges=edges)

    # ----------------------------------------------------------------- #
    # evidence pack — pure retrieval, no synthesis
    # ----------------------------------------------------------------- #

    def get_evidence_pack(self, q: EvidencePackQuery) -> EvidencePack:
        """Assemble an evidence pack for a query: lexical hits + entity matches
        + a graph neighborhood + citations. The server returns *evidence only*;
        the local agent synthesizes the final answer from it.
        """
        tenant = q.tenant

        # 1. lexical hits
        lexical_rows = self._fetchall(
            *sql.search_memory(
                SearchMemoryInput(tenant=tenant, query=q.query, limit=q.lexical_limit)
            )
        )
        lexical = [
            EvidenceHit(
                memory_id=str(r["id"]),
                content=r["content"],
                rank=float(r["rank"]),
                snippet=r["snippet"],
                source_type=r["source_type"],
                source_uri=r.get("source_uri"),
            )
            for r in lexical_rows
        ]

        # 2. entity matches
        entity_hits = self.lookup_entities(tenant, q.query, q.entity_limit)

        # 3. graph neighborhood around the top entity match (if any)
        graph = GraphNeighborhood(entities=[], edges=[])
        if entity_hits and q.neighborhood_depth > 0:
            graph = self.neighborhood(tenant, entity_hits[0].entity_id, q.neighborhood_depth)

        # 4. citations — every lexical hit is a citable source
        citations = [
            Citation(
                memory_id=h.memory_id,
                source_type=h.source_type,
                source_uri=h.source_uri,
            )
            for h in lexical
        ]

        return EvidencePack(
            query=q.query,
            tenant=tenant,
            lexical=lexical,
            entities=entity_hits,
            graph=graph,
            citations=citations,
        )
