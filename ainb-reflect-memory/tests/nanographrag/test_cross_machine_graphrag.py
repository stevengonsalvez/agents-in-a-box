# ABOUTME: Capstone — a REAL nano-graphrag GraphRAG pipeline runs UNCHANGED on
# ABOUTME: the Postgres backends: insert on "machine A", then a fresh "machine B"
# ABOUTME: (own working_dir, no shared local files) answers local + naive from PG.

from __future__ import annotations

import json

import pytest

pytestmark = pytest.mark.integration

WS = "cccccccc-cccc-cccc-cccc-cccccccccccc"

# A canned entity-extraction result in nano-graphrag's exact wire format
# (tuple "<|>", record "##", completion "<|COMPLETE|>"). Stands in for the
# client-side LLM extraction — which is the whole point: the LLM stays on the
# client, the server only stores what the client produced.
_EXTRACTION = (
    '("entity"<|>"AUTH MIDDLEWARE"<|>"component"<|>"Guards every request and checks the token")##'
    '("entity"<|>"JWT"<|>"concept"<|>"A signed token validated on each request")##'
    '("relationship"<|>"AUTH MIDDLEWARE"<|>"JWT"<|>"validates the token on every request"<|>2)'
    "<|COMPLETE|>"
)

_COMMUNITY_JSON = json.dumps(
    {
        "title": "Auth subsystem",
        "summary": "Auth middleware validates JWTs on each request.",
        "findings": [{"summary": "token validation", "explanation": "auth middleware uses JWT"}],
        "rating": 5.0,
        "rating_explanation": "core security path",
    }
)

DOC = "The auth middleware validates the JWT token on every request before routing."


def _fake_embedding():
    """Deterministic feature-hashing bag-of-words embedding (no model, no
    network). Texts that SHARE vocabulary get a positive cosine similarity, so
    local/naive retrieval actually finds the inserted content — unlike a random
    per-text vector. Good enough to drive the real query pipeline in a test."""
    import hashlib
    import re

    import numpy as np
    from nano_graphrag._utils import wrap_embedding_func_with_attrs

    def _vec(text: str):
        v = np.zeros(768)
        for tok in re.findall(r"[a-z0-9]+", text.lower()):
            h = int.from_bytes(hashlib.sha256(tok.encode()).digest()[:8], "little")
            v[h % 768] += 1.0
        norm = np.linalg.norm(v)
        return v / norm if norm > 0 else v

    @wrap_embedding_func_with_attrs(embedding_dim=768, max_token_size=8192)
    async def embedding_func(texts):
        return np.array([_vec(t) for t in texts])

    return embedding_func


_GLOBAL_POINTS = json.dumps(
    {"points": [{"description": "Auth middleware validates the JWT token", "score": 10}]}
)


async def _canned_llm(prompt, system_prompt=None, history_messages=[], **kwargs):
    """Client-side LLM stand-in: returns pre-extracted entities / a community
    report / global-map points, mirroring reflect-kb's passthrough. No external
    API call — the whole point is the LLM stays client-side and swappable."""
    kwargs.pop("hashing_kv", None)
    low = (prompt or "")[:400].lower()
    sys_low = (system_prompt or "").lower()
    if "-goal-" in low and "text document" in low:
        return _EXTRACTION
    # Global query map step: nano-graphrag asks (via system prompt) for scored
    # "points" as JSON. Return one so global mode produces real context.
    if "points" in sys_low and "score" in sys_low:
        return _GLOBAL_POINTS
    if "community" in low or "report" in low or "community" in sys_low:
        return _COMMUNITY_JSON
    return "No additional information available."


