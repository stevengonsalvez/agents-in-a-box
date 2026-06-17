# ABOUTME: Behavioral proof for C3 — graphml_repair --maintain runs the 3-pass
# ABOUTME: post-delete sweep (orphan-entity prune + stale cooccurrence-edge prune
# ABOUTME: + relink of nodes that lost neighbours), driving the REAL graphml_repair
# ABOUTME: module on a hand-built graphml with KNOWN orphans/stale edges. No LLM.
"""C3 graph-maintenance post-delete sweep proof.

Port C3 is a CONSOLIDATION port (surface=consolidation). It extends
``plugins/reflect/scripts/graphml_repair.py`` with a ``--maintain`` mode and
wires it into the drain hook to run once per N drains. The sweep is a pure
*structural* rewrite of the local GraphRAG graphml — no embedding model, no LLM,
no network — so it is driven here directly via the REAL module (no mock of the
thing under test). The retrieval/ranking surface is untouched (this runs at
capture/consolidation time, not query time), so the behavioral_kb retrieval
fixture is the wrong surface and is deliberately not used. All state is isolated
to a tmp dir; the only inputs are a graphml string and the module, so the seed
fully determines the verdict — there is nothing for an LLM to decide.

The graphml schema mirrors nano-graphrag's on-disk cache (see
``reflect-kb/tests/test_typed_links_engine.py``): node ids are quote-wrapped,
nodes carry an ``entity_type`` (d0) and a ``source_id`` (learning-ref chunk ids,
joined with ``<SEP>``) data attr, edges carry a ``weight``. A learning being
deleted/superseded drops its chunk id out of those source_id lists, leaving:
  * orphan entities — a node with no remaining learning refs AND no edges;
  * stale cooccurrence edges — an edge pointing at a node that no longer exists;
  * nodes that lost their only neighbour once a stale edge was pruned but still
    carry a learning ref (must be relinked to a co-referencing node).

Invariants (each arm's graphml + the module fully determine the verdict — no LLM):

  A. ORPHAN-ENTITY COUNT GOES TO 0. A graph with a KNOWN orphan entity (a node
     whose source_id is empty and which has no edges) has zero orphan entities
     after ``maintain`` — the acceptance criterion. The non-orphan nodes survive.

  B. STALE COOCCURRENCE EDGE IS PRUNED. An edge whose endpoint references a node
     that does not exist in the graph (a dangling endpoint left after the entity
     was deleted) is removed; live edges are kept. This is decisive: if stale
     edges survived, the dangling endpoint would still be reachable.

  C. NODE THAT LOST NEIGHBOURS IS RELINKED. A node that still carries a learning
     ref but became isolated after the stale-edge prune is relinked to a node it
     co-occurs with (shares a source_id chunk) — its cooccurrence signal is
     topped back up rather than stranded. The new edge connects two real,
     surviving, co-referencing nodes (not an arbitrary pair).

  D. IDEMPOTENT ON A CLEAN GRAPH. Running ``maintain`` on an already-clean graph
     (no orphans, no dangling edges, no isolated-with-refs nodes) reports a
     pure no-op (all-zero stats) and leaves the file byte-for-byte unchanged.
     This is the falsifiable arm: a sweep that mutated indiscriminately would
     change the clean file and the byte-equality assertion would FAIL.

  E. RESULT STILL VALIDATES. After the mutate+rewrite the graphml still parses
     (``is_valid`` is True) — the sweep never ships a broken file.

Falsifiability: if the orphan survived, arm A fails. If the stale edge survived,
arm B's count is non-zero and fails. If the isolated node were left stranded,
arm C finds no relink edge and FAILS. If the sweep were not idempotent, arm D's
byte-equality assertion FAILS. The maintain pass is exercised through the real
module's public ``maintain`` / ``maintain_tree`` entry points.

PORT: C3
"""
from __future__ import annotations

import sys
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

# Import the REAL graphml_repair module from the reflect plugin so we exercise
# the shipped --maintain sweep, not a copy. Path resolution mirrors
# proof_C4_lifecycle_events.py: parents[3] is the repo root where plugins/ sits
# alongside reflect-kb/; the fallback handles a reflect-kb-as-root checkout.
_BEHAVIORAL_DIR = Path(__file__).resolve().parents[1]
_PLUGIN_CANDIDATES = [
    _BEHAVIORAL_DIR.parents[3] / "plugins" / "reflect" / "scripts",
    _BEHAVIORAL_DIR.parents[2].parent / "plugins" / "reflect" / "scripts",
]
_PLUGIN_SCRIPTS = next(
    (p for p in _PLUGIN_CANDIDATES if p.exists()), _PLUGIN_CANDIDATES[0]
)
if str(_PLUGIN_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_PLUGIN_SCRIPTS))

