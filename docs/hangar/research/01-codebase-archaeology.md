# Multica Codebase Archaeological Report
**Repo**: github.com/multica-ai/multica  
**Survey date**: 2026-05-22  
**Commit**: b9d4b7f (local shallow clone state)  
**Purpose of survey**: downstream replication into agents-in-a-box (ainb)

---

## 1. Repo Topology

```
multica/
├── server/                  Go monolith (binary + CLI combined)
│   ├── cmd/
│   │   ├── multica/         CLI entry point (cobra, combined binary)
│   │   ├── server/          HTTP server + router + main.go
│   │   ├── migrate/         standalone migration runner
│   │   └── backfill_task_usage_hourly/
│   ├── internal/            private business logic
│   │   ├── agenttmpl/       curated agent template catalog (embedded JSON)
│   │   ├── analytics/       PostHog async batch client
│   │   ├── auth/            JWT + PAT + daemon-token generation & caching
│   │   ├── cli/             CLI sub-commands (cobra) + shared HTTP client
│   │   ├── cloudruntime/    HTTP proxy to SaaS-only fleet service
│   │   ├── daemon/          local agent daemon (core logic)
│   │   │   ├── execenv/     per-task isolated execution environment
│   │   │   └── repocache/   git bare-clone cache + worktree creation
│   │   ├── daemonws/        WebSocket hub for daemon push (wakeup hints)
│   │   ├── events/          in-process synchronous pub/sub bus
│   │   ├── handler/         HTTP request handlers (Chi router)
│   │   ├── issueguard/      duplicate-issue detection (normalised title lock)
│   │   ├── logger/          structured slog setup
│   │   ├── mention/         MUL-NNN identifier → mention:// link expansion
│   │   ├── metrics/         Prometheus HTTP metrics middleware
│   │   ├── middleware/       auth + workspace-scoping Chi middleware
│   │   ├── migrations/       migration runner
│   │   ├── realtime/        WebSocket hub for browser/desktop clients
│   │   ├── service/         TaskService + AutopilotService + EmailService
│   │   ├── storage/         S3 + local disk abstraction (uploads)
│   │   └── util/            UUID helpers, etc.
│   ├── pkg/
│   │   ├── agent/           provider metadata (names, versions, LaunchHeader)
│   │   ├── db/generated/    sqlc-generated query code
│   │   ├── protocol/        WebSocket event-type constants + daemon msg types
│   │   └── redact/          PII redaction helpers
│   └── migrations/          107 numbered PLpgSQL migration files (up+down)
├── apps/
│   ├── web/                 Next.js 16, App Router
│   ├── desktop/             Electron (electron-vite), bundles CLI binary
│   ├── mobile/              Expo / React Native (iOS)
│   └── docs/                Docusaurus / MDX docs site
├── packages/
│   ├── core/                headless TS (stores, hooks, API client, types)
│   ├── ui/                  shadcn/Base UI atomic components
│   ├── views/               shared business pages/components (no framework)
│   └── tsconfig/            shared TS config
├── e2e/                     Playwright end-to-end tests
├── .agents/skills/          one installed skill (web-design-guidelines)
├── skills-lock.json         pinned skill hashes (frontend: 4 skills)
├── docker-compose.selfhost.yml
├── CLAUDE.md                27 KB agent instructions
├── CLI_AND_DAEMON.md        26 KB CLI reference
└── AGENTS.md                brief agent quick-reference
```

**Key size signal**: 107 migration files in 4 months. Rapid schema iteration at ~1 migration/day pace. This is a live, fast-moving product.

---

## 2. The Daemon Model

### What the daemon is

The daemon is a **local process running on a developer's machine** that bridges the Multica server to real AI CLI tools installed on that machine. It is not a container, not a sandbox, not a VM — it runs native processes directly. The daemon binary is the same `multica` CLI binary; `multica daemon start` enters the daemon mode.

### Lifecycle

