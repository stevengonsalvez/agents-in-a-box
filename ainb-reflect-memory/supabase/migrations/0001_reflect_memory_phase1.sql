-- ============================================================================
-- ainb-reflect-memory — Phase 1 schema: memory items + entities + edges,
-- full-text search, trigram fuzzy lookup, tenant + graph indexes, RLS.
--
-- Design contract (see README):
--   * Server is DUMB and SEARCHABLE: it stores, scopes by tenant, and runs
--     lexical / graph queries. No LLM calls, no answer synthesis, ever.
--   * Tenant isolation is mandatory. `workspace_id` is the hard boundary on
--     every table, every index, every policy, every function.
--   * pgvector / embeddings are deliberately NOT here — that is Phase 2.
--
-- !! SECURITY REVIEW REQUIRED !!
-- This migration defines Row-Level Security policies. Per the issue's security
-- requirements, migrations touching RLS must get partner review before merge.
-- Do NOT self-merge. The RLS policies guard the *direct Supabase/PostgREST
-- client* path; the Python MemoryStore (trusted server/worker) is additionally
-- protected by explicit per-query workspace filtering.
--
-- Re-runnable: uses IF NOT EXISTS / CREATE OR REPLACE / DROP POLICY IF EXISTS
-- so it applies cleanly to a fresh database and is safe to re-apply in dev.
-- ============================================================================

-- ---------------------------------------------------------------------------
-- Extensions
-- ---------------------------------------------------------------------------
-- pgcrypto: gen_random_uuid(). pg_trgm: trigram fuzzy match on names/content.
-- `vector` is intentionally omitted until Phase 2.
create extension if not exists pgcrypto;
create extension if not exists pg_trgm;

-- ---------------------------------------------------------------------------
-- Schema
-- ---------------------------------------------------------------------------
create schema if not exists reflect_memory;

-- ---------------------------------------------------------------------------
-- updated_at trigger helper
-- ---------------------------------------------------------------------------
create or replace function reflect_memory.set_updated_at()
returns trigger
language plpgsql
set search_path = pg_catalog
as $$
begin
  new.updated_at := now();
  return new;
end;
$$;

-- ---------------------------------------------------------------------------
-- Tenant resolver — single source of truth for "which workspace is this
-- request?", used by every RLS policy.
--
-- The SIGNED JWT claim is AUTHORITATIVE. Resolution order:
--   1. `request.jwt.claims -> workspace_id` (Supabase, verified signature).
--      When a JWT is present, its workspace_id is the only answer — a JWT with
--      no workspace_id DENIES (returns NULL); it never falls through to the GUC.
--   2. ONLY when there is no JWT at all (trusted no-JWT worker / tests): the
--      `app.current_workspace` GUC.
-- Returns NULL when neither resolves, so every RLS policy fails CLOSED.
--
-- Why JWT-first: `app.current_workspace` is a custom placeholder GUC and is
-- USERSET — any role (incl. anon/authenticated) could `set_config` it. If the
-- GUC won over the JWT, a client who could set it would override their signed
-- tenant. JWT-first removes that trust inversion; the GUC only matters on
-- connections that carry no JWT (the trusted worker), which clients cannot
-- forge into having a JWT.
-- STABLE, SECURITY INVOKER, pinned search_path.
-- ---------------------------------------------------------------------------
create or replace function reflect_memory.current_workspace_id()
returns uuid
language plpgsql
stable
set search_path = pg_catalog
as $$
declare
  v_guc text;
  v_claims text;
  v_ws text;
begin
  -- 1. signed JWT claim is authoritative when a JWT is present
  begin
    v_claims := current_setting('request.jwt.claims', true);
  exception when others then
    v_claims := null;
  end;
  if v_claims is not null and v_claims <> '' then
    v_ws := (v_claims::jsonb) ->> 'workspace_id';
    if v_ws is not null and v_ws <> '' then
      return v_ws::uuid;
    end if;
    return null;  -- JWT present but no workspace_id => deny; do NOT use the GUC
  end if;

  -- 2. no JWT at all (trusted worker / tests) => the GUC
  begin
    v_guc := current_setting('app.current_workspace', true);
  exception when others then
    v_guc := null;
  end;
  if v_guc is not null and v_guc <> '' then
    return v_guc::uuid;
  end if;

  return null;
end;
$$;