import graphml_repair as G  # noqa: E402


GRAPHML_FILENAME = "graph_chunk_entity_relation.graphml"

# A small real graphml. ALPHA/BETA/GAMMA all reference learning chunk "L-1".
# ALPHA—BETA is a live edge. GAMMA—DELETED is a STALE cooccurrence edge (its
# target DELETED was removed when that learning was superseded — no such node).
# ORPHAN has NO learning refs and NO edges (a known orphan entity). After the
# stale-edge prune GAMMA loses its only neighbour but still references L-1, so it
# must be relinked to a co-referencing survivor (ALPHA or BETA).
_DIRTY = """<?xml version='1.0' encoding='utf-8'?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="d3" for="edge" attr.name="weight" attr.type="double" />
  <key id="d1" for="node" attr.name="source_id" attr.type="string" />
  <key id="d0" for="node" attr.name="entity_type" attr.type="string" />
  <graph edgedefault="undirected">
    <node id="&quot;ALPHA&quot;"><data key="d0">"function"</data><data key="d1">L-1</data></node>
    <node id="&quot;BETA&quot;"><data key="d0">"error"</data><data key="d1">L-1</data></node>
    <node id="&quot;GAMMA&quot;"><data key="d0">"technology"</data><data key="d1">L-1</data></node>
    <node id="&quot;ORPHAN&quot;"><data key="d0">"technology"</data><data key="d1"></data></node>
    <edge source="&quot;ALPHA&quot;" target="&quot;BETA&quot;"><data key="d3">5.0</data></edge>
    <edge source="&quot;GAMMA&quot;" target="&quot;DELETED&quot;"><data key="d3">3.0</data></edge>
  </graph>
</graphml>
"""

# A clean graph: every node carries a learning ref, every edge connects two live
# nodes, no node with refs is isolated. The sweep must leave this untouched.
_CLEAN = """<?xml version='1.0' encoding='utf-8'?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="d3" for="edge" attr.name="weight" attr.type="double" />
  <key id="d1" for="node" attr.name="source_id" attr.type="string" />
  <key id="d0" for="node" attr.name="entity_type" attr.type="string" />
  <graph edgedefault="undirected">
    <node id="&quot;ALPHA&quot;"><data key="d0">"function"</data><data key="d1">L-1</data></node>
    <node id="&quot;BETA&quot;"><data key="d0">"error"</data><data key="d1">L-1</data></node>
    <edge source="&quot;ALPHA&quot;" target="&quot;BETA&quot;"><data key="d3">5.0</data></edge>
  </graph>
</graphml>
"""


# --- helpers -------------------------------------------------------------

def _write(tmp_path: Path, text: str) -> Path:
    p = tmp_path / GRAPHML_FILENAME
    p.write_text(text, encoding="utf-8")
    return p


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def _nodes(path: Path) -> list[str]:
    root = ET.parse(str(path)).getroot()
    return [n.get("id") for n in root.iter() if _local(n.tag) == "node"]


def _edges(path: Path) -> list[tuple[str, str]]:
    root = ET.parse(str(path)).getroot()
    return [(e.get("source"), e.get("target"))
            for e in root.iter() if _local(e.tag) == "edge"]


def _ids_present(path: Path) -> set[str]:
    return set(_nodes(path))


def _orphan_count(path: Path) -> int:
    """Count orphan entities: nodes with empty source_id AND no incident edge."""
    root = ET.parse(str(path)).getroot()
    src_key = G._node_key_for(root, "source_id")
    nodes = [n for n in root.iter() if _local(n.tag) == "node"]
    edges = [e for e in root.iter() if _local(e.tag) == "edge"]
    touched: set[str] = set()
    for e in edges:
        touched.add(e.get("source"))
        touched.add(e.get("target"))
    n = 0
    for node in nodes:
        nid = node.get("id")
        if not G._learning_refs(node, src_key) and nid not in touched:
            n += 1
    return n


# --- arm A/B/C: dirty graph is swept clean -------------------------------