```
multica login
    │  OAuth browser flow → 90-day PAT stored in ~/.multica/
    ▼
multica daemon start
    │
    ├─ Detect agent CLIs on PATH (claude, codex, copilot, gemini, …)
    ├─ For each CLI × each watched workspace → POST /api/daemon/register
    │    registers an agent_runtime row: (workspace_id, daemon_id, provider)
    │
    ├─ Open WebSocket to /api/daemon/ws (daemonws.Hub)
    │    receives push wakeup hints: "task available for runtime X"
    │
    ├─ Poll loop (default 3s): POST /api/daemon/runtimes/{id}/tasks/claim
    │    server returns a Task payload or 204 (empty claim cached in Redis)
    │
    ├─ On task claimed:
    │    ├─ execenv.Prepare() → creates ~/multica_workspaces/{task_id}/
    │    │    writes CLAUDE.md / .agent_context/issue_context.md / skills
    │    │    clones or reuses git repo via repocache (bare clone + worktree)
    │    ├─ POST /api/daemon/tasks/{id}/start
    │    ├─ Spawn agent CLI as subprocess (cwd = workdir, env injected)
    │    ├─ Stream JSONL output → POST /api/daemon/tasks/{id}/messages
    │    ├─ POST /api/daemon/tasks/{id}/progress (intermediate)
    │    └─ POST /api/daemon/tasks/{id}/complete OR /fail
    │
    ├─ Heartbeat loop (default 15s): POST /api/daemon/heartbeat
    │    server updates runtime.last_seen_at
    │
    └─ On shutdown: POST /api/daemon/deregister (marks runtimes offline)
```

**Config store**: `~/.multica/` (per profile: `~/.multica/profiles/<name>/`). Multi-profile support enables one machine to serve multiple server instances simultaneously.

**Workspace garbage collection**: background goroutine scans `~/multica_workspaces/`. Three TTLs: full cleanup (done/cancelled issues, 24h), orphan cleanup (no `.gc_meta.json`, 72h), artifact-only cleanup (node_modules etc., 12h). Disk footprint is bounded automatically.

**Auto-update**: daemon checks for new CLI releases, downloads, and restarts itself (waits for in-flight tasks to drain first via `pauseClaims` + `claimsInFlight` atomic barrier).

### Key files
- `server/internal/daemon/daemon.go` — `Daemon` struct, poll loop, task dispatch
- `server/internal/daemon/types.go` — `Task`, `TaskResult`, `AgentData`, `SkillData`
- `server/internal/daemon/prompt.go` — task prompt construction (5 task kinds)
- `server/internal/daemon/execenv/execenv.go` — execution environment preparation
- `server/internal/daemon/execenv/runtime_config.go` — per-provider CLAUDE.md / AGENTS.md injection
- `server/internal/daemon/execenv/context.go` — `.agent_context/issue_context.md` rendering
- `server/internal/daemon/repocache/` — git bare-clone cache + worktree management

---

## 3. Agent Runtime

### Runtime kinds

| Kind | Where | How |
|------|-------|-----|
| **Local** | Developer's machine | daemon spawns native CLI subprocess |
| **Cloud** | SaaS fleet (MULTICA_CLOUD_FLEET_URL) | HTTP proxy via `cloudruntime.Client` |

Cloud runtime is **SaaS-only** — `cloudRuntimeFleetURL` is empty in self-hosted deployments, all cloud-runtime endpoints return 503. The open-source repo ships the proxy interface but not the fleet.

### Local runtime execution details

**Sandbox**: none. The agent CLI runs as the same OS user with full filesystem access. The isolation boundary is the `workdir` directory, but it is advisory not enforced.

**Execution environment per task**:
```
~/multica_workspaces/{task_id_short}/
├── workdir/                    CWD passed to agent
│   ├── .claude/
│   │   ├── CLAUDE.md           injected instructions + identity
│   │   └── skills/{name}/      skill files (Claude native discovery)
│   ├── .agent_context/
│   │   ├── issue_context.md    structured task brief
│   │   └── skills/{name}/      fallback for providers without native skill dir
│   └── <git worktree>          cloned repo (optional, via `multica repo checkout`)
├── output/                     agent output files
├── logs/                       task logs
└── .gc_meta.json               GC metadata (issue_id, task_id, created_at)
```

**Provider-specific skill paths** (`execenv/context.go:writeContextFiles`):
- Claude: `.claude/skills/{name}/SKILL.md`
- Copilot: `.github/skills/{name}/SKILL.md`
- OpenCode: `.opencode/skills/{name}/SKILL.md`
- OpenClaw: `skills/{name}/SKILL.md` + synthesized `openclaw-config.json`
- Pi: `.pi/skills/{name}/SKILL.md`
- Cursor: `.cursor/skills/{name}/SKILL.md`
- Kimi: `.kimi/skills/{name}/SKILL.md`
- Kiro: `.kiro/skills/{name}/SKILL.md`
- Gemini/Hermes/default: `.agent_context/skills/{name}/SKILL.md`

