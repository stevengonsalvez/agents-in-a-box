"""Minimal graspologic compatibility shim for nano-graphrag.

Provides the 3 functions nano-graphrag uses from graspologic, implemented
with pure networkx + community detection. This avoids the broken transitive
dependency chain: graspologic -> hyppo -> numba -> llvmlite.

Functions shimmed:
  graspologic.utils.largest_connected_component
  graspologic.partition.hierarchical_leiden
  graspologic.embed.node2vec_embed (stub - not used in our flow)
"""

from dataclasses import dataclass
from typing import List

import networkx as nx


# --- graspologic.utils ---

def largest_connected_component(graph: nx.Graph) -> nx.Graph:
    """Return the largest connected component of the graph."""
    if graph.number_of_nodes() == 0:
        return graph

    if graph.is_directed():
        components = nx.weakly_connected_components(graph)
    else:
        components = nx.connected_components(graph)

    largest = max(components, key=len)
    return graph.subgraph(largest).copy()


# --- graspologic.partition ---

@dataclass
class HierarchicalCluster:
    """Mimics graspologic's HierarchicalClusters named tuple."""
    node: str
    cluster: int
    level: int


def hierarchical_leiden(
    graph: nx.Graph,
    max_cluster_size: int = 10,
    random_seed: int = 0xDEADBEEF,
) -> List[HierarchicalCluster]:
    """Community detection using networkx's Louvain as a Leiden substitute.

    Returns a list of HierarchicalCluster objects compatible with
    nano-graphrag's expected format. Uses a single level since
    nx.community.louvain_communities doesn't produce hierarchies natively.
    """
    if graph.number_of_nodes() == 0:
        return []

    # Use Louvain community detection (available in networkx >= 3.0)
    communities = nx.community.louvain_communities(
        graph,
        seed=random_seed,
        resolution=1.0,
    )

    results = []
    for cluster_id, community in enumerate(communities):
        for node in community:
            results.append(HierarchicalCluster(
                node=node,
                cluster=cluster_id,
                level=0,
            ))

    return results


# --- graspologic.embed (stub) ---

def node2vec_embed(graph, **kwargs):
    """Stub for node2vec embedding - not used in our search flow."""
    raise NotImplementedError(
        "node2vec_embed not available in graspologic shim. "
        "Install full graspologic if needed."
    )


def install_shim():
    """Install the shim into sys.modules so nano-graphrag finds it.

    Call this BEFORE importing nano_graphrag.
    """
    import types
    import sys

    # Create fake graspologic module hierarchy
    graspologic = types.ModuleType("graspologic")
    graspologic.__path__ = []

    graspologic_utils = types.ModuleType("graspologic.utils")
    graspologic_utils.largest_connected_component = largest_connected_component

    graspologic_partition = types.ModuleType("graspologic.partition")
    graspologic_partition.hierarchical_leiden = hierarchical_leiden

    graspologic_embed = types.ModuleType("graspologic.embed")
    graspologic_embed.node2vec_embed = node2vec_embed

    graspologic.utils = graspologic_utils
    graspologic.partition = graspologic_partition
    graspologic.embed = graspologic_embed

    sys.modules["graspologic"] = graspologic
    sys.modules["graspologic.utils"] = graspologic_utils
    sys.modules["graspologic.partition"] = graspologic_partition
    sys.modules["graspologic.embed"] = graspologic_embed
