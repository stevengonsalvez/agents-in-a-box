"""PgVectorStorage — nano-graphrag ``BaseVectorStorage`` backed by Postgres
``ng_vectors`` + pgvector.

Embeddings are computed CLIENT-SIDE by the ``embedding_func`` nano-graphrag
INJECTS (sentence-transformers in reflect). This adapter never builds its own
model and never imports an embedding/LLM provider — it only takes the vectors
the injected function returns and stores / ANN-queries them in Postgres. The
database does the cosine ANN; it does no embedding.

Two namespaces, matching nano-graphrag: ``entities`` (meta_fields={'entity_name'})
and ``chunks`` (id-only). Vectors are unit-normalized, so cosine distance
``<=>`` gives similarity = ``1 - distance``; we filter by
``cosine_better_than_threshold`` exactly like NanoVectorDBStorage.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass

import numpy as np
from nano_graphrag.base import BaseVectorStorage

from ._conn import PgBackend, resolve_config, vector_literal

__all__ = ["PgVectorStorage"]

_TABLE = "reflect_memory.ng_vectors"


@dataclass
class PgVectorStorage(BaseVectorStorage):
    cosine_better_than_threshold: float = 0.2

    def __post_init__(self) -> None:
        dsn, workspace_id, model = resolve_config(self.global_config)
        self._pg = PgBackend.shared(dsn, workspace_id)
        self._ws = workspace_id
        self._model = model
        self._dim = int(self.embedding_func.embedding_dim)
        self._max_batch = self.global_config.get("embedding_batch_num", 32)
        self.cosine_better_than_threshold = self.global_config.get(
            "query_better_than_threshold", self.cosine_better_than_threshold
        )

    async def upsert(self, data: dict[str, dict]):
        if not data:
            return []
        # id + only the configured meta_fields (e.g. entity_name) are stored;
        # content is used solely to compute the embedding (matches NanoVectorDB).
        list_data = [
            {"__id__": k, **{mk: v[mk] for mk in self.meta_fields if mk in v}}
            for k, v in data.items()
        ]
        contents = [v["content"] for v in data.values()]
        batches = [
            contents[i : i + self._max_batch] for i in range(0, len(contents), self._max_batch)
        ]
        embeddings_list = await asyncio.gather(*[self.embedding_func(batch) for batch in batches])
        embeddings = np.concatenate(embeddings_list)

        from psycopg.types.json import Jsonb

        rows = []
        for i, d in enumerate(list_data):
            meta = {k: v for k, v in d.items() if k != "__id__"}
            rows.append(
                (
                    self._ws,
                    self.namespace,
                    d["__id__"],
                    Jsonb(meta),
                    self._model,
                    self._dim,
                    vector_literal(embeddings[i]),
                )
            )
        self._pg.executemany(
            f"insert into {_TABLE} "
            "(workspace_id, namespace, id, meta, model, dims, embedding) "
            "values (%s,%s,%s,%s,%s,%s,%s::vector) "
            "on conflict (workspace_id, namespace, id) do update set "
            "  meta=excluded.meta, model=excluded.model, dims=excluded.dims, "
            "  embedding=excluded.embedding, updated_at=now()",
            rows,
        )
        return list_data

    async def query(self, query: str, top_k: int = 5) -> list[dict]:
        embedding = (await self.embedding_func([query]))[0]
        q = vector_literal(embedding)
        rows = self._pg.fetchall(
            "select id, meta, 1 - (embedding <=> %s::vector) as distance "
            f"from {_TABLE} where workspace_id=%s and namespace=%s "
            "order by embedding <=> %s::vector limit %s",
            (q, self._ws, self.namespace, q, int(top_k)),
        )
        results = []
        for r in rows:
            similarity = float(r["distance"])
            if similarity < self.cosine_better_than_threshold:
                continue
            results.append({**(r["meta"] or {}), "id": r["id"], "distance": similarity})
        return results
