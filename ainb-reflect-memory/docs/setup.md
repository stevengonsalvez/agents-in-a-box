# Setup — ainb-reflect-memory

Operational guide: secrets, database setup, migration, seed, and tests. For the
architecture and API, see the [README](../README.md).

---

## 1. Secrets (Bitwarden)

**No secrets live in this repo.** All credentials come from Bitwarden and land
in a local `.env` that git ignores. Copy the template and fill it:

```bash
cp .env.example .env   # .env is git-ignored; never commit it
```

### Required values (placeholders in `.env.example`)

| Env var                     | What it is                                   | Needed for                          | Exposure rule                                   |
| --------------------------- | -------------------------------------------- | ----------------------------------- | ----------------------------------------------- |
| `DATABASE_URL`              | Postgres connection string                   | migration, seed, integration tests  | server/worker only                              |
| `SUPABASE_URL`              | `https://<ref>.supabase.co`                  | direct client path (Phase 2+)       | safe to expose                                  |
| `SUPABASE_ANON_KEY`         | public anon key                              | direct browser/PostgREST reads      | safe to expose (RLS still applies)              |
| `SUPABASE_SERVICE_ROLE_KEY` | service-role key (**bypasses RLS**)          | migrations / trusted workers only   | **NEVER** to browser/client; NEVER commit       |

### Bitwarden item

Supabase credentials live in Bitwarden item metadata:

- **Item:** `WOLOLO-SUPABASE`
- **Known keys:** `DATABASE_PASSWORD`, `SUPABASE_URL`, `SUPABASE_ANON_KEY`, `SUPABASE_SECRET_KEY`

Do **not** print secret values into logs, Discord, PRs, or committed files. Load values into the shell session only, then map them to the local env names below. `SUPABASE_SECRET_KEY` is the service-role/secret key and should be treated like `SUPABASE_SERVICE_ROLE_KEY`.

Pull safely with the Bitwarden CLI (values go only into ignored `.env`):

```bash
bw get item "WOLOLO-SUPABASE" \
  | jq -r '.notes | split("\n")[] | select(test("^export ")) | sub("^export "; "")' \
  >> .env
```

If the connection string is not stored as `DATABASE_URL`, build it from the Supabase connection string plus `DATABASE_PASSWORD` in the local shell only.

Unit tests need **none** of these — they run with no database and no
credentials (see §5).

---

## 2. Create the Supabase project

1. Create a project in the Supabase dashboard.
2. Copy **Project Settings → API** → `URL`, `anon` key, `service_role` key into
   the matching Bitwarden fields.
3. Copy **Project Settings → Database → Connection string** into `DATABASE_URL`.
   Use the **direct** connection string for migrations; the pooler/session
   string is fine for app traffic.

Phase 1 needs only `pgcrypto` and `pg_trgm`; both are available on Supabase by
default and are enabled by the migration itself. `vector` (pgvector) is **not**
required until Phase 2.

---

## 3. Apply the migration

The migration is plain SQL and re-runnable (`IF NOT EXISTS` / `CREATE OR
REPLACE` / `DROP … IF EXISTS`).

**Option A — psql (works anywhere):**

```bash
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
  -f supabase/migrations/0001_reflect_memory_phase1.sql
```

**Option B — Supabase CLI** (if you use it for this project):

```bash
supabase db push          # applies supabase/migrations/*.sql
```

> ⚠️ **This migration defines Row-Level Security policies.** Per the issue's
> security requirements, RLS/security-sensitive migrations require **partner
> review before merge — do not self-merge.**

---

## 4. Seed (smoke-test a live database)

```bash
export DATABASE_URL=...                       # from .env / Bitwarden
uv run --extra pg python scripts/seed.py      # default demo workspace
uv run --extra pg python scripts/seed.py <WORKSPACE_UUID>   # specific tenant
```

It inserts a couple of memory items, two entities, and an edge, then prints an
evidence pack. It is idempotent — re-running creates no duplicates.

---

## 5. Tests

### Unit tests — no database, no credentials (the default CI path)

```bash
uv sync --extra dev
uv run --extra dev pytest -m "not integration"
```

These cover normalization/dedupe hashing, model validation, and the SQL-builder
security invariants (tenant scoping, no value interpolation).

### Integration tests — require a Postgres (auto-skip otherwise)

Set `DATABASE_URL` (or `REFLECT_TEST_DATABASE_URL`) to a **throwaway** database,
then:

```bash
uv run --extra dev pytest -m integration   # or just `pytest` to run both tiers
```

If neither var is set, or the database is unreachable, every integration test
**skips** cleanly — they never fail for lack of credentials.

#### Spin a throwaway local Postgres

**Docker:**

```bash
docker run -d --name reflect-pg -e POSTGRES_PASSWORD=reflect_test \
  -e POSTGRES_DB=reflect_test -p 55432:5432 postgres:16-alpine
export DATABASE_URL='postgresql://postgres:reflect_test@localhost:55432/reflect_test'
# ... run tests ...
docker rm -f reflect-pg
```

**Homebrew Postgres (no Docker daemon):**

```bash
export PGDATA=/tmp/reflect-pgdata
initdb -D "$PGDATA" -U postgres --auth=trust
pg_ctl -D "$PGDATA" -o "-p 55432 -k /tmp -c listen_addresses=''" -l /tmp/pg.log start
psql -h /tmp -p 55432 -U postgres -d postgres -c 'CREATE DATABASE reflect_test;'
export DATABASE_URL='postgresql://postgres@/reflect_test?host=/tmp&port=55432'
# ... run tests ...  then:  pg_ctl -D "$PGDATA" stop && rm -rf "$PGDATA"
```

The integration fixtures apply the migration automatically before the first
test and truncate tables between tests.

---

## 6. What's covered (Phase 1 acceptance)

- migration applies cleanly on a fresh database, and is re-runnable
- seed inserts memory/entity/edge rows (idempotent)
- FTS search returns ranked rows with highlighted snippets
- graph neighborhood returns only same-tenant edges; cross-tenant edges are
  physically impossible (composite FK)
- duplicate ingestion is idempotent (per-tenant content hash)
- tenant isolation on the trusted path; RLS fail-closed on the direct path

See the [phase roadmap](../README.md#phase-roadmap) for Phases 4–5.

---

## 7. Shared nano-graphrag backend (Phase 2/3)

Makes reflect's nano-graphrag use this Postgres as its shared vector + graph +
community store, so the same memory is queryable from every machine. See
[README → Shared nano-graphrag backend](../README.md#shared-nano-graphrag-backend).

### Apply the Phase 2 migration (needs pgvector)

```bash
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
  -f supabase/migrations/0002_nanographrag_pgvector.sql
```

`pgvector` is enabled by the migration (`create extension vector`). It is
available on Supabase by default; locally, install the `pgvector` package for
your Postgres (e.g. `brew install pgvector`, then it lands in PostgreSQL 17's
extension dir). The embedding column is `vector(768)` to match all-mpnet-base-v2.

> ⚠️ RLS policies again — **partner review before merge, no self-merge.**

### Enable the backend in reflect-kb

`LearningsGraphEngine` switches to the shared backend when BOTH are set
(otherwise it keeps local-file behavior). The trigger is `REFLECT_PG_DSN`
**only** — the generic `DATABASE_URL` is deliberately NOT a trigger (it usually
points at an unrelated DB):

```bash
export REFLECT_PG_DSN="postgresql://USER:PASS@HOST:5432/DBNAME"
export REFLECT_WORKSPACE_ID="<workspace-uuid>"
```

**DSN role:** for writes (ingest/index) the DSN must authenticate as
`service_role` / the table owner — the trusted-worker path. The `authenticated`
role is granted **read-only** (search/lookup), and direct writes go through the
worker, not PostgREST. The adapter sets the `app.current_workspace` GUC on
connect so reads work under RLS for any role; the resolver treats a signed JWT
claim as authoritative over that GUC.

The `reflect` client still needs nano-graphrag + its embedding stack (reflect-kb
`[graph]` extra). The `ainb-reflect-memory` adapters need `[nanographrag]` (+ `[pg]`).

### Cross-machine demo + tests

```bash
# end-to-end: machine A writes, machine B reads — from Postgres, no shared files
PYTHONPATH=src DATABASE_URL=... python scripts/demo_cross_machine.py

# adapter conformance + cross-machine + RLS + full-pipeline tests
#   (need nano-graphrag + networkx + numpy + psycopg + a pgvector Postgres)
PYTHONPATH=src DATABASE_URL=... pytest -m integration tests/nanographrag

# the always-on "server stays dumb" scan needs no DB and no nano-graphrag:
pytest -m "not integration" tests/test_server_is_dumb.py
```