**Runtime config injection** (`execenv/runtime_config.go`): the daemon writes a `CLAUDE.md` (or provider-equivalent) into `workdir` containing:
1. Agent identity/persona (`AgentInstructions`)
2. Workspace context (workspace-level system prompt)
3. Requesting user profile
4. Task brief (issue_context.md reference or inline)
5. Skill list + install instructions per provider
6. Mention semantics (side-effect warnings)
7. Sub-issue creation rules
8. Output requirements (mandatory comment posting)

**Session resumption**: `PriorSessionID` + `PriorWorkDir` fields on `Task` allow Claude session continuity across retries. Daemon calls `POST /tasks/{id}/session` to persist session_id + work_dir immediately after agent emits first system message, enabling crash recovery.

**Retry logic** (`migrations/055`): `agent_task_queue` carries `attempt`, `max_attempts` (default 2), `parent_task_id`, `failure_reason`. `HandleFailedTasks` auto-spawns child tasks on `agent_error` / `runtime_recovery` failures.

**Supported providers** (11): claude, codex, copilot, opencode, openclaw, hermes, gemini, pi, cursor-agent, kimi, kiro-cli.

---

## 4. Control Plane

### Task dispatch flow

```
User assigns issue to agent  (web/desktop/CLI)
        │
        ▼
handler/issue.go → CreateIssue / UpdateIssue
  service.TaskService.MaybeEnqueueTask()
        │  INSERT INTO agent_task_queue (status='queued')
        │  events.Bus.Publish("task:queued")
        │
        ▼
events/bus.go → realtime fanout
  realtime.Hub.Broadcast("workspace:{id}", "task:queued")   ← web/desktop
  daemonws.Hub.NotifyRuntime(runtimeID, taskID)             ← daemon push
        │
        ▼
Daemon poll OR WebSocket wakeup hint
  POST /api/daemon/runtimes/{id}/tasks/claim
  → ClaimTaskByRuntime (handler/task_lifecycle.go)
     UPDATE agent_task_queue SET status='dispatched', lease_expires_at=...
     SELECT task + agent data + skills + repos + workspace context
     RETURN Task JSON to daemon
        │
        ▼
Daemon executes → POST /tasks/{id}/start → POST /tasks/{id}/progress
  → POST /tasks/{id}/complete OR /fail
     service.TaskService.HandleCompletedTask / HandleFailedTasks
     events.Bus.Publish("task:completed" / "task:failed")
     → realtime broadcast → web/desktop update
```

### Key control-plane modules

**`service.TaskService`** (`server/internal/service/task.go`):
- `MaybeEnqueueTask` — creates task row, publishes events
- `HandleCompletedTask` — updates issue status, creates system comment, fires analytics
- `HandleFailedTasks` — decides retry vs permanent failure, fires inbox notifications
- `EmptyClaimCache` — Redis-backed "no queued task" fast-path to avoid hot Postgres scan on every 3s poll per runtime

**`events.Bus`** (`server/internal/events/bus.go`): in-process synchronous pub/sub. Handlers run in registration order with panic recovery. Events carry `TaskID` + `ChatSessionID` as scope hints for the realtime fanout layer to route to task-scoped WebSocket rooms.

**`realtime.Hub`** (`server/internal/realtime/hub.go`): gorilla/websocket hub for browser/desktop clients. JWT or PAT auth. ALLOWED_ORIGINS env-configurable. Optional Redis relay (`realtime/redis_relay.go`) for multi-node deployments — events fan out across pods via sharded Redis streams.

**`daemonws.Hub`** (`server/internal/daemonws/hub.go`): separate WebSocket hub for daemon connections. Carries push wakeup hints (task available), model-list requests, local-skill import requests, CLI update triggers. Client-side dedup ring buffer (128 event IDs) prevents duplicate delivery on reconnect.

**`HeartbeatScheduler`** (`handler/heartbeat_scheduler.go`): batched goroutine that coalesces rapid daemon heartbeat writes into periodic DB flushes, preventing write storm on large deployments.

**`RuntimeSweeper`** (`cmd/server/runtime_sweeper.go`): background goroutine that marks runtimes offline when `last_seen_at` exceeds threshold. Also fails stuck tasks assigned to dead runtimes.

