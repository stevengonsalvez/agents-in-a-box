"""Entity sidecar management for Global Learnings GraphRAG.

Handles pre-extracted entity/relationship data stored alongside learning
documents as .entities.yaml sidecar files. Converts to nano-graphrag's
expected extraction format for the passthrough LLM approach.
"""

from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import List, Optional

import yaml

ENTITY_TYPES = {"technology", "error", "pattern", "function", "concept", "tool"}
RELATIONSHIP_TYPES = {"caused_by", "solves", "requires", "relates_to"}

TUPLE_DELIMITER = "<|>"
RECORD_DELIMITER = "##"
COMPLETION_DELIMITER = "<|COMPLETE|>"


@dataclass
class Entity:
    name: str
    type: str
    description: str

    def to_graphrag_tuple(self) -> str:
        return (
            f'("entity"{TUPLE_DELIMITER}"{self.name}"'
            f'{TUPLE_DELIMITER}"{self.type}"'
            f'{TUPLE_DELIMITER}"{self.description}")'
        )


@dataclass
class Relationship:
    source: str
    target: str
    type: str
    description: str
    strength: int = 5

    def to_graphrag_tuple(self) -> str:
        return (
            f'("relationship"{TUPLE_DELIMITER}"{self.source}"'
            f'{TUPLE_DELIMITER}"{self.target}"'
            f'{TUPLE_DELIMITER}"{self.description}"'
            f"{TUPLE_DELIMITER}{self.strength})"
        )


@dataclass
class DocumentEntities:
    document_id: str
    extracted_at: str = field(default_factory=lambda: datetime.now().isoformat())
    entities: List[Entity] = field(default_factory=list)
    relationships: List[Relationship] = field(default_factory=list)

    def to_graphrag_format(self) -> str:
        """Convert to nano-graphrag extraction output format.

        Returns the format expected by nano-graphrag's entity extraction parser:
        ("entity"<|>"name"<|>"type"<|>"description")
        ##
        ("relationship"<|>"source"<|>"target"<|>"description"<|>strength)
        <|COMPLETE|>
        """
        parts = []
        for entity in self.entities:
            parts.append(entity.to_graphrag_tuple())
        for rel in self.relationships:
            parts.append(rel.to_graphrag_tuple())
        if not parts:
            return COMPLETION_DELIMITER
        return f"\n{RECORD_DELIMITER}\n".join(parts) + f"\n{COMPLETION_DELIMITER}"

    def to_yaml(self) -> str:
        data = {
            "document_id": self.document_id,
            "extracted_at": self.extracted_at,
            "entities": [
                {"name": e.name, "type": e.type, "description": e.description}
                for e in self.entities
            ],
            "relationships": [
                {
                    "source": r.source,
                    "target": r.target,
                    "type": r.type,
                    "description": r.description,
                    "strength": r.strength,
                }
                for r in self.relationships
            ],
        }
        return yaml.dump(data, default_flow_style=False, allow_unicode=True)

    @classmethod
    def from_yaml(cls, yaml_str: str) -> "DocumentEntities":
        data = yaml.safe_load(yaml_str)
        entities = [
            Entity(name=e["name"], type=e["type"], description=e["description"])
            for e in data.get("entities", [])
        ]
        relationships = [
            Relationship(
                source=r["source"],
                target=r["target"],
                type=r["type"],
                description=r["description"],
                strength=r.get("strength", 5),
            )
            for r in data.get("relationships", [])
        ]
        return cls(
            document_id=data.get("document_id", ""),
            extracted_at=data.get("extracted_at", ""),
            entities=entities,
            relationships=relationships,
        )

    @classmethod
    def from_yaml_file(cls, path: Path) -> "DocumentEntities":
        return cls.from_yaml(path.read_text())

    @property
    def entity_count(self) -> int:
        return len(self.entities)

    @property
    def relationship_count(self) -> int:
        return len(self.relationships)


def find_sidecar(doc_path: Path) -> Optional[Path]:
    """Find the .entities.yaml sidecar file alongside a document.

    Looks for:
      doc.md -> doc.entities.yaml
      doc.md -> doc.md.entities.yaml
    """
    sidecar = doc_path.with_suffix(".entities.yaml")
    if sidecar.exists():
        return sidecar

    sidecar = doc_path.parent / f"{doc_path.name}.entities.yaml"
    if sidecar.exists():
        return sidecar

    return None
