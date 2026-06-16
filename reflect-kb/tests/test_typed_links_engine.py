# ABOUTME: Regression tests for port S2 (engine side) — typed causal links.
# ABOUTME: Pins the [type]-prefixed edge descriptions in the graphrag tuple and
# ABOUTME: the stdlib GraphML typed-edge reader the R1 graph arm filters with.
"""Port S2: typed causal links — engine-side acceptance.

Pins the "graph-expansion arm (R1) can filter by type" bullet:
  * entity_store.RELATIONSHIP_TYPES carries the typed causal enum
  * Relationship.to_graphrag_tuple embeds the link type as a `[type]`
    description prefix (nano-graphrag's tuple has no type slot — only the
    description survives onto the GraphML edge)
  * graph_links.parse_link_types recovers types from (merged) descriptions
  * graph_links.read_typed_edges / get_typed_edges filter stored GraphML
    edges by link type (stdlib XML — works on slim builds without networkx)

IMPORTANT fixture shape: nano-graphrag's ``clean_str`` does NOT strip the
LLM tuple quotes, so edge descriptions persist QUOTE-WRAPPED in the real
GraphML (e.g. ``"[caused_by] pool waits surface as 504s"``) — exactly like
node ids. The fixtures below mirror that on-disk shape; an earlier version
of this suite used unquoted descriptions and let a parse regression slip
through against real indexed KBs.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from reflect_kb.cli.entity_store import (
    RELATIONSHIP_TYPES,
    TYPED_CAUSAL_LINK_TYPES,
    DocumentEntities,
    Relationship,
)
from reflect_kb.cli.graph_links import (
    GRAPHML_FILENAME,
    get_typed_edges,
    parse_link_types,
    read_typed_edges,
)

BEAD_ENUM = {
    "caused_by", "causes", "enables", "prevents",
    "contradicts", "supersedes", "part_of", "uses",
}

# Mirrors real nano-graphrag output: node ids AND edge descriptions are
# stored quote-wrapped ('"..."'); merged edges join quote-wrapped segments
# with <SEP>.
GRAPHML = """<?xml version='1.0' encoding='utf-8'?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns">
  <key id="d6" for="edge" attr.name="order" attr.type="long" />
  <key id="d5" for="edge" attr.name="source_id" attr.type="string" />
  <key id="d4" for="edge" attr.name="description" attr.type="string" />
  <key id="d3" for="edge" attr.name="weight" attr.type="double" />
  <key id="d0" for="node" attr.name="entity_type" attr.type="string" />
  <graph edgedefault="undirected">
    <node id="&quot;BLOCK_ON&quot;"><data key="d0">"function"</data></node>
    <node id="&quot;PANIC&quot;"><data key="d0">"error"</data></node>
    <node id="&quot;TOKIO&quot;"><data key="d0">"technology"</data></node>
    <edge source="&quot;BLOCK_ON&quot;" target="&quot;PANIC&quot;">
      <data key="d4">"[caused_by] block_on inside async context panics"</data>
      <data key="d3">9.0</data>
    </edge>
    <edge source="&quot;BLOCK_ON&quot;" target="&quot;TOKIO&quot;">
      <data key="d4">"untyped legacy edge description"</data>
      <data key="d3">5.0</data>
    </edge>
    <edge source="&quot;TOKIO&quot;" target="&quot;PANIC&quot;">
      <data key="d4">"[enables] async runtime"&lt;SEP&gt;"[prevents] panic when used right"</data>
      <data key="d3">7.0</data>
    </edge>
  </graph>
