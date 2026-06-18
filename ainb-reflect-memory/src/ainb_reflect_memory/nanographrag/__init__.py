"""Postgres-backed storage backends for nano-graphrag.

This optional submodule lets reflect's nano-graphrag run UNCHANGED against the
shared ainb-reflect-memory Postgres, so the same vectors, entity/relation graph,
and community reports are visible from every machine. It requires the
``[nanographrag]`` extra (nano-graphrag + its client stack) and ``[pg]``.

Usage (client side — e.g. reflect-kb's LearningsGraphEngine):

    from nano_graphrag import GraphRAG
    from ainb_reflect_memory.nanographrag import storage_classes, addon_params

    graph = GraphRAG(
        working_dir=tmp_dir,                 # only for transient artifacts
        **storage_classes(),                 # PG-backed graph / vector / KV
        addon_params=addon_params(
            pg_dsn=DATABASE_URL,
            workspace_id="…uuid…",
            embedding_model="all-mpnet-base-v2",
        ),
        # embedding_func + LLM funcs stay client-side, as always.
    )

The database does NO embedding and NO LLM work — embeddings arrive pre-computed
from the injected ``embedding_func`` and Leiden clustering runs in-process.
"""

from __future__ import annotations

from .graph import PgGraphStorage
from .kv import PgKVStorage
from .vectors import PgVectorStorage

__all__ = [
    "PgKVStorage",
    "PgVectorStorage",
    "PgGraphStorage",
    "storage_classes",
    "addon_params",
]


def storage_classes() -> dict:
    """The ``GraphRAG(...)`` kwargs that select the Postgres backends."""
    return {
        "key_string_value_json_storage_cls": PgKVStorage,
        "vector_db_storage_cls": PgVectorStorage,
        "graph_storage_cls": PgGraphStorage,
    }


def addon_params(
    pg_dsn: str,
    workspace_id: str,
    embedding_model: str = "all-mpnet-base-v2",
) -> dict:
    """Build the ``addon_params`` the adapters read for DSN + tenant + model."""
    return {
        "pg_dsn": pg_dsn,
        "workspace_id": workspace_id,
        "embedding_model": embedding_model,
    }
