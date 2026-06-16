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
    # A2: bitemporal clocks (all optional, ISO-8601 date or datetime)
    tcommit: "{when reflect LEARNED this edge}"   # defaults to extracted_at
    tvalid: "{when it became true in the world}"  # defaults to tcommit
    tvalid_end: "{when it stopped being true}"    # absent => still valid
    superseded_by: "{id of the edge that replaced this one}"  # optional
```

### Bitemporal relationship clocks (A2)

Causal edges carry **two independent clocks** so a graph query can separate
*what was true in the world* from *what we knew at the time*:

| Field | Meaning | Default |
|-------|---------|---------|
| `tcommit` | Transaction time — when reflect **learned** the relationship | the sidecar's `extracted_at` (ingest time) |
| `tvalid` | Valid time — when the relationship became **true in the world** | `tcommit` |
| `tvalid_end` | When the relationship **stopped** being true | absent = still valid (open interval) |
| `superseded_by` | The edge that replaced this one | absent |

- All four are **optional and additive** — sidecars without them validate
  exactly as before. Each timestamp, when present, MUST be a valid ISO-8601
  date or datetime; a malformed clock is rejected by `validate_sidecar.py`.
- **Supersession, not deletion.** When a relationship is replaced (the
  architecture changed — "JWT in April, sessions in June"), set the old edge's
  `tvalid_end` and `superseded_by` instead of removing it. History is
  preserved, and a query scoped to the old window still surfaces the old truth.
- A date-range recall query filters graph-arm edges by **`tvalid` overlap**:
  *"what was the architecture in April?"* keeps only edges valid in April
  (`tvalid <= window.end and (tvalid_end is None or tvalid_end >= window.start)`).
  Use the `tcommit` clock instead to ask *"what did we KNOW in April?"*.
- Stamp missing `tcommit` values in bulk with
  `validate_sidecar.py --backfill-tcommit` (defaults each to `extracted_at`).

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