</graphml>
"""


# ---------------------------------------------------------------------------
# Enum + typed description prefix
# ---------------------------------------------------------------------------

def test_engine_enum_carries_typed_causal_links():
    assert BEAD_ENUM == TYPED_CAUSAL_LINK_TYPES
    assert BEAD_ENUM <= RELATIONSHIP_TYPES
    # Legacy types stay valid — existing sidecars keep indexing.
    assert {"solves", "requires", "relates_to"} <= RELATIONSHIP_TYPES


@pytest.mark.parametrize("rel_type", sorted(BEAD_ENUM))
def test_graphrag_tuple_embeds_link_type(rel_type):
    rel = Relationship(source="a", target="b", type=rel_type, description="why")
    assert f"[{rel_type}] why" in rel.to_graphrag_tuple()


def test_typed_description_is_idempotent():
    rel = Relationship(source="a", target="b", type="causes",
                       description="[causes] already prefixed")
    assert rel.typed_description() == "[causes] already prefixed"


def test_unknown_type_gets_no_prefix():
    rel = Relationship(source="a", target="b", type="vibes_with", description="why")
    assert rel.typed_description() == "why"
    assert "[vibes_with]" not in rel.to_graphrag_tuple()


def test_from_yaml_defaults_missing_type_to_relates_to():
    doc = DocumentEntities.from_yaml(
        "relationships:\n  - source: a\n    target: b\n    description: d\n"
    )
    assert doc.relationships[0].type == "relates_to"


# ---------------------------------------------------------------------------
# parse_link_types — recovering types from (merged) edge descriptions
# ---------------------------------------------------------------------------

def test_parse_link_types_quoted_nano_graphrag_shape():
    # The on-disk shape: clean_str keeps the LLM tuple quotes, so the
    # stored description is quote-wrapped. THIS is what real KBs contain.
    assert parse_link_types('"[caused_by] pool waits surface as 504s"') == ["caused_by"]


def test_parse_link_types_unquoted_still_supported():
    assert parse_link_types("[caused_by] pool waits surface as 504s") == ["caused_by"]


def test_parse_link_types_merged_quoted_segments():
    merged = '"[caused_by] a"<SEP>"[enables] b"<SEP>"plain old text"<SEP>"[caused_by] dup"'
    assert parse_link_types(merged) == ["caused_by", "enables"]


def test_parse_link_types_ignores_untyped_and_unknown():
    assert parse_link_types("no prefix here") == []
    assert parse_link_types('"no prefix here"') == []
    assert parse_link_types("[not_a_real_type] nope") == []
    assert parse_link_types('"[not_a_real_type] nope"') == []
    assert parse_link_types("") == []


# ---------------------------------------------------------------------------
# Acceptance: graph-expansion arm (R1) can filter by type
# ---------------------------------------------------------------------------

def _write_cache(tmp_path: Path) -> Path:
    graphml = tmp_path / GRAPHML_FILENAME
    graphml.write_text(GRAPHML)
    return graphml


def test_read_typed_edges_filters_by_link_type(tmp_path):
    graphml = _write_cache(tmp_path)

    caused = read_typed_edges(graphml, link_types={"caused_by"})
    assert len(caused) == 1
    assert caused[0]["source"] == "BLOCK_ON"
    assert caused[0]["target"] == "PANIC"
    assert caused[0]["link_types"] == ["caused_by"]
    assert caused[0]["weight"] == 9.0

    # A merged edge matches on ANY of its parsed types.
    prevents = read_typed_edges(graphml, link_types={"prevents"})
    assert len(prevents) == 1
    assert prevents[0]["link_types"] == ["enables", "prevents"]

    # Untyped edges never match a filter.
    assert read_typed_edges(graphml, link_types={"supersedes"}) == []


def test_read_typed_edges_unfiltered_returns_all(tmp_path):
    graphml = _write_cache(tmp_path)
    edges = read_typed_edges(graphml)
    assert len(edges) == 3
    untyped = [e for e in edges if not e["link_types"]]
    assert len(untyped) == 1  # legacy edges surface with empty link_types


def test_read_typed_edges_degrades_gracefully(tmp_path):
    assert read_typed_edges(tmp_path / "missing.graphml") == []
    corrupt = tmp_path / "corrupt.graphml"
    corrupt.write_text("<graphml><graph></graphml>")
    assert read_typed_edges(corrupt) == []


def test_get_typed_edges_joins_cache_dir(tmp_path):
    """The R1 filter surface as the engine calls it: cache dir in, edges out.

    This is the same path-join + delegation LearningsGraphEngine.get_typed_edges
    performs, exercised WITHOUT constructing the engine so slim builds
    (no networkx) still pin the behavior.
    """
    _write_cache(tmp_path)
    edges = get_typed_edges(tmp_path, ["caused_by"])
    assert len(edges) == 1
    assert edges[0]["link_types"] == ["caused_by"]
    # Missing cache dir degrades to [] like read_typed_edges.
    assert get_typed_edges(tmp_path / "nope", ["caused_by"]) == []


def test_graph_engine_exposes_typed_edge_filter(tmp_path):
    """LearningsGraphEngine.get_typed_edges — engine-level smoke test.

    SKIPS on slim builds: graph_engine imports networkx transitively
    (graspologic shim). The path-join + delegation it performs is pinned
    above by test_get_typed_edges_joins_cache_dir, which runs everywhere;
    this test only adds the engine-constructor wiring on full builds.
    """
    pytest.importorskip("networkx")
    from reflect_kb.cli.graph_engine import LearningsGraphEngine

    _write_cache(tmp_path)
    engine = LearningsGraphEngine(tmp_path)
    edges = engine.get_typed_edges(["caused_by"])
    assert len(edges) == 1
    assert edges[0]["link_types"] == ["caused_by"]


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__, "-v"]))
