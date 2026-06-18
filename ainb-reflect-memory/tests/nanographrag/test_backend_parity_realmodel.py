# ABOUTME: Tier B — backend parity with the REAL all-mpnet-base-v2 embedding
# ABOUTME: model (not a fake): local vs Postgres must return the same evidence
# ABOUTME: for naive/local/global. Skips unless sentence-transformers is present.

from __future__ import annotations

import sys
import types
from dataclasses import dataclass

import pytest

pytestmark = pytest.mark.integration

WS = "ffffffff-ffff-ffff-ffff-ffffffffffff"

CORPUS = [
    (
        "The auth middleware validates the JWT token on every request before routing.",
        '("entity"<|>"AUTH MIDDLEWARE"<|>"component"<|>"validates tokens")##'
        '("entity"<|>"JWT"<|>"concept"<|>"a signed token")##'
        '("relationship"<|>"AUTH MIDDLEWARE"<|>"JWT"<|>"validates the token"<|>2)<|COMPLETE|>',
    ),
    (
        "Kubernetes pod autoscaling scales replicas on CPU and request latency.",
        '("entity"<|>"KUBERNETES"<|>"platform"<|>"orchestrates pods")##'
        '("entity"<|>"AUTOSCALER"<|>"component"<|>"scales replicas")##'
        '("relationship"<|>"KUBERNETES"<|>"AUTOSCALER"<|>"runs the autoscaler"<|>1)<|COMPLETE|>',
    ),
]
QUERIES = ["how does auth validate the token", "scaling kubernetes pods"]
MODES = ["naive", "local", "global"]
_ENTITIES = ["AUTH MIDDLEWARE", "JWT", "KUBERNETES", "AUTOSCALER"]

import json  # noqa: E402

_GLOBAL_POINTS = json.dumps(
    {"points": [{"description": "auth validates the JWT token", "score": 9}]}
)
_COMMUNITY = json.dumps(
    {
        "title": "c",
        "summary": "auth + jwt",
        "findings": [{"summary": "x", "explanation": "y"}],
        "rating": 5.0,
        "rating_explanation": "core",
    }
)
_queue = []


def _install_shim():
    import networkx as nx

    if "graspologic" in sys.modules:
        return

    @dataclass
    class HC:
        node: str
        cluster: int
        level: int

    def lcc(g):
        return (
            g
            if g.number_of_nodes() == 0
            else g.subgraph(max(nx.connected_components(g), key=len)).copy()
        )

    def hier(g, max_cluster_size=10, random_seed=0xDEADBEEF):
        if g.number_of_nodes() == 0:
            return []
        comms = nx.community.louvain_communities(g, seed=random_seed, resolution=1.0)
        return [HC(node=n, cluster=c, level=0) for c, cm in enumerate(comms) for n in cm]

    g = types.ModuleType("graspologic")
    g.__path__ = []
    u = types.ModuleType("graspologic.utils")
    u.largest_connected_component = lcc
    p = types.ModuleType("graspologic.partition")
    p.hierarchical_leiden = hier
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


def _real_embedding():
    """The REAL all-mpnet-base-v2 embedding func nano-graphrag expects."""
    import numpy as np
    from nano_graphrag._utils import wrap_embedding_func_with_attrs
    from sentence_transformers import SentenceTransformer

    model = SentenceTransformer("all-mpnet-base-v2")

    @wrap_embedding_func_with_attrs(embedding_dim=768, max_token_size=8192)
    async def ef(texts):
        return np.asarray(model.encode(list(texts), normalize_embeddings=True))

    return ef


async def _canned_llm(prompt, system_prompt=None, history_messages=[], **kw):
    low = (prompt or "")[:400].lower()
    sys_low = (system_prompt or "").lower()
    if "-goal-" in low and "text document" in low:
        return _queue.pop(0) if _queue else "<|COMPLETE|>"
    if "points" in sys_low and "score" in sys_low:
        return _GLOBAL_POINTS
    if "community" in low or "report" in low or "community" in sys_low:
        return _COMMUNITY
    return "No additional information available."


def _build(backend, working_dir, dsn, emb):
    _install_shim()
    from nano_graphrag import GraphRAG

    kwargs = dict(
        working_dir=str(working_dir),
        embedding_func=emb,
        best_model_func=_canned_llm,
        cheap_model_func=_canned_llm,
        enable_naive_rag=True,
    )
    if backend == "pg":
        from ainb_reflect_memory.nanographrag import addon_params, storage_classes

        kwargs.update(storage_classes())
        kwargs["addon_params"] = addon_params(
            pg_dsn=dsn, workspace_id=WS, embedding_model="all-mpnet-base-v2"
        )
    return GraphRAG(**kwargs)


def _seed(g):
    global _queue
    for doc, ents in CORPUS:
        _queue = [ents]
        g.insert(doc)
    _queue = []


def _evidence(ctx):
    if not ctx:
        return frozenset()
    up = ctx.upper()
    return frozenset(e for e in _ENTITIES if e in up)


def test_realmodel_local_vs_pg_parity(clean, tmp_path) -> None:
    """REAL embeddings: local and Postgres backends return the same evidence set."""
    pytest.importorskip(
        "sentence_transformers", reason="real-model tier needs sentence-transformers"
    )
    from nano_graphrag import QueryParam

    emb = _real_embedding()  # one model instance, shared by both backends
    local = _build("local", tmp_path / "local", None, emb)
    _seed(local)
    pg = _build("pg", tmp_path / "pg", clean, emb)
    _seed(pg)

    mismatches = []
    for q in QUERIES:
        for mode in MODES:
            ls = _evidence(local.query(q, QueryParam(mode=mode, only_need_context=True)))
            ps = _evidence(pg.query(q, QueryParam(mode=mode, only_need_context=True)))
            if ls != ps:
                mismatches.append((q, mode, sorted(ls), sorted(ps)))
    assert not mismatches, "real-model LOCAL vs PG evidence differs:\n" + "\n".join(
        f"  q={q!r} mode={m}: local={lo} pg={pg_}" for q, m, lo, pg_ in mismatches
    )