-- ===========================================================================
-- memory_items — the atomic stored unit (summary, fact, preference, event…)
-- ===========================================================================
create table if not exists reflect_memory.memory_items (
  id                 uuid primary key default gen_random_uuid(),
  workspace_id       uuid not null,
  agent_id           uuid,
  source_session_id  text,
  user_id            uuid,
  source_type        text not null,
  source_uri         text,
  content            text not null,
  -- normalized-content SHA-256 (computed client-side); dedupe key per tenant.
  content_hash       text not null,
  metadata           jsonb not null default '{}'::jsonb,
  -- trust/confidence in [0,1].
  confidence         real not null default 0.5
                       check (confidence >= 0.0 and confidence <= 1.0),
  created_at         timestamptz not null default now(),
  updated_at         timestamptz not null default now(),
  -- maintained FTS column; generated so it can never drift from content.
  search_vector      tsvector
                       generated always as
                       (to_tsvector('english', coalesce(content, ''))) stored,
  -- composite-unique on (workspace_id, id) lets edges/evidence FK enforce that
  -- a referenced row lives in the SAME workspace (see edges below).
  constraint memory_items_workspace_id_key unique (workspace_id, id),
  -- idempotent dedupe: same normalized content in same tenant = same row.
  constraint memory_items_workspace_content_hash_key
    unique (workspace_id, content_hash)
);

-- tenant + recency, tenant + agent
create index if not exists memory_items_workspace_created_idx
  on reflect_memory.memory_items (workspace_id, created_at desc);
create index if not exists memory_items_workspace_agent_idx
  on reflect_memory.memory_items (workspace_id, agent_id);
-- full-text
create index if not exists memory_items_search_vector_idx
  on reflect_memory.memory_items using gin (search_vector);
-- trigram fuzzy on raw content
create index if not exists memory_items_content_trgm_idx
  on reflect_memory.memory_items using gin (content gin_trgm_ops);

drop trigger if exists memory_items_set_updated_at on reflect_memory.memory_items;
create trigger memory_items_set_updated_at
  before update on reflect_memory.memory_items
  for each row execute function reflect_memory.set_updated_at();

-- ===========================================================================
-- entities — canonical things found in memory
-- ===========================================================================
create table if not exists reflect_memory.entities (
  id             uuid primary key default gen_random_uuid(),
  workspace_id   uuid not null,
  canonical_name text not null,
  entity_type    text not null,
  aliases        text[] not null default '{}',
  metadata       jsonb not null default '{}'::jsonb,
  created_at     timestamptz not null default now(),
  updated_at     timestamptz not null default now(),
  constraint entities_workspace_id_key unique (workspace_id, id),
  -- upsert key: one canonical entity per (tenant, type, name).
  constraint entities_workspace_type_name_key
    unique (workspace_id, entity_type, canonical_name)
);

create index if not exists entities_workspace_idx
  on reflect_memory.entities (workspace_id);
create index if not exists entities_canonical_trgm_idx
  on reflect_memory.entities using gin (canonical_name gin_trgm_ops);
create index if not exists entities_aliases_idx
  on reflect_memory.entities using gin (aliases);

drop trigger if exists entities_set_updated_at on reflect_memory.entities;
create trigger entities_set_updated_at
  before update on reflect_memory.entities
  for each row execute function reflect_memory.set_updated_at();

-- ===========================================================================
-- edges — relationships between entities, optionally backed by evidence
-- ===========================================================================
create table if not exists reflect_memory.edges (
  id                 uuid primary key default gen_random_uuid(),
  workspace_id       uuid not null,
  source_entity_id   uuid not null,
  target_entity_id   uuid not null,
  relation_type      text not null,
  evidence_memory_id uuid,
  weight             real not null default 1.0,
  metadata           jsonb not null default '{}'::jsonb,
  created_at         timestamptz not null default now(),
  updated_at         timestamptz not null default now(),
  -- both endpoints must live in the SAME workspace as the edge: the composite
  -- FKs reference (workspace_id, id), so a cross-tenant edge is impossible.
  constraint edges_source_fk
    foreign key (workspace_id, source_entity_id)
    references reflect_memory.entities (workspace_id, id) on delete cascade,
  constraint edges_target_fk
    foreign key (workspace_id, target_entity_id)
    references reflect_memory.entities (workspace_id, id) on delete cascade,
  constraint edges_evidence_fk
    foreign key (workspace_id, evidence_memory_id)
    references reflect_memory.memory_items (workspace_id, id) on delete set null,
  -- upsert key.
  constraint edges_workspace_triple_key
    unique (workspace_id, source_entity_id, target_entity_id, relation_type)
);

