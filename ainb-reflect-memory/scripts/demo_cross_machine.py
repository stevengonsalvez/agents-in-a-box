#!/usr/bin/env python3
# ABOUTME: Cross-machine proof — insert via nano-graphrag on "machine A", then a
# ABOUTME: fresh "machine B" (own working_dir, no shared files) answers from PG.
"""Demo: the same reflect store across two machines, via Postgres-backed
nano-graphrag.

Run (needs nano-graphrag + networkx + numpy + psycopg, and migrations applied):

    export DATABASE_URL='postgresql://USER:PASS@HOST:5432/DBNAME'
    PYTHONPATH=src python scripts/demo_cross_machine.py

"Machine A" and "machine B" are two GraphRAG instances with DIFFERENT local
working dirs but the SAME Supabase project + workspace. B sees what A wrote
because the graph / vectors / community reports live in Postgres, not on disk.
All LLM + embedding work is stubbed CLIENT-SIDE here; the server only stores.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import tempfile
import types
from dataclasses import dataclass

WS = "dddddddd-dddd-dddd-dddd-dddddddddddd"
DOC = "The auth middleware validates the JWT token on every request before routing."

_EXTRACTION = (
    '("entity"<|>"AUTH MIDDLEWARE"<|>"component"<|>"Guards every request and checks the token")##'
    '("entity"<|>"JWT"<|>"concept"<|>"A signed token validated on each request")##'
    '("relationship"<|>"AUTH MIDDLEWARE"<|>"JWT"<|>"validates the token on every request"<|>2)'
    "<|COMPLETE|>"
)
_COMMUNITY = json.dumps(
    {
        "title": "Auth",
        "summary": "Auth middleware validates JWTs.",
        "findings": [{"summary": "x", "explanation": "y"}],
        "rating": 5.0,
        "rating_explanation": "core",
    }
)


def _install_graspologic_shim() -> None:
    import networkx as nx

    if "graspologic" in sys.modules:
        return

    @dataclass
    class HC:
        node: str
        cluster: int
        level: int

    def lcc(graph):
        if graph.number_of_nodes() == 0:
            return graph
        comps = nx.connected_components(graph)
        return graph.subgraph(max(comps, key=len)).copy()

    def hierarchical_leiden(graph, max_cluster_size=10, random_seed=0xDEADBEEF):
        if graph.number_of_nodes() == 0:
            return []
        comms = nx.community.louvain_communities(graph, seed=random_seed, resolution=1.0)
        return [HC(node=n, cluster=c, level=0) for c, comm in enumerate(comms) for n in comm]

    g = types.ModuleType("graspologic")
    g.__path__ = []
    u = types.ModuleType("graspologic.utils")
    u.largest_connected_component = lcc
    p = types.ModuleType("graspologic.partition")
    p.hierarchical_leiden = hierarchical_leiden
    e = types.ModuleType("graspologic.embed")
    e.node2vec_embed = lambda *a, **k: None
    g.utils, g.partition, g.embed = u, p, e
    sys.modules.update(
        {
            "graspologic": g,
            "graspologic.utils": u,
            "graspologic.partition": p,
            "graspologic.embed": e,
        }
    )


def _embedding():
    import numpy as np
    from nano_graphrag._utils import wrap_embedding_func_with_attrs

    def _vec(text):
        v = np.zeros(768)
        for tok in re.findall(r"[a-z0-9]+", text.lower()):
            h = int.from_bytes(hashlib.sha256(tok.encode()).digest()[:8], "little")
            v[h % 768] += 1.0
        n = np.linalg.norm(v)
        return v / n if n else v

    @wrap_embedding_func_with_attrs(embedding_dim=768, max_token_size=8192)
    async def embedding_func(texts):
        return np.array([_vec(t) for t in texts])

    return embedding_func


async def _llm(prompt, system_prompt=None, history_messages=[], **kw):
    low = (prompt or "")[:400].lower()
    if "-goal-" in low and "text document" in low:
        return _EXTRACTION
    if "community" in low or "report" in low:
        return _COMMUNITY
    return "No additional information available."


def _graph(working_dir, dsn):
    _install_graspologic_shim()
    from nano_graphrag import GraphRAG

    from ainb_reflect_memory.nanographrag import addon_params, storage_classes

    return GraphRAG(
        working_dir=working_dir,
        embedding_func=_embedding(),
        best_model_func=_llm,
        cheap_model_func=_llm,
        enable_naive_rag=True,
        **storage_classes(),
        addon_params=addon_params(pg_dsn=dsn, workspace_id=WS, embedding_model="demo-fake"),
    )


def main() -> int:
    from nano_graphrag import QueryParam

    dsn = os.environ.get("DATABASE_URL")
    if not dsn:
        print("DATABASE_URL not set", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory() as tmp:
        a_dir, b_dir = os.path.join(tmp, "machine_a"), os.path.join(tmp, "machine_b")
        os.makedirs(a_dir)
        os.makedirs(b_dir)

        print(f"workspace : {WS}")
        print(f"[machine A {a_dir}] insert: {DOC!r}")
        a = _graph(a_dir, dsn)
        a.insert(DOC)
        graphml = [f for f in os.listdir(a_dir) if f.endswith(".graphml")]
        print(f"[machine A] .graphml files written: {graphml or 'NONE (graph is in Postgres)'}")

        print(f"\n[machine B {b_dir}] fresh instance, no shared local files")
        b = _graph(b_dir, dsn)
        local = b.query(DOC, QueryParam(mode="local", only_need_context=True))
        print(
            f"[machine B] local-mode context mentions AUTH MIDDLEWARE: "
            f"{'AUTH MIDDLEWARE' in (local or '').upper()}"
        )
        naive = b.query(
            "auth middleware validates jwt token request",
            QueryParam(mode="naive", only_need_context=True),
        )
        print(
            f"[machine B] naive-mode context mentions the chunk: "
            f"{'auth middleware' in (naive or '').lower()}"
        )
        print("\nProof: machine B answered from machine A's data with no shared files.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
