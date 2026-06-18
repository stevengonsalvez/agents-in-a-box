# Journal

## Boot: 2026-06-18T09:57:13Z
- agent: freeman
- inbox: kh79aa9c021hhynw5ssw07s2rd88wt40
- repo: stevengonsalvez/agents-in-a-box
- branch: freeman/kh79aa9c021hhynw5ssw07s2rd88wt40-reflect-memory
- base: origin/main
- main_clone: /tmp/ainb-clean-main-kh79aa9c021hhynw5ssw07s2rd88wt40
- worktree: /Users/stevengonsalvez/d/git/_worktrees/agents-in-a-box/kh79aa9c021hhynw5ssw07s2rd88wt40-reflect-memory
- note: fleet-worktree-start refused canonical clone because it had pre-existing untracked files; using isolated clean clone to avoid touching Stevie's dirty worktree.

## 2026-06-18 — Phase 1 implementation (issue #299)

### Scope
Implemented Phase 1 ONLY of the Reflect Memory GraphRAG substrate: Postgres
schema + FTS + graph tables + typed helper layer + tests + docs. pgvector
(Phase 2) deliberately not enabled.

### Key decisions
- **Package location:** `ainb-reflect-memory/` at repo root as a new uv
  workspace member (sibling to `reflect-kb/`). Matches repo-native Python
  layout. Root `pyproject.toml` `[tool.uv.workspace].members` extended; this is
  a virtual workspace root (no `[project]`), uv resolves it fine.
- **Extraction boundary:** package is self-contained (own pyproject/src/tests/
  migrations/docs), imports nothing from `reflect_kb` or AINB, exposes a small
  public surface. Lift-out = `git filter-repo` the dir later.
- **Dumb server / smart client:** all ranking + graph traversal live in SQL
  functions; no LLM, no embeddings, no answer synthesis server-side. `sql.py`
  is the only place query text is built; `store.py` is the trusted-worker path.
- **Tenancy:** `workspace_id` is the first bound param of every helper; unit
  test pins the invariant. RLS policies guard the direct PostgREST/JWT path via
  `current_workspace_id()` (GUC `app.current_workspace` → JWT claim → NULL =
  deny). Defense in depth.
- **Idempotency:** per-tenant `content_hash` (client-side SHA-256 of normalized
  content) + entity/edge upsert keys. Establishes the Phase 3 contract now.
- **Test tiers:** unit tests (pure Python, NO DB, always run) + integration
  tests (marked `integration`, auto-skip when no `DATABASE_URL`/Postgres
  reachable). Satisfies "no live Supabase creds required for unit tests".

### What was already present (prior session) vs added this session
- Prior: migration SQL, models/normalize/errors/sql/store, package pyproject,
  workspace wiring.
- Added this session: README.md, docs/setup.md, .env.example, scripts/seed.py,
  full test suite (tests/test_normalize.py, test_models.py, test_sql_builders.py,
  conftest.py, test_integration_store.py), root README tree entry, ruff-format
  pass over the package.

### Gates run (all green)
- Spun a throwaway local Postgres 14 (homebrew; Docker daemon was down) at
  /tmp socket, db `reflect_test`.
- Migration applies cleanly on fresh DB AND is re-runnable (idempotent) — exit 0.
- `pytest` WITH DB: 34 passed (23 unit + 11 integration).
- `pytest` WITHOUT DB: 23 passed, 11 skipped (clean auto-skip — no creds needed).
- Integration proves: FTS ranked hit + snippet, per-tenant idempotent insert,
  entity alias lookup, same-tenant-only neighborhood, cross-tenant edge FK
  rejection, tenant-scoped search, evidence pack assembly, RLS fail-closed +
  per-workspace isolation via unprivileged role + GUC.
- `seed.py` run twice → idempotent (2 mem / 2 ent / 1 edge), prints populated
  evidence pack.
- `ruff check` clean; `ruff format --check` clean; `uv lock --check` consistent;
  wheel builds. Build artifact `dist/` removed (not committed).

### Notes / blockers
- Docker daemon not running on host → used homebrew Postgres 14 for the live
  run instead. PG14 supports every feature the migration uses (generated
  columns, websearch_to_tsquery, gin_trgm_ops, recursive CTE). Supabase is
  Postgres 15+, so this is a conservative lower bound.
- **Security:** migration defines RLS policies → partner review required before
  merge. NOT self-merging. Branch pushed only.
- **Open question (documented in docs/setup.md):** exact Bitwarden item/
  collection name for the Supabase credentials — needs Stevie to confirm.
- Commit messages follow the global rule (no AI attribution / no Claude footer).