### Task kinds (5)
1. **on_assign** — standard issue assignment; agent reads issue, does work, comments result
2. **comment_trigger** — `@agent_name` mention in a comment triggers a new task run
3. **chat** — synchronous chat session (no issue required)
4. **autopilot** — scheduled/webhook-fired automation
5. **quick_create** — agent translates a natural-language prompt into `multica issue create`

---

## 5. Data Plane

### Schema themes (107 migrations)

**Core entities** (migration 001):
- `user`, `workspace`, `member` (role: owner/admin/member)
- `agent` (runtime_mode: local/cloud; status: idle/working/blocked/error/offline)
- `issue` (7-state status machine; polymorphic assignee_type: member/agent/squad)
- `comment` (types: comment/status_change/progress_update/system)
- `inbox_item` (severity: action_required/attention/info)
- `agent_task_queue` (status: queued→dispatched→running→completed/failed/cancelled)
- `daemon_connection`, `activity_log`

**Runtime separation** (migration 004): `agent_runtime` table decouples agent identity from runtime registration. One agent can have multiple runtimes (different devices, different providers).

**Skills** (migration 008): `skill` + `skill_file` + `agent_skill` junction. Skills stored as DB rows with `content` (SKILL.md text) + supporting files. Agents have a many-to-many skill set.

**Chat** (migration 033): `chat_session` + `chat_message`. `issue_id` on `agent_task_queue` made nullable to support chat tasks.

**Projects** (migration 034): `project` groups issues (sprint/epic/workstream). Optional `lead` (member or agent).

**Autopilot** (migration 042): `autopilot` + `autopilot_trigger` (schedule/webhook/api) + `autopilot_run`. Concurrency policy: skip/queue/replace.

**Squads** (migration 084): `squad` + `squad_member`. A squad has a `leader_id` (agent). Squads are first-class issue assignees (`assignee_type = 'squad'`). When assigned, platform dispatches to leader agent who can delegate.

**GitHub integration** (migration 079): `github_installation` + `github_pull_request` + `issue_pull_request`. GitHub App webhook-driven PR mirroring + issue↔PR link table.

**Issue metadata** (migration 105): `metadata JSONB` column on `issue`. KV map for agent pipeline state (pr_number, pipeline_status, etc.). GIN index for `@>` containment queries. 50 key cap, 8KB blob limit.

**Usage analytics** (migrations 032, 073, 101): `task_usage` (raw per-model token counts per task) → `task_usage_hourly` rollup (UTC hourly buckets, pre-grouped by workspace/agent/project/model). Dirty-queue pattern for invalidation. Driven by pg_cron (migration 076).

### Storage
- **Postgres 17 + pgvector** (pgvector extension enabled but not yet prominently used — future vector search)
- **Redis** (optional): realtime relay shards, PAT cache, DaemonToken cache, MembershipCache, EmptyClaimCache, rate limiter
- **S3 / CloudFront** (optional): file uploads and attachments
- **Local disk** (`/app/data/uploads`): fallback upload storage

---

## 6. Integration Surface

### GitHub App
- `handler/github.go` — GitHub App OAuth connect, webhook handler
- `migrations/079` — `github_installation`, `github_pull_request`, `issue_pull_request`
- Webhook events: `pull_request`, `check_suite`, `check_run` → update PR state, link to issues
- CI check aggregation: `checks_passed/failed/pending` per PR head SHA
- Merge state mirroring: `mergeable_state` (clean/dirty/blocked/behind)
- Admin-only: `installation_id` field gated by role in list endpoint

### Linear / Jira / others
**Not present**. GitHub is the only external issue tracker integrated. No Linear, Jira, or webhook-to-issue bridge exists in this codebase (beyond the autopilot webhook trigger).

### Autopilot webhooks
- `handler/autopilot_webhook.go` — receives `POST /api/webhooks/autopilots/{token}` 
- Token-authenticated (per-trigger `webhook_token`), rate-limited per IP + per workspace
- Fires `autopilot_trigger` of kind `webhook` → creates autopilot run → enqueues task
- `migrations/091` — `webhook_deliveries` table for deduplication

