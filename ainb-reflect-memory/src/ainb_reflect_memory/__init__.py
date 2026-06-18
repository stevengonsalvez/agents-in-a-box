"""ainb-reflect-memory — Postgres-backed GraphRAG memory substrate.

The server stays *dumb and searchable*: it stores memory, enforces tenancy,
and runs lexical / graph queries. All LLM reasoning (embeddings, entity/edge
extraction, answer synthesis) stays client-side. This package is the typed
boundary between the two.

Public surface (kept deliberately small so it can be lifted into a standalone
``ainb-reflect-memory`` repo unchanged):

    from ainb_reflect_memory import MemoryStore, Tenant, InsertMemoryInput

    store = MemoryStore(conn)                 # conn = psycopg connection
    item = store.insert_memory(InsertMemoryInput(...))
    hits = store.search_memory(SearchMemoryInput(...))
    pack = store.get_evidence_pack(EvidencePackQuery(...))
"""

from .errors import (
    ReflectMemoryError,
    TenantScopeError,
    ValidationError,
)
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
    Tenant,
    UpsertEdgeInput,
    UpsertEntityInput,
)
from .store import MemoryStore

__version__ = "0.1.0"

__all__ = [
    "__version__",
    # store
    "MemoryStore",
    # tenancy
    "Tenant",
    # inputs
    "InsertMemoryInput",
    "SearchMemoryInput",
    "UpsertEntityInput",
    "UpsertEdgeInput",
    "EvidencePackQuery",
    # records
    "MemoryItem",
    "Entity",
    "Edge",
    "SearchResult",
    # evidence pack
    "EvidenceHit",
    "EntityHit",
    "GraphNeighborhood",
    "Citation",
    "EvidencePack",
    # errors
    "ReflectMemoryError",
    "TenantScopeError",
    "ValidationError",
]