## 2026-06-18 — Goal framing correction
- Stevie clarified Supabase provisioning is prerequisite context, not the goal.
- Retrieved Supabase project metadata via Bitwarden access token without printing secrets: project `memory`, ref `gbirfbnyygbyztdodbvz`, region `eu-central-1`, Postgres 17, host `db.gbirfbnyygbyztdodbvz.supabase.co`; API keys available via Management API but not written to repo/logs.
- Wrote `/make-a-goal` artifact at `.agents/goals/reflect-memory-shared-graphrag-substrate.md` around Tank Option 2 outcome: Postgres shared GraphRAG substrate, client keeps all LLM brain, server stays dumb/searchable memory.

## 2026-06-18 — Real goal clarified: shared reflect store across machines + nano-graphrag backend decision
- Stevie reframed: do NOT replace the markdown file KB (stays source of truth). Replace ONLY the per-machine derived layer — the local vector store (SBERT+hnswlib) and nano-graphrag graph store — with a SHARED Postgres so every machine queries the same memory. Main goal = use the SAME reflect store across multiple machines.
- Q resolved: NO sqlite staging in the normal path (would create two vector stores → drift). Embed locally → upsert straight to Postgres (pgvector) = the one shared store. SQLite only as an optional OFFLINE write-buffer/read-cache that flushes on reconnect.
- Verified from installed source (`/Users/stevengonsalvez/.learnings/.venv/.../nano_graphrag`): nano-graphrag's graph is NOT our Phase 1 entities/edges, and there is no "postgres graphml" — graphml is just `NetworkXStorage`'s local serialization. nano-graphrag persists 4 PLUGGABLE stores: `BaseGraphStorage` (entity node{name,type,description,source_id} + rel edge{weight,description,order,source_id} + node_degree/get_node_edges/clustering/community_schema), `BaseVectorStorage` (TWO spaces: entity-vdb + chunk-vdb), `BaseKVStorage` (full_docs/text_chunks/community_reports/llm_cache). Defaults NetworkX/NanoVectorDB/JsonKV; ships `Neo4jStorage` as the external-DB reference backend. reflect-kb's `LearningsGraphEngine` wraps `GraphRAG` with defaults; modes naive/local/global.
- DECISION (Stevie, via AskUserQuestion): **Option A — implement Postgres-backed storage backends for nano-graphrag** (`PgGraphStorage`/`PgVectorStorage`/`PgKVStorage`, mirroring `Neo4jStorage`); nano-graphrag code stays UNCHANGED, only `*_storage_cls` swapped on the GraphRAG config. Phase 1 entities/edges is a lossy skeleton and must be extended to nano-graphrag's full model. Leiden clustering runs client-side, writes community_schema/reports back to PG.
- Wrote corrected goal at `.agents/goals/reflect-shared-store-across-machines.md` (supersedes the generic `reflect-memory-shared-graphrag-substrate.md`). 3 measurable gates: cross-machine parity, write-on-A answerable-on-B (local+global mode) idempotent w/ file KB untouched, server has zero LLM/embedding imports + RLS/pgvector tests green incl. a PgGraphStorage conformance test mirroring Neo4jStorage.

## 2026-06-18 — Built the shared nano-graphrag Postgres backend (goal execution)
- Verified from installed nano-graphrag source: storage is pluggable via
  `graph_storage_cls`/`vector_db_storage_cls`/`key_string_value_json_storage_cls`;
  ships `Neo4jStorage` as the external-DB reference. Embedding model:
  all-mpnet-base-v2, 768-d, unit-normalized.
- Shipped:
  - `supabase/migrations/0002_nanographrag_pgvector.sql` — pgvector + `ng_kv`,
    `ng_graph_nodes`, `ng_graph_edges`, `ng_vectors(vector(768)+hnsw)`,
    tenant-scoped, RLS mirroring 0001. Applies clean + idempotent on PG17.
  - `src/ainb_reflect_memory/nanographrag/` — `PgKVStorage`, `PgVectorStorage`
    (pgvector ANN; embeds via INJECTED embedding_func, never its own model),
    `PgGraphStorage` (subclasses `NetworkXStorage`; overrides ONLY load PG→nx /
    save nx→PG, so Leiden + community_schema reused verbatim, client-side, no
    graphml). `[nanographrag]` extra is light; nano-graphrag/graspologic/SBERT
    are client-provided peers (kept out of the lock to avoid the numba chain).
  - reflect-kb `LearningsGraphEngine`: opt-in PG backend when REFLECT_PG_DSN
    (or DATABASE_URL) + REFLECT_WORKSPACE_ID set; default local behavior intact.
- Validation env: Docker daemon down → used local Homebrew PG17 + `brew install
  pgvector` (vector 0.8.3, matches Supabase). nano-graphrag/numpy/networkx came
  from `~/.learnings/.venv` (py3.11); added psycopg+pytest to it (additive, did
  NOT touch the reflect CLI behavior). graspologic absent → shimmed with
  networkx Louvain (same trick reflect-kb uses) for the e2e clustering.
