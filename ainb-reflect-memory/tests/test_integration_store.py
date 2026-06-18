# ABOUTME: Integration tests against a live Postgres (auto-skipped without one).
# ABOUTME: Proves the gates: insert/search FTS, idempotent ingestion, graph
# ABOUTME: neighborhood, tenant isolation, and RLS fail-closed direct access.

from __future__ import annotations

import pytest

from ainb_reflect_memory import (
    EvidencePackQuery,
    InsertMemoryInput,
    SearchMemoryInput,
    Tenant,
    UpsertEdgeInput,
    UpsertEntityInput,
)

pytestmark = pytest.mark.integration

WS_A = "11111111-1111-1111-1111-111111111111"
WS_B = "22222222-2222-2222-2222-222222222222"


# --------------------------------------------------------------------------- #
# insert + full-text search
# --------------------------------------------------------------------------- #


def test_insert_and_fts_search_returns_ranked_hit_with_snippet(store) -> None:
    a = Tenant(workspace_id=WS_A)
    item = store.insert_memory(
        InsertMemoryInput(
            tenant=a,
            content="The auth middleware token expiry uses a strict less-than check",
            source_type="codebase_note",
            source_uri="src/auth.rs",
        )
    )
    hits = store.search_memory(SearchMemoryInput(tenant=a, query="auth token expiry"))
    assert len(hits) == 1
    assert hits[0].item.id == item.id
    assert hits[0].rank > 0
    assert "<b>" in hits[0].snippet  # ts_headline highlighting present


def test_search_returns_nothing_for_unrelated_query(store) -> None:
    a = Tenant(workspace_id=WS_A)
    store.insert_memory(InsertMemoryInput(tenant=a, content="kubernetes pod scheduling notes"))
    assert store.search_memory(SearchMemoryInput(tenant=a, query="quantum chromodynamics")) == []


# --------------------------------------------------------------------------- #
# idempotent ingestion
# --------------------------------------------------------------------------- #


def test_insert_is_idempotent_per_normalized_content(store) -> None:
    a = Tenant(workspace_id=WS_A)
    first = store.insert_memory(
        InsertMemoryInput(tenant=a, content="Fixed the bug", confidence=0.5)
    )
    # same normalized content (case/whitespace folded) => same row, refreshed.
    second = store.insert_memory(
        InsertMemoryInput(tenant=a, content="  fixed   THE bug\n", confidence=0.9)
    )
    assert first.id == second.id
    assert second.confidence == pytest.approx(0.9)

    hits = store.search_memory(SearchMemoryInput(tenant=a, query="fixed bug"))
    assert len({h.item.id for h in hits}) == 1  # exactly one underlying row


def test_same_content_in_two_tenants_is_two_rows(store) -> None:
    # Dedupe is per-tenant: identical content in different workspaces coexists.
    a, b = Tenant(workspace_id=WS_A), Tenant(workspace_id=WS_B)
    ia = store.insert_memory(InsertMemoryInput(tenant=a, content="shared note text"))
    ib = store.insert_memory(InsertMemoryInput(tenant=b, content="shared note text"))
    assert ia.id != ib.id


# --------------------------------------------------------------------------- #
# entities + graph neighborhood
# --------------------------------------------------------------------------- #


def test_entity_alias_lookup(store) -> None:
    a = Tenant(workspace_id=WS_A)
    store.upsert_entity(
        UpsertEntityInput(
            tenant=a,
            canonical_name="JSON Web Token",
            entity_type="concept",
            aliases=["JWT", "jwt token"],
        )
    )
    hits = store.lookup_entities(a, "JWT")
    assert hits
    assert hits[0].canonical_name == "JSON Web Token"
    assert hits[0].matched_alias is not None


def test_upsert_entity_is_idempotent(store) -> None:
    a = Tenant(workspace_id=WS_A)
    first = store.upsert_entity(
        UpsertEntityInput(tenant=a, canonical_name="Auth", entity_type="component")
    )
    second = store.upsert_entity(
        UpsertEntityInput(
            tenant=a, canonical_name="Auth", entity_type="component", aliases=["authn"]
        )
    )
    assert first.id == second.id
    assert "authn" in second.aliases


def test_graph_neighborhood_is_same_tenant_only(store) -> None:
    a = Tenant(workspace_id=WS_A)
    auth = store.upsert_entity(
        UpsertEntityInput(tenant=a, canonical_name="Auth", entity_type="component")
    )
    jwt = store.upsert_entity(
        UpsertEntityInput(tenant=a, canonical_name="JWT", entity_type="concept")
    )
    store.upsert_edge(
        UpsertEdgeInput(
            tenant=a, source_entity_id=auth.id, target_entity_id=jwt.id, relation_type="uses"
        )
    )

    nb = store.neighborhood(a, auth.id, depth=1)
    assert len(nb.edges) == 1
    assert {e.canonical_name for e in nb.entities} == {"Auth", "JWT"}

    # A different tenant asking about A's entity id sees nothing.
    nb_b = store.neighborhood(Tenant(workspace_id=WS_B), auth.id, depth=1)
    assert nb_b.edges == []
    assert nb_b.entities == []


