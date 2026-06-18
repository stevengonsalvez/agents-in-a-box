# ABOUTME: Unit tests for the typed input/record models and their validation.
# ABOUTME: No database — proves tenant scope is mandatory and bad input is rejected early.

from __future__ import annotations

from datetime import datetime, timezone

import pytest

from ainb_reflect_memory.errors import TenantScopeError, ValidationError
from ainb_reflect_memory.models import (
    Edge,
    Entity,
    EvidencePackQuery,
    InsertMemoryInput,
    MemoryItem,
    SearchMemoryInput,
    Tenant,
    UpsertEdgeInput,
    UpsertEntityInput,
)

WS = "11111111-1111-1111-1111-111111111111"


# --------------------------------------------------------------------------- #
# Tenant — the mandatory scope
# --------------------------------------------------------------------------- #


def test_tenant_requires_workspace_id() -> None:
    with pytest.raises(TenantScopeError):
        Tenant(workspace_id="")
    with pytest.raises(TenantScopeError):
        Tenant(workspace_id="   ")


def test_tenant_accepts_optional_subscopes() -> None:
    t = Tenant(workspace_id=WS, agent_id="a", source_session_id="s", user_id="u")
    assert t.workspace_id == WS
    assert t.agent_id == "a"


# --------------------------------------------------------------------------- #
# Input validation
# --------------------------------------------------------------------------- #


def test_insert_memory_input_validates_content_and_confidence() -> None:
    t = Tenant(workspace_id=WS)
    with pytest.raises(ValidationError):
        InsertMemoryInput(tenant=t, content="")
    with pytest.raises(ValidationError):
        InsertMemoryInput(tenant=t, content="ok", confidence=1.5)
    with pytest.raises(ValidationError):
        InsertMemoryInput(tenant=t, content="ok", confidence=-0.1)
    # valid
    ok = InsertMemoryInput(tenant=t, content="ok", confidence=0.9)
    assert ok.source_type == "note"


def test_search_memory_input_requires_query_and_positive_limit() -> None:
    t = Tenant(workspace_id=WS)
    with pytest.raises(ValidationError):
        SearchMemoryInput(tenant=t, query="")
    with pytest.raises(ValidationError):
        SearchMemoryInput(tenant=t, query="x", limit=0)


def test_upsert_entity_input_requires_name_and_type() -> None:
    t = Tenant(workspace_id=WS)
    with pytest.raises(ValidationError):
        UpsertEntityInput(tenant=t, canonical_name="", entity_type="person")
    with pytest.raises(ValidationError):
        UpsertEntityInput(tenant=t, canonical_name="Ada", entity_type="")


def test_upsert_edge_input_requires_endpoints_and_relation() -> None:
    t = Tenant(workspace_id=WS)
    with pytest.raises(ValidationError):
        UpsertEdgeInput(tenant=t, source_entity_id="", target_entity_id="b", relation_type="r")
    with pytest.raises(ValidationError):
        UpsertEdgeInput(tenant=t, source_entity_id="a", target_entity_id="b", relation_type="")


def test_evidence_pack_query_validation() -> None:
    t = Tenant(workspace_id=WS)
    with pytest.raises(ValidationError):
        EvidencePackQuery(tenant=t, query="x", lexical_limit=0)
    with pytest.raises(ValidationError):
        EvidencePackQuery(tenant=t, query="x", neighborhood_depth=-1)
    # depth 0 is allowed (lexical + entities only, no graph expansion)
    ok = EvidencePackQuery(tenant=t, query="x", neighborhood_depth=0)
    assert ok.neighborhood_depth == 0


# --------------------------------------------------------------------------- #
# Record hydration from DB rows
# --------------------------------------------------------------------------- #


def _now() -> datetime:
    return datetime(2026, 6, 18, tzinfo=timezone.utc)


def test_memory_item_from_row_handles_nulls_and_uuid_objects() -> None:
    import uuid

    row = {
        "id": uuid.UUID(WS),
        "workspace_id": uuid.UUID(WS),
        "agent_id": None,
        "source_session_id": None,
        "user_id": None,
        "source_type": "note",
        "source_uri": None,
        "content": "hello",
        "content_hash": "abc",
        "metadata": None,  # NULL jsonb tolerated -> {}
        "confidence": 0.5,
        "created_at": _now(),
        "updated_at": _now(),
    }
    item = MemoryItem.from_row(row)
    assert isinstance(item.id, str)
    assert item.agent_id is None
    assert item.metadata == {}
    assert item.confidence == 0.5


def test_entity_and_edge_from_row() -> None:
    ent = Entity.from_row(
        {
            "id": "e1",
            "workspace_id": WS,
            "canonical_name": "Ada",
            "entity_type": "person",
            "aliases": None,  # NULL -> ()
            "metadata": {"k": "v"},
            "created_at": _now(),
            "updated_at": _now(),
        }
    )
    assert ent.aliases == ()
    assert ent.metadata == {"k": "v"}

    edge = Edge.from_row(
        {
            "id": "x1",
            "workspace_id": WS,
            "source_entity_id": "e1",
            "target_entity_id": "e2",
            "relation_type": "knows",
            "evidence_memory_id": None,
            "weight": 1.0,
            "metadata": None,
            "created_at": _now(),
            "updated_at": _now(),
        }
    )
    assert edge.evidence_memory_id is None
    assert edge.weight == 1.0
