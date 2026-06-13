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

Closed enum — every `relationships[].type` MUST be one of the values below.
Typed causal links (S2, Hindsight `memory_links` shape) carry graph semantics:
"what enabled this fix?" / "what does this rule prevent?" / "what does this
learning supersede?" become answerable from sidecars.

### Typed causal links (preferred)

| Type | Description | Example |
|------|-------------|---------|
| `caused_by` | Effect was caused by source condition | nested runtime panic -> block_on |
| `causes` | Source condition produces the effect | block_on -> nested runtime panic |
| `enables` | Source makes the target possible | spawn_blocking -> sync code in async context |
| `prevents` | Source stops the target from happening | connection pooling -> n+1 query |
| `contradicts` | Source conflicts with the target | eager loading rule -> lazy loading rule |
| `supersedes` | Source replaces the (now outdated) target | new auth flow -> legacy session cookie |
| `part_of` | Source is a component of the target | spawn_blocking -> tokio |
| `uses` | Source depends on / invokes the target | webpack -> babel |

### Legacy types (still valid)

| Type | Description | Example |
|------|-------------|---------|
| `solves` | What fixes the error | spawn_blocking -> nested runtime panic |
| `requires` | Prerequisites | spawn_blocking -> tokio runtime |
| `relates_to` | Related concepts (weakest edge; backfill default) | tokio -> async context |
| `implements` | Source realizes the target pattern/spec | prefetch_related -> eager loading |
| `configures` | Source sets up the target | webpack.config.js -> webpack |
| `triggers` | Source sets off the target event | deploy hook -> cache invalidation |

## Extraction Guidelines

- Extract 3-8 entities per learning (focused, not exhaustive)
- Always include at least one `solves` relationship for bug-fix type
- **Prefer a typed causal link over `relates_to`** whenever the direction of
  causality/effect is known — `relates_to` is the fallback for genuinely
  undirected association only
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
    type: caused_by | causes | enables | prevents | contradicts | supersedes | part_of | uses | solves | requires | relates_to | implements | configures | triggers
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
  - source: "spawn_blocking"
    target: "nested runtime panic"
    type: prevents
    description: "spawn_blocking keeps sync work off the async runtime, preventing the panic"
    strength: 9
```
