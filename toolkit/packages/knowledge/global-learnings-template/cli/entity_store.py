"""Entity sidecar management for Global Learnings GraphRAG.

Handles pre-extracted entity/relationship data stored alongside learning
documents as .entities.yaml sidecar files. Converts to nano-graphrag's
expected extraction format for the passthrough LLM approach.

Includes heuristic entity extraction for documents without LLM-generated
sidecars, ensuring every document contributes to the knowledge graph.
"""

import re
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional, Set, Tuple

import yaml

ENTITY_TYPES = {"technology", "error", "pattern", "function", "concept", "tool",
                "artifact", "code", "config", "service", "platform", "framework", "library"}
RELATIONSHIP_TYPES = {"caused_by", "solves", "requires", "relates_to", "uses",
                      "implements", "configures", "triggers", "part_of"}

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


# ---------------------------------------------------------------------------
# Heuristic entity extraction (no LLM required)
# ---------------------------------------------------------------------------

# Well-known technology names to boost detection confidence
_KNOWN_TECHNOLOGIES = {
    "react", "next.js", "nextjs", "vue", "angular", "svelte", "remix",
    "node", "nodejs", "deno", "bun", "express", "fastify", "koa",
    "python", "django", "flask", "fastapi", "uvicorn",
    "rust", "cargo", "tokio", "axum", "actix",
    "go", "golang", "gin", "echo",
    "typescript", "javascript", "java", "kotlin", "swift",
    "docker", "kubernetes", "k8s", "terraform", "pulumi",
    "postgres", "postgresql", "mysql", "redis", "mongodb", "sqlite",
    "supabase", "firebase", "aws", "gcp", "azure", "vercel", "netlify",
    "graphql", "grpc", "rest", "websocket",
    "git", "github", "gitlab", "bitbucket",
    "nginx", "caddy", "traefik",
    "playwright", "cypress", "jest", "vitest", "pytest", "mocha",
    "tailwind", "css", "sass", "postcss",
    "prisma", "drizzle", "sqlalchemy", "typeorm", "knex",
    "openai", "anthropic", "claude", "gpt", "llm",
    "tmux", "vim", "neovim", "vscode",
    "posthog", "sentry", "datadog", "grafana",
    "stripe", "auth0", "okta",
    "safari", "chrome", "firefox",
    "ios", "android", "react native", "flutter", "expo",
    "nano-graphrag", "graphrag", "hnswlib",
    "apple mail", "oauth", "pkce", "jwt", "otp",
}

# Patterns that indicate error/bug entities
_ERROR_PATTERNS = re.compile(
    r"(?:error|bug|issue|problem|failure|crash|panic|exception|broken|fix(?:ed)?)\b",
    re.IGNORECASE,
)

# Backtick-quoted terms (code references)
_BACKTICK_RE = re.compile(r"`([^`]{2,60})`")

# Category-to-entity-type mapping
_CATEGORY_TYPE_MAP = {
    "architecture-decisions": "pattern",
    "patterns": "pattern",
    "debugging": "error",
    "bug-fix": "error",
    "session-reflections": "concept",
    "infrastructure": "technology",
    "devops": "technology",
    "configuration": "config",
    "tool-usage": "tool",
    "workflow": "concept",
    "security": "concept",
    "performance": "pattern",
    "testing": "pattern",
}


def _classify_entity_type(name: str, context: str = "") -> str:
    """Classify an entity name into a type using heuristics."""
    name_lower = name.lower().strip()

    if name_lower in _KNOWN_TECHNOLOGIES:
        return "technology"

    # Check for file-like patterns (.tsx, .py, etc.)
    if re.search(r"\.\w{1,4}$", name_lower) and not name_lower.startswith("http"):
        return "code"

    # Check for function-like patterns (camelCase, snake_case with parens)
    if re.search(r"[a-z][A-Z]|_[a-z]", name) and len(name) < 40:
        return "function"

    # Check for config-like patterns (env vars, flags)
    if name_lower.startswith("--") or name.isupper() or name_lower.startswith("env."):
        return "config"

    # Check for service-like patterns
    if any(kw in name_lower for kw in ["api", "service", "server", "worker", "daemon"]):
        return "service"

    # Check for framework/library patterns
    if any(kw in name_lower for kw in ["framework", "library", "sdk", "cli", "plugin"]):
        return "framework"

    # Check if error context
    if _ERROR_PATTERNS.search(context):
        return "error"

    return "concept"


