#!/usr/bin/env python3
# ABOUTME: Demo seed — inserts sample memory/entity/edge rows for one workspace
# ABOUTME: and prints an evidence pack, to smoke-test a live database end to end.
"""Seed the reflect memory substrate with a small demo graph.

Usage:

    # 1. apply the migration first (see docs/setup.md), then:
    export DATABASE_URL='postgresql://USER:PASS@HOST:5432/DBNAME'
    uv run --extra pg python scripts/seed.py [WORKSPACE_UUID]

WORKSPACE_UUID defaults to the demo workspace below. The script is idempotent —
re-running it does not create duplicate memory/entity/edge rows (it relies on
the per-tenant content-hash and canonical-name/relation upsert keys).

This is a *client*: it does the (trivial, hand-written) "extraction" of entities
and edges here, then pushes them to the dumb server. No LLM is involved.
"""

from __future__ import annotations

import os
import sys

from ainb_reflect_memory import (
    EvidencePackQuery,
    InsertMemoryInput,
    MemoryStore,
    Tenant,
    UpsertEdgeInput,
    UpsertEntityInput,
)

DEMO_WORKSPACE = "00000000-0000-0000-0000-0000000000aa"


def main() -> int:
    dsn = os.environ.get("DATABASE_URL")
    if not dsn:
        print("DATABASE_URL is not set. See docs/setup.md.", file=sys.stderr)
        return 2

    try:
        import psycopg
        from psycopg.rows import dict_row
    except ImportError:
        print("psycopg not installed. Run: uv sync --extra pg", file=sys.stderr)
        return 2

    workspace_id = sys.argv[1] if len(sys.argv) > 1 else DEMO_WORKSPACE
    tenant = Tenant(workspace_id=workspace_id, agent_id=None)

    conn = psycopg.connect(dsn, row_factory=dict_row)
    try:
        store = MemoryStore(conn)

        # 1. memory items
        note = store.insert_memory(
            InsertMemoryInput(
                tenant=tenant,
                content="The auth middleware token expiry uses a strict less-than check.",
                source_type="codebase_note",
                source_uri="src/auth/middleware.rs",
                metadata={"file": "src/auth/middleware.rs", "line": 42},
                confidence=0.9,
            )
        )
        store.insert_memory(
            InsertMemoryInput(
                tenant=tenant,
                content="Stevie prefers many small single-concern commits over bulk commits.",
                source_type="user_preference",
                confidence=0.95,
            )
        )

        # 2. entities (client-extracted)
        auth = store.upsert_entity(
            UpsertEntityInput(
                tenant=tenant,
                canonical_name="Auth Middleware",
                entity_type="component",
                aliases=["auth middleware", "authn"],
            )
        )
        jwt = store.upsert_entity(
            UpsertEntityInput(
                tenant=tenant,
                canonical_name="JWT",
                entity_type="concept",
                aliases=["json web token", "token"],
            )
        )

        # 3. edge with evidence ref back to the memory item
        store.upsert_edge(
            UpsertEdgeInput(
                tenant=tenant,
                source_entity_id=auth.id,
                target_entity_id=jwt.id,
                relation_type="validates",
                evidence_memory_id=note.id,
                weight=0.9,
            )
        )

        # 4. read it back as an evidence pack (retrieval only). "auth middleware"
        # hits the memory note lexically AND the entity (alias), so the pack
        # shows a populated graph neighborhood.
        pack = store.get_evidence_pack(EvidencePackQuery(tenant=tenant, query="auth middleware"))
        print(f"workspace      : {workspace_id}")
        print(f"lexical hits   : {len(pack.lexical)}")
        for h in pack.lexical:
            print(f"  - [{h.rank:.3f}] {h.snippet}")
        print(f"entity matches : {[e.canonical_name for e in pack.entities]}")
        print(f"graph edges    : {len(pack.graph.edges)}")
        for e in pack.graph.edges:
            print(f"  - {e.source_entity_id} --{e.relation_type}--> {e.target_entity_id}")
        print(f"citations      : {[c.source_uri or c.memory_id for c in pack.citations]}")
        print("\nSeed complete (idempotent — safe to re-run).")
        return 0
    finally:
        conn.close()


if __name__ == "__main__":
    raise SystemExit(main())
