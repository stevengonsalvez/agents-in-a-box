# ABOUTME: Proves the Postgres adapters satisfy nano-graphrag's storage
# ABOUTME: contracts, round-trip across two isolated instances ("machine A/B"),
# ABOUTME: and isolate tenants. Mirrors what the bundled Neo4jStorage must do.

from __future__ import annotations

import asyncio
import json

import pytest

pytestmark = pytest.mark.integration

WS_A = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
WS_B = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"


def make_config(dsn: str, workspace_id: str, working_dir, model: str = "test-fake") -> dict:
    """A minimal nano-graphrag ``global_config`` for instantiating an adapter."""
    return {
        "addon_params": {
            "pg_dsn": dsn,
            "workspace_id": workspace_id,
            "embedding_model": model,
        },
        "embedding_batch_num": 32,
        "query_better_than_threshold": 0.2,
        "max_graph_cluster_size": 10,
        "graph_cluster_seed": 0xDEADBEEF,
        "working_dir": str(working_dir),
        "node2vec_params": {},
    }


def _run(coro):
    return asyncio.run(coro)


# --------------------------------------------------------------------------- #
# PgKVStorage — BaseKVStorage contract
# --------------------------------------------------------------------------- #


def test_kv_roundtrip_and_filter(clean, tmp_path) -> None:
    from ainb_reflect_memory.nanographrag import PgKVStorage

    cfg = make_config(clean, WS_A, tmp_path)
    kv = PgKVStorage(namespace="full_docs", global_config=cfg)

    _run(kv.upsert({"d1": {"content": "alpha", "n": 1}, "d2": {"content": "beta"}}))
    assert _run(kv.get_by_id("d1"))["content"] == "alpha"
    assert _run(kv.get_by_id("missing")) is None
    assert set(_run(kv.all_keys())) == {"d1", "d2"}
    # fields projection
    assert _run(kv.get_by_ids(["d1"], fields={"n"})) == [{"n": 1}]
    # filter_keys returns only the NON-existing keys
    assert _run(kv.filter_keys(["d1", "d3"])) == {"d3"}
    _run(kv.drop())
    assert _run(kv.all_keys()) == []


# --------------------------------------------------------------------------- #
# PgVectorStorage — BaseVectorStorage contract (fake embeddings)
# --------------------------------------------------------------------------- #


def test_vector_upsert_and_nearest_query(clean, tmp_path, fake_embedding) -> None:
    from ainb_reflect_memory.nanographrag import PgVectorStorage

    cfg = make_config(clean, WS_A, tmp_path)
    vdb = PgVectorStorage(
        namespace="entities",
        global_config=cfg,
        embedding_func=fake_embedding,
        meta_fields={"entity_name"},
    )
    _run(
        vdb.upsert(
            {
                "ent-1": {"content": "auth middleware", "entity_name": "AUTH MIDDLEWARE"},
                "ent-2": {"content": "kubernetes pod", "entity_name": "KUBERNETES"},
            }
        )
    )
    # Deterministic fake embeddings => querying an entity's own content returns
    # it as the top hit, carrying its entity_name meta field.
    hits = _run(vdb.query("auth middleware", top_k=2))
    assert hits, "expected at least one hit above threshold"
    assert hits[0]["id"] == "ent-1"
    assert hits[0]["entity_name"] == "AUTH MIDDLEWARE"
    assert hits[0]["distance"] >= 0.99  # self-similarity ~1.0


# --------------------------------------------------------------------------- #
# PgGraphStorage — write on "machine A", read on a fresh "machine B"
# --------------------------------------------------------------------------- #


def test_graph_roundtrips_across_isolated_instances(clean, tmp_path) -> None:
    from ainb_reflect_memory.nanographrag import PgGraphStorage

    cfg = make_config(clean, WS_A, tmp_path / "a")

    async def write_on_a():
        a = PgGraphStorage(namespace="chunk_entity_relation", global_config=cfg)
        await a.upsert_node(
            "AUTH MIDDLEWARE",
            {"entity_type": "component", "description": "guards requests", "source_id": "chunk-1"},
        )
        await a.upsert_node(
            "JWT", {"entity_type": "concept", "description": "a token", "source_id": "chunk-1"}
        )
        await a.upsert_edge(
            "AUTH MIDDLEWARE",
            "JWT",
            {"weight": 2.0, "description": "validates", "source_id": "chunk-1"},
        )
        await a.index_done_callback()  # persist to Postgres

    _run(write_on_a())

    cfg_b = make_config(clean, WS_A, tmp_path / "b")  # different working_dir = fresh machine

    async def read_on_b():
        b = PgGraphStorage(namespace="chunk_entity_relation", global_config=cfg_b)
        assert await b.has_node("AUTH MIDDLEWARE")
        assert await b.has_edge("AUTH MIDDLEWARE", "JWT")
        node = await b.get_node("JWT")
        assert node["description"] == "a token"
        assert await b.node_degree("AUTH MIDDLEWARE") == 1
        edge = await b.get_edge("AUTH MIDDLEWARE", "JWT")
        assert float(edge["weight"]) == 2.0
        edges = await b.get_node_edges("AUTH MIDDLEWARE")
        assert ("AUTH MIDDLEWARE", "JWT") in [tuple(e) for e in edges] or (
            "JWT",
            "AUTH MIDDLEWARE",
        ) in [tuple(e) for e in edges]

    _run(read_on_b())