### Mention system
- `internal/mention/expand.go` — scans markdown for `PREFIX-NNN` patterns (workspace-specific prefix), expands to `[MUL-123](mention://issue/<uuid>)` links
- Agent mentions `[@Name](mention://agent/<id>)` trigger new task enqueue (side-effecting)
- Member mentions `[@Name](mention://member/<id>)` send inbox notifications
- Anti-loop guidance injected into every agent brief

---

## 7. Auth & Multi-Tenancy

### Auth model
- **Email OTP**: 6-digit verification codes (Resend or SMTP); no passwords
- **Google OAuth**: optional
- **JWT**: session cookies (httpOnly, Secure on HTTPS origins, Domain configurable)
- **Personal Access Tokens (PATs)**: `mul_` prefixed, SHA-256 hashed in DB, 90-day default, Redis-cached
- **Daemon tokens**: `mdt_` prefixed, separate cache, scoped to daemon identity
- **Signup controls**: `ALLOW_SIGNUP`, `ALLOWED_EMAIL_DOMAINS`, `ALLOWED_EMAILS`

### Multi-tenancy shape
**Multi-user, multi-workspace SaaS**. Every DB query filters by `workspace_id`. Membership check gates access. `X-Workspace-ID` header routes requests.

Roles: owner / admin / member. Key access differences:
- `github installation_id` admin-only in responses
- Runtime visibility: private (owner + admins only) vs public (any member)
- Workspace settings mutations admin-gated

Self-hosted deployments are **single-tenant in practice** (one org, one Postgres instance), but the schema supports multiple workspaces per user and multiple users per workspace — it is architecturally multi-tenant.

---

## 8. Frontend

### Web — `apps/web/` (Next.js 16, App Router)

Routes under `[workspaceSlug]/(dashboard)/`:
- `issues/` — issue list, kanban, filters
- `agents/` — agent management, runtime pairing
- `projects/` — project/sprint management
- `squads/` — squad creation and management
- `autopilots/` — scheduled/webhook automations
- `runtimes/` — runtime device management, usage charts
- `skills/` — skill library
- `inbox/` — notifications
- `members/` — team management
- `my-issues/` — personal issue view
- `usage/` — token usage dashboard
- `settings/` — workspace + notification preferences

Key deps: TanStack Query v5, Zustand, Tiptap (rich editor), @base-ui/react (shadcn Base UI variant), dnd-kit, @tanstack/react-query.

### Desktop — `apps/desktop/` (Electron, electron-vite)

Bundles the `multica` CLI binary via `scripts/bundle-cli.mjs`. Ships the same shared views as web via `packages/views/`. Tab-based multi-workspace UI with per-workspace memory routers. Notable architectural constraints (from CLAUDE.md desktop rules):
- Pre-workspace flows (create workspace, accept invite) are `WindowOverlay` state, not routes
- Tab groups isolated per workspace via `stores/tab-store.ts`
- `setCurrentWorkspace(null, null)` required before destructive workspace operations
- `<DragStrip />` required on every full-window page (macOS drag region)

### Mobile — `apps/mobile/` (Expo / React Native, iOS)

Locked to Expo SDK React version (lags web React by 6-12 months). Shares only types and pure functions from `@multica/core/` with `import type`. Independent UI, state, build pipeline. Routes under `(app)/` and `(auth)/`. No Android target mentioned.

### Shared packages
- `packages/core/` — Zustand stores, TanStack Query hooks, API client, platform bridge (`CoreProvider`). Zero react-dom, zero localStorage.
- `packages/ui/` — shadcn components (Base UI primitives). Zero `@multica/core` imports.
- `packages/views/` — business pages/components. Zero `next/*` or `react-router-dom` imports. Uses `NavigationAdapter` for routing.

**Internal packages pattern**: packages export raw `.ts/.tsx` (no pre-compilation). Consuming app bundler compiles directly — zero-config HMR + instant go-to-definition.

---

## 9. Skills System

### Server-side skills
Skills are workspace-level entities stored in Postgres (`skill` + `skill_file` tables, migration 008). An agent has a many-to-many skill set (`agent_skill`). When a task is dispatched, the daemon's `ClaimTaskByRuntime` response includes the full skill content + files. The daemon writes these into provider-native paths in the task workdir.

**Skill import** (`handler/skill_create.go`): URL import from skills.sh, GitHub repos, or any `SKILL.md`-hosting URL. The handler fetches, parses frontmatter (name, description), stores content.

