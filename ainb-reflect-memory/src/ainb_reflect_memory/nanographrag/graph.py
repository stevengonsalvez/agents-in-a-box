"""PgGraphStorage — nano-graphrag ``BaseGraphStorage`` backed by Postgres.

Subclasses ``NetworkXStorage`` and overrides ONLY persistence:

  * ``__post_init__`` loads the graph from ``ng_graph_nodes`` / ``ng_graph_edges``
    into an in-memory ``nx.Graph`` (instead of reading a local ``.graphml``).
  * ``index_done_callback`` upserts the in-memory graph back to Postgres
    (instead of writing ``.graphml``).

Everything else — ``upsert_node`` / ``upsert_edge`` / ``get_node`` /
``node_degree`` / ``get_node_edges`` / ``clustering`` (Leiden) /
``community_schema`` — is inherited UNCHANGED from ``NetworkXStorage``. The
Leiden clustering therefore still runs CLIENT-SIDE (graspologic, in process);
only the load/save target moves to the shared database, so the same graph and
communities are visible from every machine. No ``.graphml`` file is written.
"""

from __future__ import annotations

import networkx as nx
from nano_graphrag._storage.gdb_networkx import NetworkXStorage

from ._conn import PgBackend, resolve_config

__all__ = ["PgGraphStorage"]

_NODES = "reflect_memory.ng_graph_nodes"
_EDGES = "reflect_memory.ng_graph_edges"


class PgGraphStorage(NetworkXStorage):
    def __post_init__(self) -> None:
        dsn, workspace_id, _model = resolve_config(self.global_config)
        self._pg = PgBackend.shared(dsn, workspace_id)
        self._ws = workspace_id
        # Load the shared graph from Postgres (not from a local .graphml file).
        self._graph = self._load_graph_from_pg()
        # Same algorithm registries NetworkXStorage sets up — so the inherited
        # clustering / node-embedding methods work verbatim.
        self._clustering_algorithms = {"leiden": self._leiden_clustering}
        self._node_embed_algorithms = {"node2vec": self._node2vec_embed}

    def _load_graph_from_pg(self) -> nx.Graph:
        g = nx.Graph()
        for r in self._pg.fetchall(
            f"select node_id, attrs from {_NODES} where workspace_id=%s and namespace=%s",
            (self._ws, self.namespace),
        ):
            g.add_node(r["node_id"], **(r["attrs"] or {}))
        for r in self._pg.fetchall(
            f"select source, target, attrs from {_EDGES} where workspace_id=%s and namespace=%s",
            (self._ws, self.namespace),
        ):
            g.add_edge(r["source"], r["target"], **(r["attrs"] or {}))
        return g

    async def index_done_callback(self) -> None:
        self._save_graph_to_pg()

    def _save_graph_to_pg(self) -> None:
        from psycopg.types.json import Jsonb

        node_rows = [
            (self._ws, self.namespace, node_id, Jsonb(dict(data)))
            for node_id, data in self._graph.nodes(data=True)
        ]
        self._pg.executemany(
            f"insert into {_NODES} (workspace_id, namespace, node_id, attrs) "
            "values (%s,%s,%s,%s) "
            "on conflict (workspace_id, namespace, node_id) "
            "do update set attrs = excluded.attrs, updated_at = now()",
            node_rows,
        )

        # Canonicalize undirected edges (source <= target) so one logical edge
        # is exactly one row regardless of insertion direction.
        edge_rows = []
        for s, t, data in self._graph.edges(data=True):
            src, tgt = (s, t) if s <= t else (t, s)
            edge_rows.append((self._ws, self.namespace, src, tgt, Jsonb(dict(data))))
        self._pg.executemany(
            f"insert into {_EDGES} (workspace_id, namespace, source, target, attrs) "
            "values (%s,%s,%s,%s,%s) "
            "on conflict (workspace_id, namespace, source, target) "
            "do update set attrs = excluded.attrs, updated_at = now()",
            edge_rows,
        )