- Gates (all green):
  - no-DB tier (uv py3.13): 30 passed incl. `test_server_is_dumb` (static scan:
    adapters import no LLM/embedding provider; embed only via injected func).
  - integration tier on PG17: 20 passed = 11 Phase-1 + 9 nano-graphrag. The 9:
    KV/vector/graph contract, graph round-trip machine-A→machine-B, idempotent
    re-upsert, community_schema, tenant isolation, ng RLS fail-closed, and the
    CAPSTONE — real `GraphRAG.insert` on A → fresh B answers local+naive from PG
    with NO graphml written + idempotent.
  - `scripts/demo_cross_machine.py`: machine B answers machine A's data, 0 graphml.
  - ruff check + format clean; `uv lock --check` consistent; wheel builds.
- Bug caught+fixed by tests: `PgKVStorage` needed `@dataclass` (BaseKVStorage
  defines no `__post_init__`, so the subclass hook wasn't auto-called).
- Known follow-ups (NOT faked): real cross-host WAN/concurrent-writer merge;
  Phase-4 community summary versioning; realism proxy — pipeline tests use a
  deterministic fake embedding + canned LLM + networkx-Louvain shim (legit since
  LLM/embeds are client-side & swappable; real SBERT output not exercised).

## 2026-06-18 — Independent review + harness gap closure
- Spawned an INDEPENDENT reviewer agent (reviewer-never-authored): it re-ran
  every gate from committed code on PG17. Verdict: PASS, architecture sound, but
  criteria 1 & 2 PARTIAL — 4 harness items had been substituted with weaker
  proxies. Closed all 4 (test-only, no source change):
  1. global-mode query answered on machine B from PG community reports
     (added canned global-map "points" LLM response + assertion).
  2. cross-machine top-k id+score PARITY between two isolated instances.
  3. runtime "server stays dumb": poison openai/anthropic/cohere in sys.modules
     to raise on any access; storage path still works (dynamic complement to the
     static scan).
  4. assert the markdown file KB is byte-identical after a PG-backed insert.
- Final: 52 tests green — 30 no-DB (always-run) + 22 integration on PG17+pgvector
  (11 Phase-1 + 11 nano-graphrag). ruff clean, lock consistent, wheel builds,
  migrations idempotent, demo cross-machine transcript reproducible.
- All 3 success criteria now PASS (no proxies on 1 & 2). RLS migrations
  (0001/0002) still require partner review before merge — branch pushed, not
  merged.

## 2026-06-18 — Follow-ups: explainer, SQLite correction, regression suite
- Published an /explain-to-me + /fireworks-tech-graph architecture explainer to
  the here.now custom domain: https://explainers.stevengonsalvez.com/reflect-shared-memory/
  (Link mount via /api/v1/links, slug sacred-ether-q9xq, token used in-session
  only — NOT stored). Future explainers = new path under the same domain.
- CORRECTION (Stevie caught it twice): I wrongly said reflect has no SQLite. It
  DOES — (a) QMD `~/.cache/qmd/index.sqlite` = the BM25 LEXICAL arm recall.py
  RRF-fuses with the semantic arm; (b) `~/.reflect/reflect.db` = SQLite state/
  telemetry (4.1.0 tables). My PG change moved ONLY the nano-graphrag semantic
  arm (was hnswlib+graphml); QMD sqlite + reflect.db are untouched. QMD is a
  COMPLEMENT to the vector lookup (hybrid lex+vec via RRF), not an alternate.
  Fixed the d1 diagram + added a "Where SQLite lives" section to the explainer.
- 4.1.0 = the "57 ports" recall upgrade (57 = literal proof-file count; Waves
  1-4). Of the 57, exactly ONE (R1 graph-arm) routes through the PG backend; the
  other 56 are recall-layer + backend-agnostic (verified).
- Regression suite (Stevie chose full 13-type, both backends). Shipped Tier A:
  - tests/nanographrag/test_backend_parity.py — local vs PG return the SAME
    evidence set for naive/local/global over a fixed corpus (+ only local writes
    graphml). NB: tie-break order of equal-scoring items differs between
    NanoVectorDB and pgvector — expected, reranked downstream; parity asserted at
    evidence-set level.
    tests/test_recall_backend_independence.py (no DB) — 56 recall-layer ports
    have zero backend coupling.
  - docs/regression-suite.md — 13-lookup + 57-port manifest; Tier B full-stack
    golden (real model + qmd) scaffolded for CI.
  Final: 56 tests green (32 no-DB + 24 integration on PG17+pgvector).
- SQLite local-only: Stevie chose "default file-mode is enough" — local-only
  already ships (hnswlib+graphml + QMD sqlite when no PG env); no new sqlite
  backend built.
- Added psycopg+pytest to ~/.learnings/.venv (additive) for validation; throwaway
  PG17+pgvector spun up/torn down per run.
- OPEN: "shotclubhouse" password rule still unclarified — nothing password-
  protected yet (the reflect explainer is public).