def test_maintain_drops_orphans_prunes_stale_relinks(tmp_path):
    """The acceptance criterion end-to-end: orphan-entity count -> 0, the stale
    cooccurrence edge is pruned, and the node that lost its neighbour is relinked
    to a co-referencing survivor — while live structure is preserved."""
    p = _write(tmp_path, _DIRTY)

    # Sanity on the seed: it really does contain a known orphan + a stale edge.
    assert _orphan_count(p) == 1, "fixture must seed exactly one orphan entity"
    assert ('"GAMMA"', '"DELETED"') in _edges(p)

    assert G.maintain(p, quiet=True) is True

    # arm A: orphan-entity count goes to 0 (acceptance).
    assert _orphan_count(p) == 0
    # ORPHAN is gone; the referenced entities survive.
    ids = _ids_present(p)
    assert '"ORPHAN"' not in ids
    assert {'"ALPHA"', '"BETA"', '"GAMMA"'} <= ids

    edges = _edges(p)
    # arm B: the stale cooccurrence edge (dangling DELETED endpoint) is pruned;
    # no surviving edge references a non-existent node.
    assert ('"GAMMA"', '"DELETED"') not in edges
    assert '"DELETED"' not in ids
    for s, t in edges:
        assert s in ids and t in ids, (s, t)
    # The original live edge is untouched.
    assert ('"ALPHA"', '"BETA"') in edges

    # arm C: GAMMA lost its only neighbour but kept its L-1 ref, so it is
    # relinked to a node it co-occurs with (ALPHA or BETA, both share L-1).
    gamma_partners = {t for s, t in edges if s == '"GAMMA"'} \
        | {s for s, t in edges if t == '"GAMMA"'}
    assert gamma_partners, "GAMMA was left stranded — not relinked"
    assert gamma_partners <= {'"ALPHA"', '"BETA"'}


def test_maintain_tree_reports_each_pass(tmp_path):
    """Drive the pure tree-rewrite entry point: stats report one orphan pruned,
    one stale edge pruned, and one node relinked — proving all three passes run,
    not just the orphan prune."""
    p = _write(tmp_path, _DIRTY)
    root = ET.parse(str(p)).getroot()
    stats = G.maintain_tree(root)
    assert stats["orphans_pruned"] == 1
    assert stats["edges_pruned"] == 1
    assert stats["nodes_relinked"] == 1


def test_maintain_result_still_validates(tmp_path):
    """arm E: after the mutate + rewrite the graphml still parses — the sweep
    never ships a broken file forward to reindex."""
    p = _write(tmp_path, _DIRTY)
    assert G.maintain(p, quiet=True) is True
    assert G.is_valid(p) is True


# --- arm D: clean graph is left byte-for-byte unchanged (idempotent) -----

def test_maintain_clean_graph_is_byte_identical_noop(tmp_path):
    """A clean graph reports an all-zero no-op and is left byte-for-byte
    unchanged — the decisive idempotency arm: an indiscriminate sweep would
    mutate the clean file and this byte-equality assertion would FAIL."""
    p = _write(tmp_path, _CLEAN)
    before = p.read_bytes()

    # maintain_tree on the clean parse reports a pure no-op.
    root = ET.parse(str(p)).getroot()
    stats = G.maintain_tree(root)
    assert stats == {"orphans_pruned": 0, "edges_pruned": 0, "nodes_relinked": 0}

    # The file-level maintain() must therefore not touch the file at all.
    assert G.maintain(p, quiet=True) is True
    assert p.read_bytes() == before, "clean graph was rewritten by a no-op sweep"


def test_maintain_is_idempotent_on_dirty_then_clean(tmp_path):
    """Running the sweep twice: the first pass cleans the dirty graph, the second
    pass is a pure no-op (the swept graph is now clean and must not change)."""
    p = _write(tmp_path, _DIRTY)

    assert G.maintain(p, quiet=True) is True
    after_first = p.read_bytes()
    nodes_first, edges_first = _nodes(p), _edges(p)

    # Second sweep: now clean -> no-op, byte-identical.
    assert G.maintain(p, quiet=True) is True
    assert p.read_bytes() == after_first
    assert _nodes(p) == nodes_first
    assert _edges(p) == edges_first
    assert _orphan_count(p) == 0


if __name__ == "__main__":  # pragma: no cover
    sys.exit(pytest.main([__file__, "-q"]))
