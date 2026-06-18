# ABOUTME: Backend-parity regression — the SAME corpus + queries through the
# ABOUTME: local-default backend and the Postgres backend must return IDENTICAL
# ABOUTME: evidence for naive/local/global. Proves the PG swap changed nothing.

from __future__ import annotations

import hashlib
import json
import re
import sys
import types
from dataclasses import dataclass

import pytest

pytestmark = pytest.mark.integration

WS = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"

# A small fixed corpus: (doc, canned-extraction). Entities overlap across docs
# so local/global graph modes have something to traverse.
CORPUS = [
    (
        "The auth middleware validates the JWT token on every request before routing.",
        '("entity"<|>"AUTH MIDDLEWARE"<|>"component"<|>"validates tokens")##'
        '("entity"<|>"JWT"<|>"concept"<|>"a signed token")##'
        '("relationship"<|>"AUTH MIDDLEWARE"<|>"JWT"<|>"validates the token"<|>2)<|COMPLETE|>',
    ),
    (
        "The rate limiter uses a token bucket keyed by JWT subject to throttle requests.",
        '("entity"<|>"RATE LIMITER"<|>"component"<|>"throttles requests")##'
        '("entity"<|>"JWT"<|>"concept"<|>"a signed token")##'
        '("relationship"<|>"RATE LIMITER"<|>"JWT"<|>"keys buckets by subject"<|>1)<|COMPLETE|>',
    ),
    (
        "Kubernetes pod autoscaling scales replicas on CPU and request latency.",
        '("entity"<|>"KUBERNETES"<|>"platform"<|>"orchestrates pods")##'
        '("entity"<|>"AUTOSCALER"<|>"component"<|>"scales replicas")##'
        '("relationship"<|>"KUBERNETES"<|>"AUTOSCALER"<|>"runs the autoscaler"<|>1)<|COMPLETE|>',
    ),
]

QUERIES = [
    "auth middleware validates jwt token request",
    "rate limiter token bucket jwt",
    "kubernetes pod autoscaling replicas",
    "jwt token",
]
MODES = ["naive", "local", "global"]

_GLOBAL_POINTS = json.dumps(
    {"points": [{"description": "Auth and rate limiting both rely on the JWT token", "score": 9}]}
)
_COMMUNITY = json.dumps(
    {
        "title": "c",
        "summary": "auth + rate limiter share JWT",
        "findings": [{"summary": "x", "explanation": "y"}],
        "rating": 5.0,
        "rating_explanation": "core",
    }
)


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
        if g.number_of_nodes() == 0:
            return g
        return g.subgraph(max(nx.connected_components(g), key=len)).copy()

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


def _embedding():
    import numpy as np
    from nano_graphrag._utils import wrap_embedding_func_with_attrs

    def vec(text):
        v = np.zeros(768)
        for tok in re.findall(r"[a-z0-9]+", text.lower()):
            h = int.from_bytes(hashlib.sha256(tok.encode()).digest()[:8], "little")
            v[h % 768] += 1.0
        n = np.linalg.norm(v)
        return v / n if n else v

    @wrap_embedding_func_with_attrs(embedding_dim=768, max_token_size=8192)
    async def ef(texts):
        return np.array([vec(t) for t in texts])

    return ef


_queue = []


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


def _build(backend, working_dir, dsn):
    _install_shim()
    from nano_graphrag import GraphRAG

    kwargs = dict(
        working_dir=str(working_dir),
        embedding_func=_embedding(),
        best_model_func=_canned_llm,
        cheap_model_func=_canned_llm,
        enable_naive_rag=True,
    )
    if backend == "pg":
        from ainb_reflect_memory.nanographrag import addon_params, storage_classes

        kwargs.update(storage_classes())
        kwargs["addon_params"] = addon_params(pg_dsn=dsn, workspace_id=WS, embedding_model="parity")
    return GraphRAG(**kwargs)


def _seed(g):
    global _queue
    for doc, ents in CORPUS:
        _queue = [ents]
        g.insert(doc)
    _queue = []


_ENTITY_NAMES = ["AUTH MIDDLEWARE", "JWT", "RATE LIMITER", "KUBERNETES", "AUTOSCALER"]
_DOC_MARKERS = ["auth middleware", "rate limiter", "autoscaling", "token bucket"]


def _evidence_set(ctx):
    """The SET of evidence a context surfaces — entity names + which source docs.

    Compared instead of the raw string because the two ANN engines (local
    NanoVectorDB vs pgvector) break score TIES in different orders. Same
    evidence retrieved, possibly different ordering of equal-scoring items —
    which is fine: reflect re-ranks (RRF/MMR/cross-encoder) downstream. Parity
    that matters = same evidence, not identical tie-break order.
    """
    if not ctx:
        return frozenset()
    up = ctx.upper()
    low = ctx.lower()
    found = {e for e in _ENTITY_NAMES if e in up}
    found |= {m for m in _DOC_MARKERS if m in low}
    return frozenset(found)


def test_local_vs_pg_backend_parity(clean, tmp_path) -> None:
    """Identical corpus + queries → identical EVIDENCE SET on both backends,
    for every mode. (Tie-break order of equal-scoring items may differ between
    the two ANN engines; that is expected and downstream-reranked.)"""
    from nano_graphrag import QueryParam

    local = _build("local", tmp_path / "local", None)
    _seed(local)
    pg = _build("pg", tmp_path / "pg", clean)
    _seed(pg)

    mismatches = []
    nonempty = 0
    for q in QUERIES:
        for mode in MODES:
            ls = _evidence_set(local.query(q, QueryParam(mode=mode, only_need_context=True)))
            ps = _evidence_set(pg.query(q, QueryParam(mode=mode, only_need_context=True)))
            if ls:
                nonempty += 1
            if ls != ps:
                mismatches.append((q, mode, sorted(ls), sorted(ps)))
    assert nonempty >= len(QUERIES), "expected most queries to surface evidence"
    assert not mismatches, "LOCAL vs PG evidence SET differs:\n" + "\n".join(
        f"  q={q!r} mode={m}\n   local={lo}\n   pg   ={pg_}" for q, m, lo, pg_ in mismatches
    )


def test_local_vs_pg_local_files_only_on_local(clean, tmp_path) -> None:
    """The local backend writes graphml/nanovdb; the PG backend writes neither
    for the shared corpus (single source of truth)."""
    local = _build("local", tmp_path / "local2", None)
    _seed(local)
    pg = _build("pg", tmp_path / "pg2", clean)
    _seed(pg)

    local_files = {p.name for p in (tmp_path / "local2").glob("*")}
    pg_files = {p.name for p in (tmp_path / "pg2").glob("*")}
    assert any(f.endswith(".graphml") for f in local_files), (
        f"local should write graphml: {local_files}"
    )
    assert not any(f.endswith(".graphml") for f in pg_files), (
        f"PG must write no graphml: {pg_files}"
    )
