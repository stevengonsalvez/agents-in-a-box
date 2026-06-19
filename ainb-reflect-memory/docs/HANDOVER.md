# Handover — Reflect shared Postgres GraphRAG memory

**Branch:** `freeman/kh79aa9c021hhynw5ssw07s2rd88wt40-reflect-memory`
**PR:** [#302](https://github.com/stevengonsalvez/agents-in-a-box/pull/302) — OPEN, ~36 commits
**Status:** code complete, all gates green, **awaiting human RLS review before merge**

---

## TL;DR

`ainb-reflect-memory` is a shared Postgres (Supabase) GraphRAG memory substrate.
reflect's nano-graphrag runs **unchanged** against it via storage-class
injection, so the same vector + graph + community store is queryable from every
machine. The markdown file KB stays the local source of truth; all
LLM/embedding/Leiden clustering stays client-side; the DB only stores + scopes +
runs ANN/graph reads.

Opt-in: set `REFLECT_PG_DSN` + `REFLECT_WORKSPACE_ID` → reflect uses Postgres.
Unset → original local-file behavior (hnswlib + graphml), unchanged.

---

## What's in the PR

```
ainb-reflect-memory/
  supabase/migrations/0001_…phase1.sql   memory_items/entities/edges, FTS, RLS, tenant resolver
  supabase/migrations/0002_…pgvector.sql pgvector + ng_kv/ng_graph_nodes/ng_graph_edges/ng_vectors
  src/ainb_reflect_memory/
    models.py normalize.py sql.py store.py errors.py   Phase-1 typed MemoryStore
    nanographrag/{_conn,kv,vectors,graph,__init__}.py  nano-graphrag PG storage backends
  tests/  (unit no-DB + integration nanographrag/)
  scripts/{seed,demo_cross_machine}.py
  docs/{setup,regression-suite,HANDOVER}.md
reflect-kb/src/reflect_kb/cli/graph_engine.py   opt-in PG backend wiring (only changed reflect-kb file)
pyproject.toml + uv.lock                         workspace member
```

Three adapters implement nano-graphrag's pluggable storage (the way it ships
`Neo4jStorage`): `PgKVStorage`, `PgVectorStorage` (pgvector ANN), `PgGraphStorage`
(subclasses `NetworkXStorage` — Leiden + community_schema reused; no graphml).

---

## Tests — 58 green

| Tier | How to run | Needs |
|------|-----------|-------|
| no-DB (32) | `cd ainb-reflect-memory && uv run --extra dev pytest -m "not integration"` | nothing |
| integration (26) | `DATABASE_URL=… PYTHONPATH=src <py3.11> -m pytest -m integration` | Postgres + pgvector, nano-graphrag stack |

Integration covers: storage-contract conformance, machine-A→fresh-machine-B
round-trip, idempotency, community_schema, tenant isolation, RLS fail-closed,
**JWT-wins-over-GUC**, cross-machine parity, **LOCAL-vs-PG evidence parity (fake
AND real all-mpnet model)**, full GraphRAG e2e (local/naive/global), migrations
clean+idempotent.

### Local env recipe (validated on this machine)

- Postgres: `brew install postgresql@17 pgvector`; throwaway cluster:
  `initdb -D /tmp/pg -U postgres --auth=trust && pg_ctl -D /tmp/pg -o "-p 55433 -k /tmp" start`,
  `createdb`, apply both migrations.
- Python stack for integration: `~/.learnings/.venv` (py3.11) has nano-graphrag +
  sentence-transformers + all-mpnet model cached. psycopg + pytest were added to
  it. graspologic is absent → tests inline a networkx-Louvain shim.
- DSN used: `postgresql://postgres@/reflect_test?host=/tmp&port=55433`.

---

## Security review (done) — ⚠ one human gate left

Ran `/code-review high` + `supabase-security-reviewer` (reproduced findings
against a real PG). 7 findings, **all fixed** (see PR comment + JOURNAL):

| Sev | Was | Fix |
|-----|-----|-----|
| HIGH | resolver read GUC before JWT (tenant trust-inversion) | JWT-authoritative resolver + regression test |
| MED | adapter never set tenant GUC | sets `app.current_workspace` on connect |
| MED | `authenticated` had full CRUD | read-only; writes via `service_role` |
| MED | `DATABASE_URL` auto-enabled PG mode | trigger is `REFLECT_PG_DSN` only |
| MED | graph save left stale rows | atomic full-replace |
| LOW×2 | PUBLIC grants, no search_path, depth, NaN vectors, kv projection | revoke PUBLIC, pin search_path, clamp ≤5, guards |

> **DO NOT self-merge.** The RLS migrations need a human partner review. The
> automated pass is not a substitute. This is the only thing blocking merge.

---

## How to resume / re-link the worktree

A prior session's worktree gitdir lived under `/tmp` and was wiped by a machine
restart — origin has 100% of the work (tip = the PR head). To work locally again:

```bash
cd <this worktree>
git init -q
git remote add origin https://github.com/stevengonsalvez/agents-in-a-box.git
git fetch -q origin freeman/kh79aa9c021hhynw5ssw07s2rd88wt40-reflect-memory
git checkout -B freeman/kh79aa9c021hhynw5ssw07s2rd88wt40-reflect-memory FETCH_HEAD
```

---

## Open items

- **[blocking]** Human RLS review of `0001`/`0002`, then merge #302.
- Tier B full golden (all 13 lookups incl. qmd/BM25 + each 4.1.0 port's
  Acceptance) — scaffolded for CI in `docs/regression-suite.md`; the existing 57
  behavioral proofs + shipped Tier A/real-model parity cover the core.
- `explainer`: live at https://explainers.stevengonsalvez.com/reflect-shared-memory/
  (gitignored — published to here.now, not in the repo).
- SQLite single-file backend: not built (default file-mode is the local mode).
- Extraction: package is self-contained for lift-out to `ainb-reflect-memory` repo later.
