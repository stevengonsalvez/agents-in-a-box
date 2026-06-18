# ainb-reflect-memory

Postgres-backed **GraphRAG memory substrate** for agents-in-a-box.

A durable memory layer many AINB agents can share, search, and cite — without
moving any LLM reasoning to the server.

> **Status:** Phase 1 (schema + FTS + graph tables + typed helper layer) **and**
> the shared **nano-graphrag backend** (Phase 2/3): Postgres-backed
> `BaseGraphStorage` / `BaseVectorStorage` / `BaseKVStorage` adapters + pgvector,
> so reflect's nano-graphrag runs unchanged against one store shared across
> machines. See [Shared nano-graphrag backend](#shared-nano-graphrag-backend).

---

## The contract: dumb server, smart client

```
┌──────────────────────────── CLIENT (smart) ────────────────────────────┐
│  embeddings · entity/edge extraction · summaries · answer synthesis     │
└───────────────┬─────────────────────────────────────────▲──────────────┘
                │ insert / upsert / search / evidence-pack │ evidence only
                ▼                                           │ (no answer)
┌──────────────────────────── SERVER (dumb) ──────────────────────────────┐
│  Postgres: store · enforce tenancy + RLS · lexical/graph queries         │
│  NO LLM calls · NO embeddings · NO answer synthesis — ever               │
└──────────────────────────────────────────────────────────────────────────┘
```

| Side       | Owns                                                                       |
| ---------- | -------------------------------------------------------------------------- |
| **Server** | storage, tenant isolation, RLS, lexical (FTS/trigram) + graph queries      |
| **Client** | embeddings, entity/edge extraction, summaries, final answer synthesis      |

The server returns **evidence** (ranked hits, entity matches, a graph
neighborhood, citations). The local agent turns that into an answer. The
database never synthesizes.

---

## Tenancy

`workspace_id` is the hard isolation boundary on **every** table, index, policy,
and query helper. `agent_id`, `source_session_id`, and `user_id` are optional
sub-scopes for provenance/filtering — they never widen access beyond the
workspace. A `Tenant` without a `workspace_id` raises before any SQL is built.

Defense in depth, two independent guards:

1. **Trusted server/worker path** (`MemoryStore` over psycopg) — every query
   carries `workspace_id` as a bound parameter, applied *before* ranking or
   graph expansion. Pinned by `tests/test_sql_builders.py`.
2. **Direct Supabase/PostgREST path** (JWT client) — Row-Level Security policies
   scope every row to `reflect_memory.current_workspace_id()`, which fails
   **closed** (denies all) when no workspace is resolvable.

---

## Data model (Phase 1)

```
memory_items ──evidence──┐
   (FTS + trigram)       │
                         ▼
entities ◀──source/target── edges
   (trigram + alias)        (relation graph)
```

- **memory_items** — atomic stored unit (summary, fact, preference, event,
  codebase note, decision, correction). Generated `tsvector` for FTS, trigram
  index on content, `content_hash` for dedupe.
- **entities** — canonical things; trigram on name, GIN on aliases.
- **edges** — typed relationships between entities, optional evidence ref back
  to a memory item. Composite FKs guarantee both endpoints + evidence live in
  the *same* workspace — a cross-tenant edge is physically impossible.

Full schema: [`supabase/migrations/0001_reflect_memory_phase1.sql`](supabase/migrations/0001_reflect_memory_phase1.sql).

---

## API

```python
from ainb_reflect_memory import (
    MemoryStore, Tenant,
    InsertMemoryInput, SearchMemoryInput,
    UpsertEntityInput, UpsertEdgeInput, EvidencePackQuery,
)
import psycopg
from psycopg.rows import dict_row

conn = psycopg.connect(DATABASE_URL, row_factory=dict_row)   # dict_row required
store = MemoryStore(conn)

t = Tenant(workspace_id="…uuid…", agent_id="…uuid…")

item = store.insert_memory(InsertMemoryInput(
    tenant=t, content="Auth token expiry uses a strict < check",
    source_type="codebase_note",
))

hits = store.search_memory(SearchMemoryInput(tenant=t, query="auth token expiry"))

ada = store.upsert_entity(UpsertEntityInput(
    tenant=t, canonical_name="Auth Middleware", entity_type="component"))
tok = store.upsert_entity(UpsertEntityInput(
    tenant=t, canonical_name="JWT", entity_type="concept"))
store.upsert_edge(UpsertEdgeInput(
    tenant=t, source_entity_id=ada.id, target_entity_id=tok.id,
    relation_type="uses", evidence_memory_id=item.id))

pack = store.get_evidence_pack(EvidencePackQuery(tenant=t, query="auth token expiry"))
# pack.lexical / pack.entities / pack.graph / pack.citations — evidence only.
```

| Method                       | Purpose                                                   |
| ---------------------------- | --------------------------------------------------------- |
| `insert_memory`              | insert a memory item (idempotent per normalized content)  |
| `search_memory`              | ranked FTS within the tenant, with highlighted snippets   |
| `upsert_entity`              | upsert a canonical entity (idempotent per type+name)      |
| `upsert_edge`                | upsert a typed edge (idempotent per source+target+rel)    |
| `lookup_entities`            | fuzzy entity lookup by canonical name / alias             |
| `neighborhood`               | entities + edges within N hops (same tenant only)         |
| `get_evidence_pack`          | lexical + entity + graph + citations, **no synthesis**    |

### Idempotency

Inserting the *same normalized content* in the *same tenant* updates the
existing row instead of creating a duplicate (`unique (workspace_id,
content_hash)`). Entities key on `(workspace_id, entity_type, canonical_name)`;
edges on `(workspace_id, source, target, relation_type)`. This is the Phase 3
ingestion-idempotency contract, established now.

---

## Install & layout

The base package is **dependency-free on purpose** — the typed helper layer
(models / normalize / SQL builders) imports and unit-tests *without* a database
driver or live credentials. The Postgres driver lives in the optional `[pg]`
extra and is only needed to talk to a live database.

```
ainb-reflect-memory/
├── src/ainb_reflect_memory/
│   ├── models.py       # typed inputs/records — the API boundary, validates early
│   ├── normalize.py    # content normalization + SHA-256 dedupe hash (client-side)
│   ├── sql.py          # pure (sql, params) builders — only place SQL text is made
│   ├── store.py        # MemoryStore: typed CRUD/search over a psycopg connection
│   └── errors.py       # TenantScopeError / ValidationError
├── supabase/migrations/0001_reflect_memory_phase1.sql
├── scripts/seed.py     # demo seed (memory + entity + edge)
├── tests/              # unit (no DB) + integration (auto-skip without DB)
└── docs/setup.md       # Supabase setup, Bitwarden secret names, commands
```

```bash
uv sync --extra dev            # install dev deps (pytest, ruff, psycopg)
uv run --extra dev pytest -m "not integration"   # unit tests, no DB needed
```

See [`docs/setup.md`](docs/setup.md) for Supabase setup, secret names, and the
migration / seed / integration-test commands.

---

## Phase roadmap

| Phase | Scope                                                              | Status |
| ----- | ------------------------------------------------------------------ | ------ |
| **1** | Postgres schema + FTS + trigram + graph tables + typed helpers     | ✅      |
| **2** | `pgvector` + client-generated embeddings (entity + chunk spaces)   | ✅      |
| **3** | nano-graphrag storage backends (graph/vector/KV) push to Postgres  | ✅      |
| 4     | community detection / summaries (client-side, versioned)           | ◑ clustering runs client-side via the graph backend; versioning TBD |
| 5     | evidence-pack API; local agent synthesizes the answer              | ▢      |

---

## Shared nano-graphrag backend

`ainb_reflect_memory.nanographrag` makes reflect's nano-graphrag run **unchanged**
against shared Postgres, so the **same vector index + entity/relation graph +
community reports are visible from every machine**. It implements nano-graphrag's
three pluggable storage interfaces (the same way nano-graphrag ships a
`Neo4jStorage`):

| Adapter | nano-graphrag interface | Postgres table(s) |
| --- | --- | --- |
| `PgGraphStorage` | `BaseGraphStorage` (subclasses `NetworkXStorage`) | `ng_graph_nodes`, `ng_graph_edges` |
| `PgVectorStorage` | `BaseVectorStorage` (pgvector ANN) | `ng_vectors` (entity + chunk spaces) |
| `PgKVStorage` | `BaseKVStorage` | `ng_kv` (full_docs, text_chunks, community_reports, …) |

The database stays **dumb**: embeddings are computed client-side by the injected
`embedding_func`, and Leiden clustering runs in-process (`PgGraphStorage` reuses
`NetworkXStorage`'s clustering verbatim — only load/save move to Postgres; no
`.graphml` file is written). Wiring:

```python
from nano_graphrag import GraphRAG
from ainb_reflect_memory.nanographrag import storage_classes, addon_params

graph = GraphRAG(
    working_dir=tmp_dir,
    embedding_func=my_client_side_embedder,   # LLM/embeds stay on the client
    **storage_classes(),
    addon_params=addon_params(pg_dsn=DATABASE_URL, workspace_id="…uuid…"),
)
```

reflect-kb's `LearningsGraphEngine` enables this automatically when
`REFLECT_PG_DSN` (or `DATABASE_URL`) **and** `REFLECT_WORKSPACE_ID` are set;
otherwise it keeps its original local-file behavior. Schema lives in
[`supabase/migrations/0002_nanographrag_pgvector.sql`](supabase/migrations/0002_nanographrag_pgvector.sql);
see [`scripts/demo_cross_machine.py`](scripts/demo_cross_machine.py) for an
end-to-end "machine A writes, machine B reads" proof.

The embedding model is **pinned** (all-mpnet-base-v2, 768-d, unit-normalized);
`ng_vectors` records `model` + `dims` so a model change is a versioned re-embed,
never silent reuse.

---

## Extraction boundary

This package is built to be lifted into a standalone repo
(`stevengonsalvez/ainb-reflect-memory`) unchanged once reuse outside AINB is
real. To keep that option cheap:

- **Self-contained** under `ainb-reflect-memory/` — own `pyproject.toml`,
  `src/`, `tests/`, `supabase/migrations/`, `docs/`.
- **No imports across the boundary.** It does not import `reflect_kb` or any
  AINB module, and nothing in AINB imports its internals — only the small public
  surface (`MemoryStore`, `Tenant`, the input/record dataclasses, and the
  optional `nanographrag` adapters). The `nanographrag` submodule depends on
  `nano-graphrag` as an OPTIONAL peer (the `[nanographrag]` extra) — the core
  package stays dependency-free; reflect-kb is the consumer that wires it in.
- **Portable migration.** The SQL runs on a vanilla Postgres (tests) and on
  Supabase (role-conditional grants); the tenant resolver falls back from JWT
  claim to a GUC so it works without Supabase Auth.
- **String UUIDs** at the boundary so a future non-Python client reproduces the
  same shapes.

Extraction = `git filter-repo` this directory into a new repo, then point AINB
at it via submodule or package dependency. Nothing here assumes the monorepo.

---

## Client scope and access path

First clients are **all of them**: AINB CLI runtime, web surfaces, and the Claude/Codex/Copilot hook paths. The API boundary stays language-portable so hooks can ingest and query memory without dragging LLM reasoning server-side.

Recommended production path:

1. **Trusted worker writes / privileged graph mutations.** Hooks and web clients call a worker with normal workspace identity; the worker validates payloads, sets tenant context, applies rate limits/audit, and uses server-held credentials.
2. **Direct Supabase anon path only for tightly scoped read/search or local dev.** RLS remains the independent guard, but service-role credentials never reach browser code, local hooks, or agent subprocesses.

Why: hooks are heterogeneous and easy to leak from. A worker keeps service-role power in one blast radius while preserving the dumb Postgres substrate.

---

## Security

- Tenant isolation is mandatory; every helper scopes by `workspace_id` first.
- The **service-role key must never** reach browser/client code — it bypasses
  RLS and is for migrations/trusted workers only.
- Migrations touching RLS require **partner review before merge** — do not
  self-merge. See [`docs/setup.md`](docs/setup.md).

## License

MIT