def test_cross_tenant_edge_is_physically_rejected(store) -> None:
    import psycopg

    a, b = Tenant(workspace_id=WS_A), Tenant(workspace_id=WS_B)
    ea = store.upsert_entity(UpsertEntityInput(tenant=a, canonical_name="X", entity_type="t"))
    eb = store.upsert_entity(UpsertEntityInput(tenant=b, canonical_name="Y", entity_type="t"))
    # Edge in workspace A pointing at B's entity violates the composite
    # (workspace_id, entity_id) FK — a cross-tenant edge cannot exist.
    with pytest.raises(psycopg.errors.ForeignKeyViolation):
        store.upsert_edge(
            UpsertEdgeInput(
                tenant=a, source_entity_id=ea.id, target_entity_id=eb.id, relation_type="rel"
            )
        )


# --------------------------------------------------------------------------- #
# tenant isolation on the trusted (MemoryStore) path
# --------------------------------------------------------------------------- #


def test_search_is_tenant_scoped(store) -> None:
    a, b = Tenant(workspace_id=WS_A), Tenant(workspace_id=WS_B)
    store.insert_memory(InsertMemoryInput(tenant=a, content="alpha unique token zebra"))
    store.insert_memory(InsertMemoryInput(tenant=b, content="alpha unique token zebra"))

    hits_a = store.search_memory(SearchMemoryInput(tenant=a, query="zebra"))
    assert len(hits_a) == 1
    assert hits_a[0].item.workspace_id == WS_A


# --------------------------------------------------------------------------- #
# evidence pack — retrieval only, no synthesis
# --------------------------------------------------------------------------- #


def test_evidence_pack_assembles_lexical_entities_graph_citations(store) -> None:
    a = Tenant(workspace_id=WS_A)
    item = store.insert_memory(
        InsertMemoryInput(
            tenant=a,
            content="The auth middleware validates the token on every request",
            source_type="codebase_note",
            source_uri="src/auth.rs",
        )
    )
    auth = store.upsert_entity(
        UpsertEntityInput(
            tenant=a, canonical_name="auth", entity_type="component", aliases=["auth middleware"]
        )
    )
    token = store.upsert_entity(
        UpsertEntityInput(tenant=a, canonical_name="token", entity_type="concept")
    )
    store.upsert_edge(
        UpsertEdgeInput(
            tenant=a,
            source_entity_id=auth.id,
            target_entity_id=token.id,
            relation_type="validates",
            evidence_memory_id=item.id,
        )
    )

    pack = store.get_evidence_pack(EvidencePackQuery(tenant=a, query="auth"))
    assert pack.query == "auth"
    assert pack.tenant.workspace_id == WS_A
    assert any(h.memory_id == item.id for h in pack.lexical)
    assert any(e.canonical_name == "auth" for e in pack.entities)
    assert len(pack.graph.edges) >= 1
    assert any(c.memory_id == item.id for c in pack.citations)


# --------------------------------------------------------------------------- #
# Row-Level Security — the direct (PostgREST/JWT) access path, fail-closed
# --------------------------------------------------------------------------- #


def test_rls_isolates_direct_access_by_workspace_guc(conn, store) -> None:
    """An unprivileged role sees only its current-workspace rows, nothing
    without a workspace set. Exercises the RLS policies + tenant resolver GUC
    fallback that guard the direct Supabase/PostgREST client path."""
    a, b = Tenant(workspace_id=WS_A), Tenant(workspace_id=WS_B)
    store.insert_memory(InsertMemoryInput(tenant=a, content="alpha secret in A"))
    store.insert_memory(InsertMemoryInput(tenant=b, content="beta secret in B"))

    with conn.cursor() as cur:
        # Recreate a clean non-superuser role (RLS does not apply to owners).
        cur.execute(
            "do $$ begin "
            "  if exists (select 1 from pg_roles where rolname='reflect_rls_test') then "
            "    execute 'drop owned by reflect_rls_test'; "
            "    execute 'drop role reflect_rls_test'; "
            "  end if; "
            "end $$;"
        )
        cur.execute("create role reflect_rls_test nologin;")
        cur.execute("grant usage on schema reflect_memory to reflect_rls_test;")
        cur.execute("grant select on all tables in schema reflect_memory to reflect_rls_test;")
        cur.execute("grant execute on all functions in schema reflect_memory to reflect_rls_test;")
        conn.commit()

        cur.execute("set role reflect_rls_test;")

        # No workspace resolvable -> resolver returns NULL -> deny all.
        cur.execute("select count(*) as n from reflect_memory.memory_items;")
        assert cur.fetchone()["n"] == 0

        # Scope to A via the GUC (set_config so it can be parameterized).
        cur.execute("select set_config('app.current_workspace', %s, false);", (WS_A,))
        cur.execute("select content from reflect_memory.memory_items order by content;")
        assert [r["content"] for r in cur.fetchall()] == ["alpha secret in A"]

        # Switch to B -> only B's row.
        cur.execute("select set_config('app.current_workspace', %s, false);", (WS_B,))
        cur.execute("select content from reflect_memory.memory_items order by content;")
        assert [r["content"] for r in cur.fetchall()] == ["beta secret in B"]

        cur.execute("reset role;")
    conn.commit()
