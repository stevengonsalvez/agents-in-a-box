# Knowledge Format Reference

Entity types, relationship types, and extraction guidelines for GraphRAG indexing.

## Entity Types

| Type | Description | Examples |
|------|-------------|----------|
| `technology` | Languages, frameworks, runtimes | tokio, react, postgresql |
| `error` | Error types, messages, exceptions | nested runtime panic, n+1 query |
| `pattern` | Design patterns, anti-patterns | eager loading, spawn_blocking |
| `function` | Specific functions, methods, APIs | block_on, prefetch_related |
| `concept` | Abstract concepts, principles | async context, connection pooling |
| `tool` | CLI tools, dev tools, services | cargo, webpack, docker |

## Relationship Types

| Type | Description | Example |
|------|-------------|---------|
| `caused_by` | What caused the error | block_on -> nested runtime panic |
| `solves` | What fixes the error | spawn_blocking -> nested runtime panic |
| `requires` | Prerequisites | spawn_blocking -> tokio runtime |
| `relates_to` | Related concepts | tokio -> async context |

## Extraction Guidelines

- Extract 3-8 entities per learning (focused, not exhaustive)
- Always include at least one `solves` relationship for bug-fix type
- Strength: 9-10 direct/causal, 5-7 moderate, 1-4 weak
- Entity names normalized to lowercase canonical form
- Use the most specific entity type available

## Entity Sidecar Format (`.entities.yaml`)

```yaml
document_id: lrn-{slug}-{hash6}
extracted_at: "{ISO timestamp}"
entities:
  - name: "{entity name}"
    type: technology | error | pattern | function | concept | tool
    description: "{brief description}"
relationships:
  - source: "{entity A}"
    target: "{entity B}"
    type: caused_by | solves | requires | relates_to
    description: "{how they relate}"
    strength: 1-10
```

## Example

```yaml
entities:
  - name: "tokio"
    type: technology
    description: "Async runtime for Rust"
  - name: "nested runtime panic"
    type: error
    description: "Cannot start a runtime from within a runtime"
  - name: "spawn_blocking"
    type: function
    description: "Tokio function to run sync code within async context"
relationships:
  - source: "block_on"
    target: "nested runtime panic"
    type: caused_by
    description: "Calling block_on inside async context causes nested runtime panic"
    strength: 9
  - source: "spawn_blocking"
    target: "nested runtime panic"
    type: solves
    description: "Use spawn_blocking instead of block_on for sync code in async context"
    strength: 10
```