def test_graph_idempotent_reupsert(clean, tmp_path) -> None:
    """Re-saving the same graph does not create duplicate node/edge rows."""
    import psycopg

    from ainb_reflect_memory.nanographrag import PgGraphStorage

    cfg = make_config(clean, WS_A, tmp_path)

    async def write_twice():
        for _ in range(2):
            g = PgGraphStorage(namespace="chunk_entity_relation", global_config=cfg)
            await g.upsert_node("AUTH MIDDLEWARE", {"entity_type": "component", "source_id": "c1"})
            await g.upsert_node("JWT", {"entity_type": "concept", "source_id": "c1"})
            await g.upsert_edge("AUTH MIDDLEWARE", "JWT", {"weight": 1.0, "source_id": "c1"})
            await g.index_done_callback()

    _run(write_twice())
    c = psycopg.connect(clean, autocommit=True)
    with c.cursor() as cur:
        cur.execute(
            "select count(*) from reflect_memory.ng_graph_nodes where workspace_id=%s", (WS_A,)
        )
        n_nodes = cur.fetchone()[0]
        cur.execute(
            "select count(*) from reflect_memory.ng_graph_edges where workspace_id=%s", (WS_A,)
        )
        n_edges = cur.fetchone()[0]
    c.close()
    assert n_nodes == 2
    assert n_edges == 1


def test_community_schema_reads_clusters(clean, tmp_path) -> None:
    """community_schema() (inherited from NetworkXStorage) works over a
    PG-loaded graph once nodes carry `clusters` — i.e. global-mode metadata
    round-trips through Postgres."""
    from ainb_reflect_memory.nanographrag import PgGraphStorage

    cfg = make_config(clean, WS_A, tmp_path)
    clusters = json.dumps([{"level": 0, "cluster": 0}])

    async def build_and_read():
        g = PgGraphStorage(namespace="chunk_entity_relation", global_config=cfg)
        await g.upsert_node("AUTH MIDDLEWARE", {"source_id": "c1", "clusters": clusters})
        await g.upsert_node("JWT", {"source_id": "c1", "clusters": clusters})
        await g.upsert_edge("AUTH MIDDLEWARE", "JWT", {"weight": 1.0, "source_id": "c1"})
        await g.index_done_callback()
        # fresh instance loads from PG, then computes the community schema
        g2 = PgGraphStorage(namespace="chunk_entity_relation", global_config=cfg)
        return await g2.community_schema()

    schema = _run(build_and_read())
    assert "0" in schema
    assert set(schema["0"]["nodes"]) == {"AUTH MIDDLEWARE", "JWT"}


# --------------------------------------------------------------------------- #
# Tenant isolation across all three stores
# --------------------------------------------------------------------------- #


def test_tenant_isolation_across_stores(clean, tmp_path, fake_embedding) -> None:
    from ainb_reflect_memory.nanographrag import (
        PgGraphStorage,
        PgKVStorage,
        PgVectorStorage,
    )

    cfg_a = make_config(clean, WS_A, tmp_path / "a")
    cfg_b = make_config(clean, WS_B, tmp_path / "b")

    async def seed_a():
        kv = PgKVStorage(namespace="full_docs", global_config=cfg_a)
        await kv.upsert({"d1": {"content": "secret in A"}})
        vdb = PgVectorStorage(
            namespace="entities",
            global_config=cfg_a,
            embedding_func=fake_embedding,
            meta_fields={"entity_name"},
        )
        await vdb.upsert({"ent-1": {"content": "auth", "entity_name": "AUTH"}})
        g = PgGraphStorage(namespace="chunk_entity_relation", global_config=cfg_a)
        await g.upsert_node("AUTH", {"source_id": "c1"})
        await g.index_done_callback()

    async def assert_b_sees_nothing():
        kv = PgKVStorage(namespace="full_docs", global_config=cfg_b)
        assert await kv.get_by_id("d1") is None
        assert await kv.all_keys() == []
        vdb = PgVectorStorage(
            namespace="entities",
            global_config=cfg_b,
            embedding_func=fake_embedding,
            meta_fields={"entity_name"},
        )
        assert await vdb.query("auth", top_k=5) == []
        g = PgGraphStorage(namespace="chunk_entity_relation", global_config=cfg_b)
        assert not await g.has_node("AUTH")

    _run(seed_a())
    _run(assert_b_sees_nothing())