create index if not exists edges_workspace_source_idx
  on reflect_memory.edges (workspace_id, source_entity_id);
create index if not exists edges_workspace_target_idx
  on reflect_memory.edges (workspace_id, target_entity_id);
create index if not exists edges_workspace_relation_idx
  on reflect_memory.edges (workspace_id, relation_type);
create index if not exists edges_evidence_idx
  on reflect_memory.edges (evidence_memory_id);

drop trigger if exists edges_set_updated_at on reflect_memory.edges;
create trigger edges_set_updated_at
  before update on reflect_memory.edges
  for each row execute function reflect_memory.set_updated_at();

-- ===========================================================================
-- Searchable-server functions. These are the lexical / graph query surface,
-- reachable from psql, PostgREST RPC, or the Python helper. Every one takes
-- the workspace id as its FIRST argument and filters by it BEFORE ranking or
-- traversal, so a tenant can never leak through them.
-- ===========================================================================

-- Ranked full-text search. Returns the memory row + ts_rank + a highlighted
-- ts_headline snippet.
create or replace function reflect_memory.search_memory(
  p_workspace_id uuid,
  p_query        text,
  p_limit        int  default 10,
  p_agent_id     uuid default null,
  p_min_rank     real default null
)
returns table (
  id                 uuid,
  workspace_id       uuid,
  agent_id           uuid,
  source_session_id  text,
  user_id            uuid,
  source_type        text,
  source_uri         text,
  content            text,
  content_hash       text,
  metadata           jsonb,
  confidence         real,
  created_at         timestamptz,
  updated_at         timestamptz,
  rank               real,
  snippet            text
)
language sql
stable
set search_path = pg_catalog, public, extensions, reflect_memory
as $$
  with q as (select websearch_to_tsquery('english', p_query) as tsq)
  select
    m.id, m.workspace_id, m.agent_id, m.source_session_id, m.user_id,
    m.source_type, m.source_uri, m.content, m.content_hash, m.metadata,
    m.confidence, m.created_at, m.updated_at,
    ts_rank(m.search_vector, q.tsq) as rank,
    ts_headline('english', m.content, q.tsq,
      'StartSel=<b>, StopSel=</b>, MaxFragments=2, MaxWords=18, MinWords=5'
    ) as snippet
  from reflect_memory.memory_items m, q
  where m.workspace_id = p_workspace_id
    and m.search_vector @@ q.tsq
    and (p_agent_id is null or m.agent_id = p_agent_id)
    and (p_min_rank is null or ts_rank(m.search_vector, q.tsq) >= p_min_rank)
  order by rank desc, m.created_at desc
  limit greatest(p_limit, 1);
$$;

-- Fuzzy entity lookup by canonical name or alias. matched_alias is the alias
-- that triggered the match (NULL when the canonical name matched).
create or replace function reflect_memory.search_entities(
  p_workspace_id uuid,
  p_query        text,
  p_limit        int default 10
)
returns table (
  id             uuid,
  workspace_id   uuid,
  canonical_name text,
  entity_type    text,
  aliases        text[],
  metadata       jsonb,
  created_at     timestamptz,
  updated_at     timestamptz,
  matched_alias  text,
  score          real
)
language sql
stable
set search_path = pg_catalog, public, extensions, reflect_memory
as $$
  select
    e.id, e.workspace_id, e.canonical_name, e.entity_type, e.aliases,
    e.metadata, e.created_at, e.updated_at,
    (select a from unnest(e.aliases) a
      where lower(a) = lower(p_query) limit 1) as matched_alias,
    greatest(
      similarity(e.canonical_name, p_query),
      case when exists (
        select 1 from unnest(e.aliases) a where lower(a) = lower(p_query)
      ) then 1.0 else 0.0 end
    ) as score
  from reflect_memory.entities e
  where e.workspace_id = p_workspace_id
    and (
      e.canonical_name ilike '%' || p_query || '%'
      or similarity(e.canonical_name, p_query) > 0.2
      or exists (
        select 1 from unnest(e.aliases) a where lower(a) = lower(p_query)
      )
    )
  order by score desc, e.canonical_name asc
  limit greatest(p_limit, 1);
$$;

