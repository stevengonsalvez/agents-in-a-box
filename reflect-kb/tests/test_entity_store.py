# ABOUTME: Regression tests for DocumentEntities.from_yaml resilience.
# ABOUTME: Guards null/missing YAML fields surfaced by the May 2026 sidecar sweep.

from __future__ import annotations

import pytest

from reflect_kb.cli.entity_store import DocumentEntities


@pytest.mark.parametrize(
    "yaml_str, expected_entities, expected_relationships",
    [
        # `entities:` key with null value used to raise
        # `TypeError: 'NoneType' object is not iterable`.
        pytest.param(
            """
document_id: doc-null-entities
extracted_at: '2026-05-16T00:00:00'
entities:
relationships:
  - source: a
    target: b
    type: relates_to
    description: ok
    strength: 5
""",
            0,
            1,
            id="null_entities_key",
        ),
        # `relationships:` key with null value — same TypeError.
        pytest.param(
            """
document_id: doc-null-rels
extracted_at: '2026-05-16T00:00:00'
entities:
  - name: thing
    type: concept
    description: an entity
relationships:
""",
            1,
            0,
            id="null_relationships_key",
        ),
        # Entity dict missing `description` used to raise
        # `KeyError: 'description'`.
        pytest.param(
            """
document_id: doc-missing-desc
extracted_at: '2026-05-16T00:00:00'
entities:
  - name: orphan
    type: concept
relationships: []
""",
            1,
            0,
            id="entity_missing_description",
        ),
    ],
)
def test_from_yaml_tolerates_partial_sidecars(
    yaml_str: str, expected_entities: int, expected_relationships: int
) -> None:
    """Partial sidecars load and serialize without raising."""
    doc = DocumentEntities.from_yaml(yaml_str)

    assert doc.entity_count == expected_entities
    assert doc.relationship_count == expected_relationships

    # Must serialize to nano-graphrag's expected format without exploding.
    out = doc.to_graphrag_format()
    assert isinstance(out, str)
    assert out.endswith("<|COMPLETE|>")


def test_from_yaml_handles_completely_empty_document() -> None:
    """A wholly empty YAML payload yields an empty-but-valid DocumentEntities."""
    doc = DocumentEntities.from_yaml("")
    assert doc.entity_count == 0
    assert doc.relationship_count == 0
    assert doc.to_graphrag_format() == "<|COMPLETE|>"
