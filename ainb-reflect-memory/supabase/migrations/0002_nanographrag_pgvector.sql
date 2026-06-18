-- ============================================================================
-- ainb-reflect-memory — Phase 2 schema: Postgres-backed storage for
-- nano-graphrag, so reflect's GraphRAG runs UNCHANGED against a shared store
-- and the same vectors / graph / community reports are visible from every
-- machine.
--
-- These tables back three nano-graphrag storage adapters (see
-- src/ainb_reflect_memory/nanographrag/):
--   * ng_kv          ← BaseKVStorage      (full_docs · text_chunks ·
--                                          community_reports · llm_response_cache)
--   * ng_graph_nodes ← BaseGraphStorage   (entity nodes: name, type,
--   * ng_graph_edges                       description, source_id, clusters)
--   * ng_vectors     ← BaseVectorStorage  (TWO spaces: entities · chunks)
--
-- Design contract (unchanged from Phase 1):
--   * Server is DUMB: it stores, scopes by tenant, runs ANN / graph reads.
--     NO LLM, NO embedding generation, NO answer synthesis server-side.
--     Embeddings are computed CLIENT-SIDE (sentence-transformers) and arrive
--     as ready-made vectors; Leiden clustering also runs client-side.
--   * `workspace_id` is the hard tenant boundary on every table / index /
--     policy, exactly like Phase 1.
--
-- Embedding model is PINNED: all-mpnet-base-v2, 768-dim, unit-normalized
-- (cosine == dot product). The `embedding` column is vector(768) to match.
-- A model change is a NEW migration + versioned re-embed, never silent reuse;
-- ng_vectors carries `model` + `dims` so a mismatch is detectable, not fatal.
--
-- !! SECURITY REVIEW REQUIRED !!
-- This migration defines Row-Level Security policies (mirroring 0001). Per the
-- issue's security requirements, RLS-touching migrations get partner review
-- before merge. Do NOT self-merge.
--
-- Depends on 0001 (schema reflect_memory, current_workspace_id(),
-- set_updated_at()). Re-runnable: IF NOT EXISTS / CREATE OR REPLACE /
-- DROP POLICY IF EXISTS.
-- ============================================================================

-- pgvector — required for the embedding column + ANN index. Available on
-- Supabase by default; locally `CREATE EXTENSION vector` needs the pgvector
-- package installed.
create extension if not exists vector;

-- schema + helpers come from 0001; assert it ran.
do $$
begin
  if not exists (select 1 from pg_namespace where nspname = 'reflect_memory') then
    raise exception 'run 0001_reflect_memory_phase1.sql before 0002';
  end if;
end;
$$;

-- ===========================================================================
-- ng_kv — nano-graphrag BaseKVStorage. One row per (tenant, namespace, key);
-- value is the raw JSON document nano-graphrag stores (a full_doc, a text
-- chunk, a community report, or an llm cache entry).
-- ===========================================================================
create table if not exists reflect_memory.ng_kv (
  workspace_id uuid not null,
  namespace    text not null,
  key          text not null,
  value        jsonb not null,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now(),
  primary key (workspace_id, namespace, key)
);

drop trigger if exists ng_kv_set_updated_at on reflect_memory.ng_kv;
create trigger ng_kv_set_updated_at
  before update on reflect_memory.ng_kv
  for each row execute function reflect_memory.set_updated_at();

-- ===========================================================================
-- ng_graph_nodes / ng_graph_edges — nano-graphrag BaseGraphStorage.
-- The graph is undirected (nano-graphrag uses nx.Graph). node_id is the
-- entity name as nano-graphrag keys it (e.g. "AUTH MIDDLEWARE"). All node/edge
-- attributes (entity_type, description, source_id, weight, order, clusters…)
-- live in `attrs` jsonb so the adapter can round-trip nx node/edge data dicts
-- losslessly without the schema having to know nano-graphrag's evolving keys.
-- ===========================================================================
create table if not exists reflect_memory.ng_graph_nodes (
  workspace_id uuid not null,
  namespace    text not null,
  node_id      text not null,
  attrs        jsonb not null default '{}'::jsonb,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now(),
  primary key (workspace_id, namespace, node_id)
);
create index if not exists ng_graph_nodes_ws_ns_idx
  on reflect_memory.ng_graph_nodes (workspace_id, namespace);

create table if not exists reflect_memory.ng_graph_edges (
  workspace_id uuid not null,
  namespace    text not null,
  source       text not null,
  target       text not null,
  attrs        jsonb not null default '{}'::jsonb,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now(),
  primary key (workspace_id, namespace, source, target)
);
create index if not exists ng_graph_edges_ws_ns_source_idx
  on reflect_memory.ng_graph_edges (workspace_id, namespace, source);