**Agent templates** (`internal/agenttmpl/`): static JSON files embedded at compile time (`agenttmpl/templates/*.json`). Each template has `Instructions` + `Skills` (list of `TemplateSkillRef` with `source_url`). Creating an agent from a template materialises the skills into the workspace. No runtime mutation path — changes go through PR review.

### Client-side skills lockfile (`skills-lock.json`)
The repo itself has a `skills-lock.json` locking 4 frontend design skills:
- `frontend-design` (anthropics/skills)
- `shadcn` (shadcn/ui)
- `ui-ux-pro-max` (nextlevelbuilder/ui-ux-pro-max-skill)
- `web-design-guidelines` (vercel-labs/agent-skills)

Format: `{version: 1, skills: {name: {source, sourceType, skillPath?, computedHash}}}`. This is how Multica dogfoods its own skill system for its own frontend development agents.

### Local skills discovery
The daemon also discovers skills from agent CLIs' user-level skill directories:
- Claude: `~/.claude/skills/`
- Copilot: `~/.github/copilot/skills/`  
- OpenCode: `~/.opencode/skills/`
- Pi: `~/.pi/skills/`
- Cursor: `~/.cursor/skills/`
- Kiro: `~/.kiro/skills/`

These local skills are surfaced to the server via the `local-skills` daemon WebSocket flow and can be imported into workspace skills via the UI.

---

## 10. Self-Host Story

### Docker Compose (`docker-compose.selfhost.yml`)

```
┌───────────┐    ┌──────────────────┐    ┌──────────────────────┐
│ postgres  │◀───│ backend (Go)     │◀───│ frontend (Next.js)   │
│ pg17+pgv  │    │ :8080 (loopback) │    │ :3000 (loopback)     │
└───────────┘    └──────────────────┘    └──────────────────────┘
      ▲
  pgvector
  volume:pgdata
  volume:backend_uploads
```

Three services: Postgres, Go backend, Next.js frontend. All bind to `127.0.0.1` only — requires reverse proxy (Caddy/nginx/Cloudflare Tunnel) for public access.

### Configuration surface (selected env vars)

