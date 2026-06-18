"""Typed input/record models for the reflect memory substrate.

These dataclasses ARE the API boundary. They validate early (in
``__post_init__``) so bad input never reaches SQL, and they carry the tenant on
every operation so a query can never be built without one. Ids are plain
strings (UUID text) for portability — psycopg adapts them to ``uuid`` columns,
and a future non-Python client can produce the same shapes.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any, Mapping, Optional, Sequence

from .errors import TenantScopeError, ValidationError

__all__ = [
    "Tenant",
    "InsertMemoryInput",
    "SearchMemoryInput",
    "UpsertEntityInput",
    "UpsertEdgeInput",
    "EvidencePackQuery",
    "MemoryItem",
    "Entity",
    "Edge",
    "SearchResult",
    "EvidenceHit",
    "EntityHit",
    "GraphNeighborhood",
    "Citation",
    "EvidencePack",
    "SOURCE_TYPES",
]

# Recommended source types (from the spec). Not an allow-list — callers may use
# their own — but documents the intended vocabulary.
SOURCE_TYPES = frozenset(
    {
        "session_summary",
        "user_preference",
        "project_fact",
        "observed_event",
        "codebase_note",
        "decision_record",
        "correction",
        "note",
    }
)


def _require_nonempty(value: object, field_name: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValidationError(f"{field_name} must be a non-empty string")
    return value


@dataclass(frozen=True)
class Tenant:
    """The mandatory tenant scope for every operation.

    ``workspace_id`` is the hard isolation boundary. ``agent_id``,
    ``source_session_id`` and ``user_id`` are optional sub-scopes used for
    provenance and optional filtering — they never widen access beyond the
    workspace.
    """

    workspace_id: str
    agent_id: Optional[str] = None
    source_session_id: Optional[str] = None
    user_id: Optional[str] = None

    def __post_init__(self) -> None:
        # A missing workspace is fatal: it is the only thing standing between
        # one tenant's memory and another's.
        if not isinstance(self.workspace_id, str) or not self.workspace_id.strip():
            raise TenantScopeError("workspace_id is required for every operation")


@dataclass(frozen=True)
class InsertMemoryInput:
    tenant: Tenant
    content: str
    source_type: str = "note"
    source_uri: Optional[str] = None
    metadata: Mapping[str, Any] = field(default_factory=dict)
    confidence: float = 0.5

    def __post_init__(self) -> None:
        _require_nonempty(self.content, "content")
        _require_nonempty(self.source_type, "source_type")
        if not (0.0 <= float(self.confidence) <= 1.0):
            raise ValidationError("confidence must be in [0.0, 1.0]")


@dataclass(frozen=True)
class SearchMemoryInput:
    tenant: Tenant
    query: str
    limit: int = 10
    # Optional extra filter: restrict to one agent's memories within the tenant.
    agent_id: Optional[str] = None
    # Drop hits below this lexical rank (ts_rank). None = keep all.
    min_rank: Optional[float] = None

    def __post_init__(self) -> None:
        _require_nonempty(self.query, "query")
        if int(self.limit) <= 0:
            raise ValidationError("limit must be > 0")


@dataclass(frozen=True)
class UpsertEntityInput:
    tenant: Tenant
    canonical_name: str
    entity_type: str
    aliases: Sequence[str] = field(default_factory=tuple)
    metadata: Mapping[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        _require_nonempty(self.canonical_name, "canonical_name")
        _require_nonempty(self.entity_type, "entity_type")


@dataclass(frozen=True)
class UpsertEdgeInput:
    tenant: Tenant
    source_entity_id: str
    target_entity_id: str
    relation_type: str
    evidence_memory_id: Optional[str] = None
    weight: float = 1.0
    metadata: Mapping[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        _require_nonempty(self.source_entity_id, "source_entity_id")
        _require_nonempty(self.target_entity_id, "target_entity_id")
        _require_nonempty(self.relation_type, "relation_type")


@dataclass(frozen=True)
class EvidencePackQuery:
    tenant: Tenant
    query: str
    lexical_limit: int = 10
    entity_limit: int = 10
    neighborhood_depth: int = 1

    def __post_init__(self) -> None:
        _require_nonempty(self.query, "query")
        if int(self.lexical_limit) <= 0 or int(self.entity_limit) <= 0:
            raise ValidationError("limits must be > 0")
        if int(self.neighborhood_depth) < 0:
            raise ValidationError("neighborhood_depth must be >= 0")


# --------------------------------------------------------------------------- #
# Records returned from the database.
# --------------------------------------------------------------------------- #


@dataclass(frozen=True)
class MemoryItem:
    id: str
    workspace_id: str
    agent_id: Optional[str]
    source_session_id: Optional[str]
    user_id: Optional[str]
    source_type: str
    source_uri: Optional[str]
    content: str
    content_hash: str
    metadata: Mapping[str, Any]
    confidence: float
    created_at: datetime
    updated_at: datetime

    @classmethod
    def from_row(cls, row: Mapping[str, Any]) -> "MemoryItem":
        return cls(
            id=str(row["id"]),
            workspace_id=str(row["workspace_id"]),
            agent_id=_opt_str(row.get("agent_id")),
            source_session_id=_opt_str(row.get("source_session_id")),
            user_id=_opt_str(row.get("user_id")),
            source_type=row["source_type"],
            source_uri=row.get("source_uri"),
            content=row["content"],
            content_hash=row["content_hash"],
            metadata=row.get("metadata") or {},
            confidence=float(row["confidence"]),
            created_at=row["created_at"],
            updated_at=row["updated_at"],
        )


@dataclass(frozen=True)
class Entity:
    id: str
    workspace_id: str
    canonical_name: str
    entity_type: str
    aliases: Sequence[str]
    metadata: Mapping[str, Any]
    created_at: datetime
    updated_at: datetime

    @classmethod
    def from_row(cls, row: Mapping[str, Any]) -> "Entity":
        return cls(
            id=str(row["id"]),
            workspace_id=str(row["workspace_id"]),
            canonical_name=row["canonical_name"],
            entity_type=row["entity_type"],
            aliases=tuple(row.get("aliases") or ()),
            metadata=row.get("metadata") or {},
            created_at=row["created_at"],
            updated_at=row["updated_at"],
        )


@dataclass(frozen=True)
class Edge:
    id: str
    workspace_id: str
    source_entity_id: str
    target_entity_id: str
    relation_type: str
    evidence_memory_id: Optional[str]
    weight: float
    metadata: Mapping[str, Any]
    created_at: datetime
    updated_at: datetime

    @classmethod
    def from_row(cls, row: Mapping[str, Any]) -> "Edge":
        return cls(
            id=str(row["id"]),
            workspace_id=str(row["workspace_id"]),
            source_entity_id=str(row["source_entity_id"]),
            target_entity_id=str(row["target_entity_id"]),
            relation_type=row["relation_type"],
            evidence_memory_id=_opt_str(row.get("evidence_memory_id")),
            weight=float(row["weight"]),
            metadata=row.get("metadata") or {},
            created_at=row["created_at"],
            updated_at=row["updated_at"],
        )


@dataclass(frozen=True)
class SearchResult:
    """A lexical hit: the memory item plus its rank and a highlighted snippet."""

    item: MemoryItem
    rank: float
    snippet: str


# --------------------------------------------------------------------------- #
# Evidence pack — pure retrieval, no synthesis. The local agent turns this into
# an answer; the server never does.
# --------------------------------------------------------------------------- #


@dataclass(frozen=True)
class EvidenceHit:
    memory_id: str
    content: str
    rank: float
    snippet: str
    source_type: str
    source_uri: Optional[str]


@dataclass(frozen=True)
class EntityHit:
    entity_id: str
    canonical_name: str
    entity_type: str
    matched_alias: Optional[str] = None


@dataclass(frozen=True)
class GraphNeighborhood:
    entities: Sequence[Entity]
    edges: Sequence[Edge]


@dataclass(frozen=True)
class Citation:
    memory_id: str
    source_type: str
    source_uri: Optional[str]


@dataclass(frozen=True)
class EvidencePack:
    query: str
    tenant: Tenant
    lexical: Sequence[EvidenceHit]
    entities: Sequence[EntityHit]
    graph: GraphNeighborhood
    citations: Sequence[Citation]


def _opt_str(value: object) -> Optional[str]:
    return None if value is None else str(value)
