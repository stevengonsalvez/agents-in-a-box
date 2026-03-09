"""Shared helpers for E2E tests."""

import yaml


def make_learning_doc(
    title: str,
    category: str = "debugging-sessions",
    key_insight: str = "Test insight",
    confidence: str = "high",
    tags: list = None,
    symptoms: list = None,
    body: str = "## Problem\nTest problem.\n\n## Solution\nTest solution.\n",
) -> str:
    """Create a learning document string with frontmatter."""
    tags = tags or ["test"]
    symptoms = symptoms or ["test symptom"]
    tag_str = "[" + ", ".join(tags) + "]"
    symptom_lines = "\n".join(f"  - {s}" for s in symptoms)
    return (
        f"---\n"
        f"title: {title}\n"
        f"category: {category}\n"
        f"key_insight: {key_insight}\n"
        f"confidence: {confidence}\n"
        f"tags: {tag_str}\n"
        f"symptoms:\n{symptom_lines}\n"
        f"---\n\n{body}"
    )


def make_entity_sidecar(
    document_id: str,
    entities: list = None,
    relationships: list = None,
) -> str:
    """Create an entity sidecar YAML string."""
    entities = entities or [
        {"name": "TestEntity", "type": "technology", "description": "A test entity"},
    ]
    relationships = relationships or []

    data = {
        "document_id": document_id,
        "extracted_at": "2025-01-01T00:00:00",
        "entities": entities,
        "relationships": relationships,
    }
    return yaml.dump(data, default_flow_style=False)