| Category | Vars |
|----------|------|
| Database | `DATABASE_URL`, `DATABASE_MAX_CONNS`, `DATABASE_MIN_CONNS` |
| Auth | `JWT_SECRET`, `APP_ENV`, `MULTICA_DEV_VERIFICATION_CODE` |
| Email | `RESEND_API_KEY` or `SMTP_HOST/PORT/USERNAME/PASSWORD` |
| OAuth | `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `GOOGLE_REDIRECT_URI` |
| Storage | `S3_BUCKET`, `S3_REGION`, `AWS_*`, `AWS_ENDPOINT_URL`, `CLOUDFRONT_*` |
| Signup | `ALLOW_SIGNUP`, `ALLOWED_EMAIL_DOMAINS`, `ALLOWED_EMAILS` |
| Redis | `REDIS_URL` (optional; enables realtime relay, rate limiting, caches) |
| Analytics | `POSTHOG_API_KEY`, `POSTHOG_HOST` |
| GitHub App | `GITHUB_APP_SLUG`, `GITHUB_WEBHOOK_SECRET` |
| Cloud fleet | `MULTICA_CLOUD_FLEET_URL` (SaaS only) |
| Observability | `METRICS_ADDR` (Prometheus), `LOG_LEVEL` |
| Security | `MULTICA_TRUSTED_PROXIES`, `RATE_LIMIT_TRUSTED_PROXIES` |

### Advanced topology (from `SELF_HOSTING_ADVANCED.md`)
- Multi-node: Redis required for realtime relay + caches
- PgBouncer/RDS Proxy: supported via `DATABASE_MAX_CONNS` tuning
- MinIO/R2/B2: supported via `AWS_ENDPOINT_URL` (path-style URLs)
- SMTP relay: STARTTLS auto-negotiated (port 25/587; no SMTPS/465)
- pgvector: extension `CREATE EXTENSION IF NOT EXISTS "pgvector"` in init migration
- Worktree-aware dev: each git worktree gets its own DB name + unique ports via `.env.worktree`

---

## 11. Notable Patterns & Conventions

### 1. Combined CLI + daemon binary
A single Go binary serves as both the user-facing CLI and the local daemon. No separate install. `multica daemon start` enters daemon mode; all other subcommands are CLI operations. This dramatically simplifies distribution (one Homebrew tap formula).

### 2. sqlc for type-safe DB queries
All Postgres queries are hand-written SQL in `server/pkg/db/queries/*.sql`, compiled by sqlc into Go structs (`server/pkg/db/generated/`). No ORM. Query changes require `make sqlc` regeneration.

### 3. In-process sync event bus + optional Redis relay
`events.Bus` is synchronous in-process pub/sub (not Kafka, not RabbitMQ). The realtime fan-out layer (`realtime/redis_relay.go`) optionally relays to Redis streams for multi-node. This means single-node deployments have zero infrastructure overhead beyond Postgres.

### 4. Polymorphic assignee pattern
`assignee_type` + `assignee_id` on `issue` (member / agent / squad). No separate junction tables. The same pattern applies to `creator_type` + `creator_id` and `linked_by_type` + `linked_by_id`. Keeps queries simple at the cost of no FK enforcement.

### 5. Workspace-scoped reserved slugs
`reserved_slugs.json` (Go side, embedded) → `reserved-slugs.ts` (TypeScript, generated). CI fails on drift. Single source of truth for route protection. See `handler/reserved_slugs.json`.

### 6. Empty-claim cache
Redis-backed `EmptyClaimCache` avoids hot Postgres scans on the 3s poll cycle for runtimes with no queued work. Server sets a short-lived "empty" flag after a miss; subsequent polls skip the DB until the cache expires or a new task arrives.

### 7. Sharded Redis stream relay for realtime
`realtime/sharded_stream_relay.go` — partitions workspace events across N Redis streams. Allows horizontal scaling of the WS fan-out layer.

### 8. Agent brief as structured markdown
The task prompt injected into every agent run is a full markdown document with named sections (`## Workspace Context`, `## Requesting User`, `## Sub-issue Creation`, `## Skills`, `## Mentions`, `## Attachments`, `## Output`). Consistent cross-provider brief structure enables skill portability.

### 9. Squads as first-class assignees
A squad dispatches to its leader agent with the squad's `id` as the assignee. The leader agent receives `SquadID` + `SquadName` in the task payload and handles delegation to squad members. `squad activity no_action` is a special exit path that suppresses result comment posting.

### 10. pgvector installed but unused
`pgvector` extension is present from migration 001. No vector columns or similarity queries exist in the 107 migrations. This is reserved infrastructure for a future search/recommendation feature.

---

## 12. Risk & Maturity Flags

### Half-built / forward-only features

| Feature | Evidence | State |
|---------|----------|-------|
| Cloud runtime fleet | `cloudruntime.Client` present; `MULTICA_CLOUD_FLEET_URL` env; all endpoints return 503 when unset | SaaS only, OSS stub |
| `run_only` autopilot mode | Migration 042 defines `execution_mode IN ('create_issue', 'run_only')`; CLI comment: "daemon task path doesn't yet resolve a workspace for runs without an issue, so it's not exposed by the CLI" | Partially wired |
| Autopilot `webhook` + `api` trigger kinds | Migration 091 defines them; CLI: "no server endpoint that fires them yet" | Schema-only |
| Linear / Jira integration | No code, no migration | Not started |
| pgvector search | Extension installed, zero use | Reserved |
| Android mobile | iOS only in docs/scripts | Not targeted |

### Security observations

| Issue | Location | Severity | Notes |
|-------|----------|----------|-------|
| Default JWT secret hardcoded | `server/internal/auth/jwt.go:7` | High | `multica-dev-secret-change-in-production` — deploy without changing = open auth. Documented but a footgun. |
| No sandbox on local agent execution | daemon execenv | Medium | Agents run as OS user with full FS access. Malicious skill or prompt injection could exfiltrate credentials. |
| `MULTICA_DEV_VERIFICATION_CODE` | `.env.example` | Medium | Fixed-code bypass; guarded by `APP_ENV != production` but relies on operator discipline. |
| Cookie domain on IP literal | `SELF_HOSTING_ADVANCED.md` | Low | RFC 6265 forbids IP literals in `Domain`; documented but could silently break auth. |
| Webhook rate limiter bypass | `handler/webhook_rate_limiter.go` | Low | XFF trust requires explicit `MULTICA_TRUSTED_PROXIES`; default is safe (fail-closed), but misconfig exposes per-IP bypass. |

### Performance hotspots

| Area | Evidence | Note |
|------|----------|------|
| 3s poll per runtime | Daemon config default | N runtimes × M workspaces = rapid DB load on large deployments. EmptyClaimCache mitigates. |
| Task usage hourly rollup | pg_cron-based; dirty queue for invalidation | Well-engineered but adds operational complexity (pg_cron extension required) |
| Comment list hard cap | `comment list` docs: "Hard cap of 2000 rows" | Agent context window risk on long-running issues |
| Realtime hub fan-out | Sharded Redis streams | Correct design, requires Redis for multi-node |

### Technical debt items

- **Migration numbering collisions**: at migration 091, three separate migrations share the `091_` prefix (`091_autopilot_webhook_triggers`, `091_issue_start_date`, `091_pr_ci_conflict`). The migration runner must handle this gracefully — indicates rapid parallel development. Same for `060_`, `079_`, `032_`, `035_`, etc. at earlier points.

- **Daemon + handler package size**: `server/internal/handler/` contains 80+ files. No sub-package structure. The handler package is a large flat collection that will become harder to navigate as the product grows.

- **`issueguard/duplicate.go`**: duplicate detection uses advisory Postgres locks on a normalised title hash. Correct but fragile — any title normalisation change silently breaks duplicate detection for in-flight locks.

- **`mention/expand.go`**: `@-mention` side effects (triggering agent runs, sending notifications) are embedded in a markdown renderer. No transactional guarantee between comment write and mention expansion. Race condition possible if mention expansion fails mid-write.

- **No Linear or webhook-to-issue bridge**: the autopilot `webhook` trigger kind exists in schema but lacks a firing endpoint. External event-driven automation (GitHub PR opened → create issue) is not currently implementable without custom code.

- **`cloudruntime` proxy is a black box**: all fleet management endpoints proxy to `MULTICA_CLOUD_FLEET_URL` with user PAT forwarding (`X-Multica-User-Id` + `X-Multica-User-PAT`). No OSS implementation. This is the moat.

---

## 13. API Surface Summary

**Daemon-facing** (`/api/daemon/`, daemon-token auth):
- Register / deregister / heartbeat / WebSocket
- ClaimTask, StartTask, ProgressTask, CompleteTask, FailTask, UsageTask, Messages
- RecoverOrphanedTasks, PinTaskSession
- GC check endpoints (issue/chat/autopilot/task)
- Runtime-level: models request, local-skills request/import, update request

**User-facing** (JWT/PAT auth, workspace-scoped via middleware):
- Issues: CRUD, comments, timeline, subscribers, reactions, metadata, batch ops, rerun, task runs
- Agents: CRUD, archive/restore, skills, tasks, from-template
- Runtimes: list, usage, update, models, local-skills, delete
- Skills: CRUD, import, files
- Projects: CRUD, status
- Squads: CRUD, briefing, activity
- Autopilots: CRUD, trigger, runs, trigger-add/update/delete
- Chat: sessions + messages
- GitHub: connect, installations, webhook
- Dashboard: usage/daily, usage/by-agent, agent runtime, runtime daily
- Workspace: CRUD, members, invitations, repos, context
- Inbox: list, unread count, read/archive ops
- Pins, Attachments, Search, Notification preferences, PATs

**Total routes**: ~120 distinct HTTP endpoints.

---

## Appendix: Dependency Summary

### Go (server/go.mod)
```
chi v5.2.5            — HTTP router
gorilla/websocket v1.5.3
jackc/pgx v5.8.0      — Postgres driver
redis/go-redis v9.18.0
golang-jwt/jwt v5.3.1
aws/aws-sdk-go-v2     — S3, SecretsManager
prometheus/client_golang v1.23.2
resend/resend-go v2.28.0
robfig/cron v3.0.1    — autopilot schedule
spf13/cobra v1.10.2   — CLI
mattn/go-shellwords   — POSIX arg parsing for MULTICA_CLAUDE_ARGS
```

### TypeScript (key shared deps, pnpm catalog)
```
Next.js 16 (App Router)
React 18/19
Electron (electron-vite)
Expo SDK (React Native, iOS)
TanStack Query v5
Zustand
Tiptap (rich text editor)
@base-ui/react (shadcn variant)
dnd-kit
Vitest
Playwright (e2e)
```

### Infrastructure
```
Postgres 17 + pgvector
Redis (optional; realtime relay, caches, rate limiter)
S3/CloudFront (optional; file uploads)
pg_cron (optional; usage rollup automation)
```