create index if not exists ng_graph_edges_ws_ns_target_idx
  on reflect_memory.ng_graph_edges (workspace_id, namespace, target);

drop trigger if exists ng_graph_nodes_set_updated_at on reflect_memory.ng_graph_nodes;
create trigger ng_graph_nodes_set_updated_at
  before update on reflect_memory.ng_graph_nodes
  for each row execute function reflect_memory.set_updated_at();

drop trigger if exists ng_graph_edges_set_updated_at on reflect_memory.ng_graph_edges;
create trigger ng_graph_edges_set_updated_at
  before update on reflect_memory.ng_graph_edges
  for each row execute function reflect_memory.set_updated_at();

-- ===========================================================================
-- ng_vectors — nano-graphrag BaseVectorStorage. namespace separates the TWO
-- spaces nano-graphrag maintains (entities, chunks). `id` is nano-graphrag's
-- vector id; `meta` holds the meta_fields it persists (e.g. entity_name).
-- `embedding` is the client-computed, unit-normalized 768-d vector.
-- ===========================================================================
create table if not exists reflect_memory.ng_vectors (
  workspace_id uuid not null,
  namespace    text not null,
  id           text not null,
  meta         jsonb not null default '{}'::jsonb,
  model        text not null default 'all-mpnet-base-v2',
  dims         int  not null default 768,
  embedding    vector(768) not null,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now(),
  primary key (workspace_id, namespace, id)
);
create index if not exists ng_vectors_ws_ns_idx
  on reflect_memory.ng_vectors (workspace_id, namespace);
-- ANN index for cosine similarity. HNSW (pgvector >= 0.5; Supabase ships it).
-- Cosine ops because embeddings are unit-normalized.
create index if not exists ng_vectors_embedding_hnsw_idx
  on reflect_memory.ng_vectors using hnsw (embedding vector_cosine_ops);

drop trigger if exists ng_vectors_set_updated_at on reflect_memory.ng_vectors;
create trigger ng_vectors_set_updated_at
  before update on reflect_memory.ng_vectors
  for each row execute function reflect_memory.set_updated_at();

-- ===========================================================================
-- Row-Level Security  ⚠ partner review required before merge
-- Same model as 0001: every row scoped to current_workspace_id(); NULL = deny.
-- ===========================================================================
alter table reflect_memory.ng_kv          enable row level security;
alter table reflect_memory.ng_graph_nodes enable row level security;
alter table reflect_memory.ng_graph_edges enable row level security;
alter table reflect_memory.ng_vectors     enable row level security;

drop policy if exists ng_kv_tenant_isolation on reflect_memory.ng_kv;
create policy ng_kv_tenant_isolation on reflect_memory.ng_kv
  for all
  using (workspace_id = reflect_memory.current_workspace_id())
  with check (workspace_id = reflect_memory.current_workspace_id());

drop policy if exists ng_graph_nodes_tenant_isolation on reflect_memory.ng_graph_nodes;
create policy ng_graph_nodes_tenant_isolation on reflect_memory.ng_graph_nodes
  for all
  using (workspace_id = reflect_memory.current_workspace_id())
  with check (workspace_id = reflect_memory.current_workspace_id());

drop policy if exists ng_graph_edges_tenant_isolation on reflect_memory.ng_graph_edges;
create policy ng_graph_edges_tenant_isolation on reflect_memory.ng_graph_edges
  for all
  using (workspace_id = reflect_memory.current_workspace_id())
  with check (workspace_id = reflect_memory.current_workspace_id());

drop policy if exists ng_vectors_tenant_isolation on reflect_memory.ng_vectors;
create policy ng_vectors_tenant_isolation on reflect_memory.ng_vectors
  for all
  using (workspace_id = reflect_memory.current_workspace_id())
  with check (workspace_id = reflect_memory.current_workspace_id());

-- ===========================================================================
-- Grants — least privilege (matches 0001): direct `authenticated` clients get
-- READ-ONLY; writes to the graph/vector store go through the trusted worker
-- (`service_role`). PUBLIC defaults revoked. Portable to vanilla Postgres.
-- ===========================================================================
revoke all on reflect_memory.ng_kv, reflect_memory.ng_graph_nodes,
              reflect_memory.ng_graph_edges, reflect_memory.ng_vectors
  from public;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    grant select
      on reflect_memory.ng_kv, reflect_memory.ng_graph_nodes,
         reflect_memory.ng_graph_edges, reflect_memory.ng_vectors
      to authenticated;
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    grant all
      on reflect_memory.ng_kv, reflect_memory.ng_graph_nodes,
         reflect_memory.ng_graph_edges, reflect_memory.ng_vectors
      to service_role;
  end if;
end;
$$;
