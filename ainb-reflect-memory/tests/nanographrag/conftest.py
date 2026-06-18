# ABOUTME: Fixtures for the Postgres-backed nano-graphrag adapter tests.
# ABOUTME: Auto-skip unless psycopg + nano_graphrag are importable AND a
# ABOUTME: Postgres (with pgvector) is reachable — never fail for missing deps.

from __future__ import annotations

import hashlib
import os
import pathlib

import pytest

_MIGRATIONS = pathlib.Path(__file__).resolve().parents[2] / "supabase" / "migrations"
_M1 = _MIGRATIONS / "0001_reflect_memory_phase1.sql"
_M2 = _MIGRATIONS / "0002_nanographrag_pgvector.sql"

WS_A = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
WS_B = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"


def _dsn() -> str | None:
    return os.environ.get("DATABASE_URL") or os.environ.get("REFLECT_TEST_DATABASE_URL")


@pytest.fixture(scope="session")
def pg_dsn() -> str:
    dsn = _dsn()
    if not dsn:
        pytest.skip("no DATABASE_URL — nano-graphrag PG adapter tests skipped")
    pytest.importorskip("psycopg", reason="psycopg not installed")
    pytest.importorskip("nano_graphrag", reason="nano_graphrag not installed")
    pytest.importorskip("networkx")
    pytest.importorskip("numpy")

    import psycopg

    try:
        conn = psycopg.connect(dsn)
    except Exception as exc:  # noqa: BLE001
        pytest.skip(f"Postgres not reachable ({exc})")
    try:
        conn.autocommit = True
        with conn.cursor() as cur:
            try:
                cur.execute(_M1.read_text())
                cur.execute(_M2.read_text())
            except Exception as exc:  # noqa: BLE001 — e.g. pgvector missing
                pytest.skip(f"migrations did not apply ({exc})")
    finally:
        conn.close()
    return dsn


@pytest.fixture
def clean(pg_dsn):
    """Truncate all reflect_memory tables before each test."""
    import psycopg

    c = psycopg.connect(pg_dsn, autocommit=True)
    with c.cursor() as cur:
        cur.execute(
            "truncate reflect_memory.ng_kv, reflect_memory.ng_graph_nodes, "
            "reflect_memory.ng_graph_edges, reflect_memory.ng_vectors, "
            "reflect_memory.memory_items, reflect_memory.entities, "
            "reflect_memory.edges cascade;"
        )
    c.close()
    return pg_dsn


@pytest.fixture
def fake_embedding():
    """A deterministic 768-d unit-vector embedding func (no model, no network).

    Same text -> same vector, so a query for an inserted entity's content is its
    own nearest neighbour. Shaped exactly like nano-graphrag expects: async,
    returns an np.ndarray, carries an ``embedding_dim`` attribute.
    """
    import numpy as np

    def _vec(text: str) -> "np.ndarray":
        seed = int.from_bytes(hashlib.sha256(text.encode()).digest()[:8], "little")
        rng = np.random.default_rng(seed)
        v = rng.standard_normal(768)
        return v / np.linalg.norm(v)

    async def embedding_func(texts):
        return np.array([_vec(t) for t in texts])

    embedding_func.embedding_dim = 768
    return embedding_func


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