-- Graph neighborhood: every edge incident to an entity reachable from
-- p_entity_id within p_max_depth hops, same tenant only. The recursive walk
-- re-checks workspace_id on every hop.
create or replace function reflect_memory.entity_neighborhood(
  p_workspace_id uuid,
  p_entity_id    uuid,
  p_max_depth    int default 1
)
returns table (
  id                 uuid,
  workspace_id       uuid,
  source_entity_id   uuid,
  target_entity_id   uuid,
  relation_type      text,
  evidence_memory_id uuid,
  weight             real,
  metadata           jsonb,
  created_at         timestamptz,
  updated_at         timestamptz
)
language sql
stable
set search_path = pg_catalog, public, extensions, reflect_memory
as $$
  with recursive reachable(entity_id, depth) as (
    select p_entity_id, 0
    union
    select
      case when e.source_entity_id = r.entity_id
           then e.target_entity_id else e.source_entity_id end,
      r.depth + 1
    from reachable r
    join reflect_memory.edges e
      on e.workspace_id = p_workspace_id
     and (e.source_entity_id = r.entity_id or e.target_entity_id = r.entity_id)
    where r.depth < least(greatest(p_max_depth, 0), 5)  -- clamp fan-out depth
  )
  select distinct
    e.id, e.workspace_id, e.source_entity_id, e.target_entity_id,
    e.relation_type, e.evidence_memory_id, e.weight, e.metadata,
    e.created_at, e.updated_at
  from reflect_memory.edges e
  join reachable r
    on (e.source_entity_id = r.entity_id or e.target_entity_id = r.entity_id)
  where e.workspace_id = p_workspace_id;
$$;

-- ===========================================================================
-- Row-Level Security  ⚠ partner review required before merge
-- ===========================================================================
-- Policies scope every row to reflect_memory.current_workspace_id(). When that
-- resolver returns NULL (no GUC, no JWT claim) the predicate is false and the
-- row is denied — fail closed. Table owners and `service_role` (BYPASSRLS)
-- bypass these by design; that is the trusted migration/worker path.
alter table reflect_memory.memory_items enable row level security;
alter table reflect_memory.entities    enable row level security;
alter table reflect_memory.edges        enable row level security;

drop policy if exists memory_items_tenant_isolation on reflect_memory.memory_items;
create policy memory_items_tenant_isolation
  on reflect_memory.memory_items
  for all
  using (workspace_id = reflect_memory.current_workspace_id())
  with check (workspace_id = reflect_memory.current_workspace_id());

drop policy if exists entities_tenant_isolation on reflect_memory.entities;
create policy entities_tenant_isolation
  on reflect_memory.entities
  for all
  using (workspace_id = reflect_memory.current_workspace_id())
  with check (workspace_id = reflect_memory.current_workspace_id());

drop policy if exists edges_tenant_isolation on reflect_memory.edges;
create policy edges_tenant_isolation
  on reflect_memory.edges
  for all
  using (workspace_id = reflect_memory.current_workspace_id())
  with check (workspace_id = reflect_memory.current_workspace_id());

-- ===========================================================================
-- Grants — least privilege, matching the documented access model:
--   * direct clients (`authenticated`) get READ-ONLY: SELECT + EXECUTE on the
--     read-only search functions. Writes/graph mutations go through the trusted
--     worker (`service_role`), NOT direct PostgREST — so an authenticated user
--     can't poison their tenant's graph/vectors via a raw REST write.
--   * `anon` gets nothing; PUBLIC defaults are revoked so inertness is explicit.
--   * `service_role` is full + BYPASSRLS (the trusted migration/worker path).
-- Portable: only touches roles that exist (vanilla Postgres tests skip them).
-- ===========================================================================
-- Strip PostgreSQL's default PUBLIC grants so "anon gets nothing" is enforced,
-- not incidental (PUBLIC has EXECUTE on functions by default).
revoke all on all functions in schema reflect_memory from public;
revoke all on all tables in schema reflect_memory from public;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    grant usage on schema reflect_memory to authenticated;
    -- READ-ONLY: select on tables; execute only on the search functions.
    grant select on all tables in schema reflect_memory to authenticated;
    grant execute on function
      reflect_memory.search_memory(uuid, text, int, uuid, real),
      reflect_memory.search_entities(uuid, text, int),
      reflect_memory.entity_neighborhood(uuid, uuid, int),
      reflect_memory.current_workspace_id()
      to authenticated;
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    grant usage on schema reflect_memory to service_role;
    grant all on all tables in schema reflect_memory to service_role;
    grant execute on all functions in schema reflect_memory to service_role;
  end if;
end;
$$;