def _extract_backtick_terms(text: str) -> List[str]:
    """Extract meaningful backtick-quoted terms from text."""
    matches = _BACKTICK_RE.findall(text)
    # Filter out common noise (single words that are just formatting)
    filtered = []
    seen = set()
    for m in matches:
        m_clean = m.strip()
        m_lower = m_clean.lower()
        # Skip very common noise terms
        if m_lower in {"true", "false", "null", "none", "undefined", "yes", "no",
                       "string", "int", "bool", "float", "list", "dict", "map",
                       "ok", "err", "todo", "fixme", "note", "warning"}:
            continue
        if m_lower not in seen and len(m_clean) >= 2:
            seen.add(m_lower)
            filtered.append(m_clean)
    return filtered


def _extract_from_frontmatter(frontmatter: Dict) -> Tuple[List[dict], List[dict]]:
    """Extract entity candidates from YAML frontmatter fields."""
    entities = []
    seen_names: Set[str] = set()

    # Title becomes a concept entity
    title = frontmatter.get("title") or frontmatter.get("name", "")
    if title and len(title) > 3:
        # Don't add the full title as entity if it's very long; extract key terms
        if len(title) <= 60:
            entities.append({
                "name": title,
                "type": "concept",
                "description": frontmatter.get("key_insight", frontmatter.get("description", title)),
            })
            seen_names.add(title.lower())

    # Category as context (not entity itself)
    category = frontmatter.get("category", "")

    # Tags become entities
    tags = frontmatter.get("tags", [])
    if isinstance(tags, str):
        tags = [t.strip() for t in tags.split(",")]
    for tag in tags:
        tag = str(tag).strip()
        if tag and tag.lower() not in seen_names and len(tag) >= 2:
            entity_type = "technology" if tag.lower() in _KNOWN_TECHNOLOGIES else _classify_entity_type(tag)
            entities.append({
                "name": tag,
                "type": entity_type,
                "description": f"Tagged in document about {category or 'learning'}",
            })
            seen_names.add(tag.lower())

    return entities, seen_names


def auto_extract_entities(content: str, frontmatter: Dict) -> DocumentEntities:
    """Extract entities and relationships from document content heuristically.

    Uses rule-based extraction from:
    - YAML frontmatter fields (title, category, tags, key_insight)
    - Backtick-quoted code terms in the body
    - Known technology name matching
    - Error/bug pattern detection

    Returns a DocumentEntities with 3-8 entities and 2-6 relationships.
    This is not as accurate as LLM extraction but ensures every document
    contributes something to the knowledge graph.
    """
    fm_entities, seen_names = _extract_from_frontmatter(frontmatter)

    # Extract backtick terms from body
    body_start = content.find("---", 3)
    body = content[body_start + 3:] if body_start > 0 else content
    backtick_terms = _extract_backtick_terms(body)

    # Extract known technology names from body text
    body_lower = body.lower()
    tech_entities = []
    for tech in _KNOWN_TECHNOLOGIES:
        if tech in body_lower and tech not in seen_names:
            # Verify it's a word boundary match (not substring of larger word)
            pattern = r"(?:^|[\s,;:`\"\'\(\[/])(" + re.escape(tech) + r")(?:[\s,;:`\"\'\)\]./]|$)"
            if re.search(pattern, body_lower):
                tech_entities.append({
                    "name": tech.title() if len(tech) > 3 else tech.upper(),
                    "type": "technology",
                    "description": f"Technology referenced in document",
                })
                seen_names.add(tech)

    # Add backtick terms as entities
    code_entities = []
    for term in backtick_terms:
        term_lower = term.lower()
        if term_lower not in seen_names:
            etype = _classify_entity_type(term, body)
            code_entities.append({
                "name": term,
                "type": etype,
                "description": f"Referenced in document",
            })
            seen_names.add(term_lower)

    # Detect error-related entities from lines mentioning errors
    error_entities = []
    for line in body.split("\n"):
        if _ERROR_PATTERNS.search(line) and len(line.strip()) > 10:
            # Extract the key phrase from the error line
            line_clean = line.strip().lstrip("#-*> 0123456789.")
            if len(line_clean) > 10 and line_clean.lower() not in seen_names:
                # Truncate long lines to a reasonable entity name
                name = line_clean[:80].rstrip(" .,;:-")
                if len(name) > 10:
                    error_entities.append({
                        "name": name,
                        "type": "error",
                        "description": "Error or issue mentioned in document",
                    })
                    seen_names.add(line_clean.lower())
                    if len(error_entities) >= 2:
                        break

    # Combine and limit to 3-8 entities (prioritize: frontmatter > tech > code > errors)
    all_entities = fm_entities + tech_entities + code_entities + error_entities
    # Cap at 8
    all_entities = all_entities[:8]
    # Ensure at least 3 if possible (pad with category as concept)
    category = frontmatter.get("category", "")
    if len(all_entities) < 3 and category and category.lower() not in seen_names:
        all_entities.append({
            "name": category,
            "type": _CATEGORY_TYPE_MAP.get(category, "concept"),
            "description": f"Document category",
        })

    # Build Entity objects
    entity_objects = [
        Entity(name=e["name"], type=e["type"], description=e["description"])
        for e in all_entities
    ]

    # Generate relationships (2-6)
    relationships = _generate_relationships(entity_objects, frontmatter, body)

    doc_id = frontmatter.get("title", frontmatter.get("name", "unknown"))
    return DocumentEntities(
        document_id=doc_id,
        entities=entity_objects,
        relationships=relationships,
    )


