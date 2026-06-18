# ABOUTME: Unit tests for the pure SQL builders — no database required.
# ABOUTME: Pins the two security invariants: workspace_id is always the first
# ABOUTME: bound param, and caller values are parameters, never interpolated.

from __future__ import annotations

from ainb_reflect_memory import sql
from ainb_reflect_memory.models import (
    InsertMemoryInput,
    SearchMemoryInput,
    Tenant,
    UpsertEdgeInput,
    UpsertEntityInput,
)
from ainb_reflect_memory.normalize import content_hash

WS = "11111111-1111-1111-1111-111111111111"
OTHER_WS = "22222222-2222-2222-2222-222222222222"


def _tenant() -> Tenant:
    return Tenant(workspace_id=WS, agent_id="agent-1", source_session_id="sess", user_id="u")


# --------------------------------------------------------------------------- #
# Invariant 1: workspace_id is ALWAYS the first bound parameter.
# --------------------------------------------------------------------------- #


def test_every_builder_puts_workspace_id_first() -> None:
    t = _tenant()
    builders = [
        sql.insert_memory(InsertMemoryInput(tenant=t, content="hello world")),
        sql.upsert_entity(UpsertEntityInput(tenant=t, canonical_name="Ada", entity_type="person")),
        sql.upsert_edge(
            UpsertEdgeInput(
                tenant=t, source_entity_id="e1", target_entity_id="e2", relation_type="knows"
            )
        ),
        sql.search_memory(SearchMemoryInput(tenant=t, query="hello")),
        sql.search_entities(t, "Ada", 10),
        sql.entity_neighborhood(t, "e1", 1),
        sql.memory_by_ids(t, ["m1", "m2"]),
        sql.entities_by_ids(t, ["e1"]),
    ]
    for query_text, params in builders:
        assert params[0] == WS, f"workspace_id not first in: {query_text}"
        # %s placeholder style throughout (psycopg), no f-string value splicing.
        assert "%s" in query_text


# --------------------------------------------------------------------------- #
# Invariant 2: caller-supplied VALUES never appear inline in the SQL text.
# --------------------------------------------------------------------------- #


def test_caller_values_are_not_interpolated_into_sql() -> None:
    t = _tenant()
    secret = "Robert'); DROP TABLE memory_items;--"
    query_text, params = sql.insert_memory(
        InsertMemoryInput(tenant=t, content=secret, source_uri="http://x/" + secret)
    )
    # The injection string must travel as a parameter, never spliced into SQL.
    assert secret not in query_text
    assert secret in params  # content carried as a bound value
    # workspace id is a param, not interpolated.
    assert WS not in query_text


# --------------------------------------------------------------------------- #
# insert_memory specifics
# --------------------------------------------------------------------------- #


def test_insert_memory_is_idempotent_upsert_on_content_hash() -> None:
    t = _tenant()
    query_text, params = sql.insert_memory(InsertMemoryInput(tenant=t, content="Fixed the bug"))
    assert "ON CONFLICT (workspace_id, content_hash) DO UPDATE" in query_text
    # content_hash is computed client-side and bound, not recomputed in SQL.
    assert content_hash("Fixed the bug") in params
    # placeholder count matches param count.
    assert query_text.count("%s") == len(params)
    # tenant sub-scopes flow through in order.
    assert params[0:4] == [WS, "agent-1", "sess", "u"]


def test_upsert_entity_conflict_key() -> None:
    t = _tenant()
    query_text, params = sql.upsert_entity(
        UpsertEntityInput(
            tenant=t, canonical_name="Ada", entity_type="person", aliases=["Lovelace"]
        )
    )
    assert "ON CONFLICT (workspace_id, entity_type, canonical_name) DO UPDATE" in query_text
    assert ["Lovelace"] in params


def test_upsert_edge_conflict_key() -> None:
    t = _tenant()
    query_text, params = sql.upsert_edge(
        UpsertEdgeInput(
            tenant=t, source_entity_id="e1", target_entity_id="e2", relation_type="knows"
        )
    )
    assert (
        "ON CONFLICT (workspace_id, source_entity_id, target_entity_id, "
        "             relation_type) DO UPDATE" in query_text
    )


def test_read_builders_call_tenant_scoped_functions() -> None:
    t = _tenant()
    sm_sql, _ = sql.search_memory(SearchMemoryInput(tenant=t, query="x"))
    assert "reflect_memory.search_memory(%s" in sm_sql
    se_sql, _ = sql.search_entities(t, "x", 5)
    assert "reflect_memory.search_entities(%s" in se_sql
    nb_sql, _ = sql.entity_neighborhood(t, "e1", 2)
    assert "reflect_memory.entity_neighborhood(%s" in nb_sql


def test_id_hydration_builders_filter_by_workspace() -> None:
    t = _tenant()
    m_sql, m_params = sql.memory_by_ids(t, ["m1", "m2"])
    assert "WHERE workspace_id = %s" in m_sql
    assert m_params[0] == WS
    e_sql, e_params = sql.entities_by_ids(t, ["e1"])
    assert "WHERE workspace_id = %s" in e_sql
    assert e_params[0] == WS
