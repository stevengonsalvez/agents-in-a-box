"""Typed link recovery from the stored GraphRAG graph (S2).

nano-graphrag's relationship tuple format has no type slot — only the
edge description survives into ``graph_chunk_entity_relation.graphml``.
``entity_store.Relationship.typed_description`` embeds the sidecar's
``type`` as a ``[type]`` prefix on the description at index time; this
module recovers those types from the stored graph so the graph-expansion
arm (R1) can filter edges by link type ("what enabled this fix?" /
"what does this rule prevent?").

Stdlib-only on purpose: the GraphML is plain XML, and parsing it with
``xml.etree`` keeps typed-link queries working on slim builds where
networkx / nano-graphrag are not installed.
"""

from __future__ import annotations

import re
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Dict, Iterable, List, Optional, Set

from reflect_kb.cli.entity_store import RELATIONSHIP_TYPES

# nano-graphrag joins merged edge descriptions with this separator; each
# segment may carry its own [type] prefix.
GRAPH_FIELD_SEP = "<SEP>"

# nano-graphrag's clean_str does NOT strip the LLM tuple quotes, so edge
# descriptions persist quote-wrapped in the GraphML (e.g.
# '"[caused_by] pool waits surface as 504s"') — tolerate a leading '"'
# per segment, same as _strip_quotes does for node ids.
_LINK_TYPE_RE = re.compile(r'^\s*"?\s*\[([a-z_]+)\]')

# nano-graphrag's on-disk graph cache filename.
GRAPHML_FILENAME = "graph_chunk_entity_relation.graphml"


def parse_link_types(description: str) -> List[str]:
    """Extract typed link types from a (possibly merged) edge description.

    Each ``<SEP>``-separated segment is checked for a leading ``[type]``
    prefix (optionally quote-wrapped, as nano-graphrag persists it); only
    members of the closed RELATIONSHIP_TYPES enum count.
    Returns ordered, de-duplicated types. Untyped descriptions yield [].
    """
    if not description:
        return []
    found: List[str] = []
    for segment in description.split(GRAPH_FIELD_SEP):
        m = _LINK_TYPE_RE.match(segment)
        if m and m.group(1) in RELATIONSHIP_TYPES and m.group(1) not in found:
            found.append(m.group(1))
    return found


def _local_name(tag: str) -> str:
    """Strip the XML namespace from a tag name."""
    return tag.rsplit("}", 1)[-1]


def _strip_quotes(name: str) -> str:
    """nano-graphrag stores node ids as '"UPPER NAME"' — strip the quotes."""
    return name.strip('"')


def read_typed_edges(
    graphml_path: str | Path,
    link_types: Optional[Iterable[str]] = None,
) -> List[Dict]:
    """Read graph edges from a GraphML file, optionally filtered by link type.

    Args:
        graphml_path: path to ``graph_chunk_entity_relation.graphml``.
        link_types: when given, only edges whose parsed types intersect
            this set are returned (the R1 filter-by-type surface). When
            None, every edge is returned with its parsed ``link_types``
            (possibly []).

    Returns:
        List of dicts: {source, target, description, weight, link_types}.
        Missing/corrupt files yield [] — callers degrade gracefully.
    """
    path = Path(graphml_path)
    if not path.exists():
        return []
    try:
        root = ET.parse(str(path)).getroot()
    except ET.ParseError:
        return []

    wanted: Optional[Set[str]] = set(link_types) if link_types is not None else None

    # Map <key id=...> -> attr.name for edge data lookups.
    key_names: Dict[str, str] = {}
    for el in root.iter():
        if _local_name(el.tag) == "key" and el.get("for") == "edge":
            key_names[el.get("id", "")] = el.get("attr.name", "")

    edges: List[Dict] = []
    for el in root.iter():
        if _local_name(el.tag) != "edge":
            continue
        description = ""
        weight = 1.0
        for data in el:
            if _local_name(data.tag) != "data":
                continue
            attr = key_names.get(data.get("key", ""), "")
            if attr == "description":
                description = data.text or ""
            elif attr == "weight":
                try:
                    weight = float(data.text or 1.0)
                except (TypeError, ValueError):
                    weight = 1.0
        types = parse_link_types(description)
        if wanted is not None and not wanted.intersection(types):
            continue
        edges.append({
            "source": _strip_quotes(el.get("source", "")),
            "target": _strip_quotes(el.get("target", "")),
            "description": description,
            "weight": weight,
            "link_types": types,
        })
    return edges


def get_typed_edges(
    cache_dir: str | Path,
    link_types: Optional[Iterable[str]] = None,
) -> List[Dict]:
    """Read typed edges from a nano-graphrag cache directory.

    Joins ``cache_dir`` with the standard GraphML filename and delegates
    to :func:`read_typed_edges`. This is the R1 filter-by-type surface;
    ``LearningsGraphEngine.get_typed_edges`` delegates here so the path
    stays exercisable on slim builds (no networkx required).
    """
    return read_typed_edges(Path(cache_dir) / GRAPHML_FILENAME, link_types=link_types)