# --------------------------------------------------------------------------- #
# Cross-machine parity — two isolated instances, identical ranked results
# --------------------------------------------------------------------------- #


def _bow_embedding():
    """Deterministic feature-hashing embedding so shared-vocabulary texts get a
    positive cosine similarity (ordering is meaningful, not just exact-match)."""
    import hashlib
    import re

    import numpy as np

    def _vec(text: str):
        v = np.zeros(768)
        for tok in re.findall(r"[a-z0-9]+", text.lower()):
            h = int.from_bytes(hashlib.sha256(tok.encode()).digest()[:8], "little")
            v[h % 768] += 1.0
        n = np.linalg.norm(v)
        return v / n if n else v

    async def embedding_func(texts):
        return np.array([_vec(t) for t in texts])

    embedding_func.embedding_dim = 768
    return embedding_func


def test_cross_machine_vector_parity(clean, tmp_path) -> None:
    """Two isolated instances (machine-A dir, machine-B dir) on the SAME PG +
    tenant return IDENTICAL top-k ids AND scores for the same query — the
    shared-store parity guarantee. Seeded once; queried independently."""
    from ainb_reflect_memory.nanographrag import PgVectorStorage

    emb = _bow_embedding()
    cfg_a = make_config(clean, WS_A, tmp_path / "a")
    cfg_b = make_config(clean, WS_A, tmp_path / "b")  # same tenant, different machine

    seeder = PgVectorStorage(
        namespace="entities", global_config=cfg_a, embedding_func=emb, meta_fields={"entity_name"}
    )
    corpus = [
        "auth middleware token",
        "jwt token validation",
        "kubernetes pod scheduling",
        "redis cache layer",
        "oauth login flow",
    ]
    _run(
        seeder.upsert(
            {f"ent-{i}": {"content": c, "entity_name": c.upper()} for i, c in enumerate(corpus)}
        )
    )

    a = PgVectorStorage(
        namespace="entities", global_config=cfg_a, embedding_func=emb, meta_fields={"entity_name"}
    )
    b = PgVectorStorage(
        namespace="entities", global_config=cfg_b, embedding_func=emb, meta_fields={"entity_name"}
    )
    qa = _run(a.query("auth token validation", top_k=5))
    qb = _run(b.query("auth token validation", top_k=5))

    assert qa, "expected ranked hits"
    assert len(qa) >= 2, "expected several hits so ordering parity is meaningful"
    assert [h["id"] for h in qa] == [h["id"] for h in qb]  # identical ranking
    assert [round(h["distance"], 6) for h in qa] == [round(h["distance"], 6) for h in qb]


# --------------------------------------------------------------------------- #
# Dynamic "server stays dumb" — providers poisoned, storage path still works
# --------------------------------------------------------------------------- #


def test_storage_path_works_with_providers_poisoned(clean, tmp_path, fake_embedding) -> None:
    """Runtime complement to the static scan: poison openai/anthropic/cohere in
    sys.modules so ANY access raises, then exercise the full storage path. It
    must still work — proving the adapters never touch an LLM/embedding provider
    at runtime (embedding is the injected client-side func)."""
    import sys
    import types

    from ainb_reflect_memory.nanographrag import PgKVStorage, PgVectorStorage

    class _Poison(types.ModuleType):
        def __getattr__(self, name):
            raise AssertionError(f"provider touched at runtime: {self.__name__}.{name}")

    targets = ("openai", "anthropic", "cohere")
    saved = {m: sys.modules.get(m) for m in targets}
    for m in targets:
        sys.modules[m] = _Poison(m)
    try:
        cfg = make_config(clean, WS_A, tmp_path)
        kv = PgKVStorage(namespace="full_docs", global_config=cfg)
        _run(kv.upsert({"d1": {"content": "x"}}))
        assert _run(kv.get_by_id("d1"))["content"] == "x"

        vdb = PgVectorStorage(
            namespace="entities",
            global_config=cfg,
            embedding_func=fake_embedding,
            meta_fields={"entity_name"},
        )
        _run(vdb.upsert({"e1": {"content": "auth", "entity_name": "AUTH"}}))
        assert _run(vdb.query("auth", top_k=1))[0]["id"] == "e1"
    finally:
        for m, v in saved.items():
            if v is None:
                sys.modules.pop(m, None)
            else:
                sys.modules[m] = v