def _install_graspologic_shim() -> None:
    """Shim the 3 graspologic functions nano-graphrag uses with pure networkx
    (Louvain stands in for Leiden), so clustering runs without the heavy
    numba/llvmlite chain — the same approach reflect-kb takes. Self-contained
    so this test depends only on nano_graphrag + networkx."""
    import sys
    import types
    from dataclasses import dataclass

    import networkx as nx

    if "graspologic" in sys.modules:
        return

    @dataclass
    class HierarchicalCluster:
        node: str
        cluster: int
        level: int

    def largest_connected_component(graph):
        if graph.number_of_nodes() == 0:
            return graph
        comps = (
            nx.weakly_connected_components(graph)
            if graph.is_directed()
            else nx.connected_components(graph)
        )
        return graph.subgraph(max(comps, key=len)).copy()

    def hierarchical_leiden(graph, max_cluster_size=10, random_seed=0xDEADBEEF):
        if graph.number_of_nodes() == 0:
            return []
        comms = nx.community.louvain_communities(graph, seed=random_seed, resolution=1.0)
        return [
            HierarchicalCluster(node=n, cluster=cid, level=0)
            for cid, comm in enumerate(comms)
            for n in comm
        ]

    def node2vec_embed(graph, **kwargs):
        raise NotImplementedError

    g = types.ModuleType("graspologic")
    g.__path__ = []
    u = types.ModuleType("graspologic.utils")
    u.largest_connected_component = largest_connected_component
    p = types.ModuleType("graspologic.partition")
    p.hierarchical_leiden = hierarchical_leiden
    e = types.ModuleType("graspologic.embed")
    e.node2vec_embed = node2vec_embed
    g.utils, g.partition, g.embed = u, p, e
    sys.modules.update(
        {
            "graspologic": g,
            "graspologic.utils": u,
            "graspologic.partition": p,
            "graspologic.embed": e,
        }
    )


def _make_graph(working_dir, dsn):
    _install_graspologic_shim()
    from nano_graphrag import GraphRAG

    from ainb_reflect_memory.nanographrag import addon_params, storage_classes

    return GraphRAG(
        working_dir=str(working_dir),
        embedding_func=_fake_embedding(),
        best_model_func=_canned_llm,
        cheap_model_func=_canned_llm,
        enable_naive_rag=True,
        **storage_classes(),
        addon_params=addon_params(pg_dsn=dsn, workspace_id=WS, embedding_model="test-fake"),
    )


def test_full_pipeline_write_a_read_b(clean, tmp_path) -> None:
    import hashlib

    from nano_graphrag import QueryParam

    # A stand-in markdown file KB (reflect's source of truth). The PG backend
    # must never touch it — record its hash to prove "file KB untouched".
    kb = tmp_path / "learnings_documents"
    kb.mkdir()
    md = kb / "auth.md"
    md.write_text("# Auth\nThe auth middleware validates the JWT token.\n")
    kb_hash_before = hashlib.sha256(md.read_bytes()).hexdigest()

    # --- machine A: insert through the real GraphRAG pipeline, persist to PG ---
    a = _make_graph(tmp_path / "machine_a", clean)
    a.insert(DOC)

    # No graphml file should have been written for the shared graph (it lives
    # in Postgres now).
    assert not list((tmp_path / "machine_a").glob("*.graphml"))

    # --- machine B: a fresh instance, different working_dir, same PG/tenant ---
    b = _make_graph(tmp_path / "machine_b", clean)

    local_ctx = b.query(DOC, QueryParam(mode="local", only_need_context=True))
    assert "AUTH MIDDLEWARE" in local_ctx.upper()

    naive_ctx = b.query(
        "auth middleware validates jwt token request",
        QueryParam(mode="naive", only_need_context=True),
    )
    assert "auth middleware" in naive_ctx.lower()

    # Global mode: answered from the community report generated on A and stored
    # in Postgres (ng_kv), retrieved + map-reduced on B.
    global_ctx = b.query(DOC, QueryParam(mode="global", only_need_context=True, level=2))
    assert "auth middleware" in global_ctx.lower()

    # The markdown file KB is the source of truth and was NOT touched by the
    # PG-backed pipeline.
    assert md.exists()
    assert hashlib.sha256(md.read_bytes()).hexdigest() == kb_hash_before


def test_full_pipeline_is_idempotent(clean, tmp_path) -> None:
    import psycopg

    a1 = _make_graph(tmp_path / "a1", clean)
    a1.insert(DOC)
    a2 = _make_graph(tmp_path / "a2", clean)
    a2.insert(DOC)  # same doc again, fresh instance

    c = psycopg.connect(clean, autocommit=True)
    with c.cursor() as cur:
        cur.execute(
            "select count(*) from reflect_memory.ng_graph_nodes where workspace_id=%s", (WS,)
        )
        nodes = cur.fetchone()[0]
    c.close()
    # The two real entities (AUTH MIDDLEWARE, JWT) — not duplicated by re-insert.
    assert nodes == 2