def _generate_relationships(entities: List[Entity], frontmatter: Dict,
                            body: str) -> List[Relationship]:
    """Generate plausible relationships between extracted entities."""
    if len(entities) < 2:
        return []

    relationships = []
    category = frontmatter.get("category", "")
    seen = set()

    # Strategy 1: First entity (usually title/concept) relates_to all others
    primary = entities[0]
    for secondary in entities[1:]:
        pair_key = (primary.name.lower(), secondary.name.lower())
        if pair_key in seen:
            continue
        seen.add(pair_key)

        rel_type = _infer_relationship_type(primary, secondary, category)
        relationships.append(Relationship(
            source=primary.name,
            target=secondary.name,
            type=rel_type,
            description=f"{primary.name} {rel_type} {secondary.name}",
            strength=5,
        ))
        if len(relationships) >= 4:
            break

    # Strategy 2: Connect technology entities to each other
    tech_entities = [e for e in entities if e.type == "technology"]
    for i, te1 in enumerate(tech_entities):
        for te2 in tech_entities[i + 1:]:
            pair_key = (te1.name.lower(), te2.name.lower())
            if pair_key in seen:
                continue
            seen.add(pair_key)
            relationships.append(Relationship(
                source=te1.name,
                target=te2.name,
                type="relates_to",
                description=f"{te1.name} used alongside {te2.name}",
                strength=4,
            ))
            if len(relationships) >= 6:
                return relationships

    # Strategy 3: Connect error entities to technologies
    error_entities = [e for e in entities if e.type == "error"]
    for err in error_entities:
        for tech in tech_entities:
            pair_key = (err.name.lower(), tech.name.lower())
            if pair_key in seen:
                continue
            seen.add(pair_key)
            relationships.append(Relationship(
                source=err.name,
                target=tech.name,
                type="caused_by",
                description=f"{err.name} related to {tech.name}",
                strength=5,
            ))
            if len(relationships) >= 6:
                return relationships

    return relationships[:6]


def _infer_relationship_type(source: Entity, target: Entity, category: str) -> str:
    """Infer the most plausible relationship type between two entities."""
    if source.type == "error" and target.type == "technology":
        return "caused_by"
    if source.type == "concept" and target.type == "technology":
        return "uses"
    if source.type == "pattern" and target.type == "technology":
        return "implements"
    if target.type == "config":
        return "configures"
    if target.type == "error":
        return "solves"
    if category in ("debugging", "bug-fix"):
        return "solves"
    return "relates_to"


def write_sidecar(doc_path: Path, doc_entities: DocumentEntities) -> Path:
    """Write an entities sidecar file next to a document.

    Returns the path to the written sidecar file.
    """
    sidecar_path = doc_path.with_suffix(".entities.yaml")
    sidecar_path.write_text(doc_entities.to_yaml())
    return sidecar_path
