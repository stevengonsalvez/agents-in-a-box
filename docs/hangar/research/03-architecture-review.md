# Multica — Architecture Review (staff-architect grade)

**Subject**: `multica-ai/multica` @ branch `feat/multica` (worktree)
**Reviewer perspective**: extracting *patterns* worth porting to `ainb` (agents-in-a-box)
**Method**: source-level read of `server/`, migrations 001–098, daemon, daemonws, realtime, service, handler, agenttmpl, cloudruntime; cross-checked against `CLAUDE.md`, `CLI_AND_DAEMON.md`, `SELF_HOSTING.md`, `AGENTS.md`.
**Verdict up front**: borrow the lifecycle (queue-runtime-task state machine), the dual-WS (user-fanout vs daemon-control) split, the workspace-scoped multi-tenancy with polymorphic actors, the embedded curated-templates pattern, and the per-task isolated env. **Do not** copy the realtime stack wholesale (Redis Streams + Hub + dedup is overbuilt for v1 ainb scale); do not copy the "everything on one Postgres aggregate" model long-term; do not copy the v1 onboarding/shim flows. Detailed evidence below.

---

## 1. High-level architecture diagram

```
┌──────────────────────────────────────────────────────────────────────────────┐
│                              CLIENTS                                         │
│                                                                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────────┐   │
│  │   Next.js    │  │  Electron    │  │ Expo / RN    │  │ `multica` CLI  │   │
│  │  (apps/web)  │  │ (apps/desk-  │  │ (apps/mobile)│  │ (Go single-bin)│   │
│  │              │  │  top)        │  │              │  │                │   │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  └────────┬───────┘   │
│         │ HTTPS+WS         │ HTTPS+WS        │ HTTPS+WS         │ HTTPS+WS   │
│         │ (cookie JWT)     │ (cookie JWT)    │ (PAT mul_…)      │ (PAT mul_… │
│         │                  │                 │                  │  or mdt_…) │
└─────────┼──────────────────┼─────────────────┼──────────────────┼────────────┘
          ▼                  ▼                 ▼                  ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│                         GO BACKEND (server/, single binary)                  │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │  Chi router + middleware stack                                       │   │
│  │   /ws ──────────► realtime.Hub      (user/workspace/task/chat WS)    │   │
│  │   /api/daemon/ws ► daemonws.Hub     (daemon-runtime control WS)      │   │
│  │   /api/* ────────► handler.*        (REST resource API)              │   │
│  │   /api/webhooks/*► autopilot/github (no Multica auth, HMAC + token)  │   │
│  └────────────┬───────────────────────────┬────────────────────────────-┘   │
│               │                           │                                  │
│               ▼                           ▼                                  │
│  ┌────────────────────────┐   ┌─────────────────────────────────┐           │
│  │  service.* (TaskSvc,   │   │  events.Bus  (in-process pub/sub│           │
│  │  AutopilotSvc, Email)  │──▶│  fan-out — Subscribe / Publish) │           │
│  └────────────┬───────────┘   └─────────────┬───────────────────┘           │
│               │                              │ workspace/task/chat events    │
│               ▼                              ▼                                │
│  ┌────────────────────────┐   ┌──────────────────────────────────┐          │
│  │ sqlc-generated queries │   │  realtime broadcaster +          │          │
│  │ (pkg/db/generated)     │   │  optional Redis Streams relay    │          │
│  └────────────┬───────────┘   └──────────────────────────────────┘          │
│               │                                                              │
└───────────────┼──────────────────────────────────────────────────────────────┘
                ▼                                  │ optional
        ┌──────────────────┐                       ▼
        │  PostgreSQL 17   │            ┌─────────────────────┐
        │  + pgvector +    │            │   Redis (PAT cache, │
        │  pgcron          │            │   empty-claim cache,│
        │  ~98 migrations  │            │   stream relay,     │
        │                  │            │   rate limit, etc.) │
        └──────────────────┘            └─────────────────────┘

                    │
                    │ (cloud agent path only)
                    ▼
        ┌──────────────────────────────────────────────────────────────┐
        │  External "cloud runtime fleet" service (HTTP proxy in       │
        │  server/internal/cloudruntime/) — separate deployment        │
        │  reached via MULTICA_CLOUD_FLEET_URL                         │
        └──────────────────────────────────────────────────────────────┘


        ┌──────────────────────────────────────────────────────────────┐
        │                  LOCAL AGENT DAEMON (per user machine)       │
        │                                                              │
        │  ┌─────────────────────────────────────────────────────────┐ │
        │  │ `multica daemon start`  (same Go binary)                │ │
        │  │  - workspaceSyncLoop / heartbeatLoop / pollLoop /       │ │
        │  │    autoUpdateLoop / gcLoop / taskWakeupLoop / serveHealth│
        │  └────────┬───────────────────────┬─────────────────────-──┘ │
        │           │ HTTPS REST            │ WSS (task wakeup +       │
        │           │ (claim/start/         │      heartbeat ack)      │
        │           │  complete/fail/       │                          │
        │           │  progress/usage)      │                          │
        │           ▼                       ▼                          │
        │   ┌────────────────────────────────────────────────┐         │
        │   │ Per-task isolated env (~/multica_workspaces/   │         │
        │   │  <ws_id>/<short_task_id>/{workdir, output,     │         │
        │   │  logs, .gc_meta.json, codex-home?, openclaw…)  │         │
        │   └───────────────────────┬────────────────────────┘         │
        │                           │ exec()                           │
        │                           ▼                                  │
        │   ┌─────────────────────────────────────────────────────┐    │
        │   │ Agent CLI subprocess (claude / codex / copilot /    │    │
        │   │  openclaw / opencode / hermes / gemini / pi /       │    │
        │   │  cursor-agent / kimi / kiro-cli)                    │    │
        │   └─────────────────────────────────────────────────────┘    │
        └──────────────────────────────────────────────────────────────┘
```

Notes:
- Daemon and CLI are **the same Go binary** (`server/cmd/multica/`) with different verbs (`multica daemon start`, `multica issue create`, ...). The "control plane" is a binary that knows how to be its own client.
- Backend is also a **single Go binary** (`server/cmd/server/`) wired in `router.go:96` (`NewRouter`) and `router.go:111` (`NewRouterWithOptions`). The Hub, DaemonHub, events.Bus, analytics client, and Redis client are constructed in `main.go` and passed through.
- No service-to-service split inside the backend — `realtime.Hub`, `daemonws.Hub`, `events.Bus`, sqlc Queries are all in-process.

---

## 2. Layered breakdown — what runs where

### Process inventory

| Process | Binary | Lives where | Talks to |
|---|---|---|---|
| API server | `server/cmd/server` (Go) | Docker container or systemd | Postgres, Redis (optional), cloud-runtime fleet (optional), PostHog (optional) |
| `multica` CLI | `server/cmd/multica` (Go) | User laptop | API server over HTTPS |
| `multica daemon` | same `multica` Go binary, `daemon start` subcommand | User laptop (background) | API server (HTTPS + WSS) and local agent CLIs (subprocess `exec`) |
| Web app | Next.js 16 App Router (`apps/web/`) | Docker `multica-web:dev` or Vercel | API server `/api/*`, `/ws` |
| Desktop | Electron (`apps/desktop/`) | User laptop | API server, also spawns/manages local daemon (`apps/desktop/src/main/daemon-manager.ts`) |
| Mobile | Expo iOS (`apps/mobile/`) | User iPhone | API server only — no daemon |
| Cloud runtime "fleet" | **separate, closed-source** HTTP service (`server/internal/cloudruntime/client.go:21` notes `MULTICA_CLOUD_FLEET_URL`) | Multica Cloud only | n/a (proxied through API server) |

### Deployment topology

```
SELF-HOST (single host)                        MULTICA CLOUD (managed)
┌───────────────────────────┐                  ┌──────────────────────────────┐
│ docker compose            │                  │  K8s / similar               │
│  - multica-backend (Go)   │                  │   - N × multica-backend pods │
│  - multica-web (Next.js)  │                  │   - multica-web              │
│  - postgres (pgvector)    │                  │   - postgres (managed)       │
│  - redis (optional)       │                  │   - redis (mandatory at      │
│                           │                  │     multi-node — see below)  │
│  - host machine runs      │                  │   - cloud-runtime fleet      │
│    `multica daemon`       │                  │     (separate service)       │
└───────────────────────────┘                  │                              │
                                               │  User machine still runs     │
                                               │  `multica daemon` locally    │
                                               │  unless they use Cloud       │
                                               │  Runtime entirely.           │
                                               └──────────────────────────────┘
```

**Critical asymmetry**: even in Multica Cloud, the **default execution model is still on the user's laptop**. The "cloud runtime" is a paid add-on proxied through the API server to a separate fleet service that Multica owns. The OSS repo only contains the *client* to that fleet (`server/internal/cloudruntime/client.go`), not the fleet itself. This is the key business-model lever — the OSS daemon does the work for free; the cloud runtime is the upsell.

---

## 3. Control plane vs data plane separation

Multica **does** enforce a control/data split, but the lines are subtle.

```
CONTROL PLANE                            DATA PLANE
─────────────                            ──────────

multica server (Chi REST API)            agent CLI subprocess
 - issue CRUD                            (claude/codex/...)
 - task lifecycle state machine            ▲
 - assignment, scheduling                  │ exec, stdin/stdout
 - workspace/membership                    │
 - daemon registration                   isolated env directory
 - skill management                      (~/multica_workspaces/
                                          <ws>/<task>/workdir)
       │                                    ▲
       │ tells daemon                       │ daemon manages
       │ "task X is yours"                  │
       ▼                                    │
multica daemon (HTTP claim,         ──────► spawns agent
              WS wakeup,                    process per task
              HTTP report)
```

What is *strictly* control plane (server-side):
- task state transitions (`queued → dispatched → running → completed/failed/cancelled` — `001_init.up.sql:132`),
- scope authorization (`server/cmd/server/scope_authorizer.go`),
- analytics event capture (`server/internal/analytics/`),
- assignment routing,
- runtime liveness (`runtime_sweeper.go:30` — `staleThresholdSeconds = 150.0`).

What is *strictly* data plane (daemon-side or off-server):
- the actual LLM call (the agent CLI subprocess does it; daemon shells out via `execenv.Prepare`),
- repo cloning and worktree creation (`server/internal/daemon/repocache/`),
- skill file writing into the env (`execenv.writeContextFiles`),
- token usage measurement (computed locally by the CLI, *reported* to server via `/tasks/{id}/usage`).

What is *intentionally muddled*:
- **Comments and progress updates** flow control-plane (REST `/tasks/{id}/progress`), even though they're produced data-side. Multica accepted this because it lets the server be the single source of truth for the UI.
- **Heartbeats** are control-plane signals but they piggyback **action delivery** — `daemon.go:1279` `handleHeartbeatActions` shows heartbeat acks deliver `PendingUpdate`, `PendingModelList`, `PendingLocalSkills`, `PendingLocalSkillImport`. The heartbeat is doing double duty as a poll for queued control commands. That's pragmatic but it bundles two concerns.

**Pattern worth copying**: server never knows what the LLM said. It only knows `result`, `session_id`, `work_dir`, `usage`. Compresses the trust boundary nicely.

---

## 4. Agent runtime model

This is the most interesting architectural decision in the codebase. Three modes coexist:

```
                         ┌─ runtime_mode = 'local' (default OSS path)
                         │      → daemon on user machine
                         │      → agent CLI spawned per task
agent_runtime row ──────►├─ runtime_mode = 'cloud'
                         │      → server proxies to Multica Cloud
                         │        fleet via cloudruntime.Client
                         │
                         └─ (legacy) 'multica_agent' provider on
                            migrated cloud runtimes
```

Source: `004_agent_runtime_loop.up.sql:6` — `runtime_mode TEXT NOT NULL CHECK (runtime_mode IN ('local', 'cloud'))`. Each `agent` row points to a `runtime_id` (FK to `agent_runtime`) since migration 004.

### Local mode (canonical)

Per-task isolation is **directory-per-task, not container-per-task**. From `execenv.go:122`:

```
PredictRootDir = {workspacesRoot}/{workspace_id}/{shortID(task_id)}/
  ├── workdir/        ← cwd passed to the agent CLI
  ├── output/
  ├── logs/
  ├── .gc_meta.json   ← lifecycle hint for daemon GC
  ├── codex-home/     ← only when provider=codex
  └── (skills written under provider-native paths)
```

- **No container**. The agent CLI runs as a regular subprocess of the daemon, on the user's user account. Sandboxing is delegated to the agent (e.g. Codex sandbox policy in `execenv/codex_sandbox.go`).
- **No worktree-per-task by default**. The "workdir" starts *empty*. The agent calls back via `multica repo checkout <url>` (handled by the daemon's local HTTP server, `daemon/health.go`-ish file `repoCheckoutRequest`) — that's when the daemon clones into the workdir, using a per-workspace repo cache (`daemon/repocache/`) backed by a bare clone and `git worktree add`.
- **Resume is supported**: `agent_task_queue.session_id` + `work_dir` (`020_task_session.up.sql`) capture the Claude Code (or equivalent) session-id and the resolved workdir. Next task for the same `(agent, issue)` resumes via `--resume <session_id>` against the same workdir.
- **GC is meta-driven**: `.gc_meta.json` tells the periodic GC loop which "parent" (issue/chat session/autopilot run/task) it belongs to so it can ask the server whether to keep, do artifact-only cleanup, or wipe (`CLI_AND_DAEMON.md:185-193`).

### Cloud mode

`server/internal/cloudruntime/client.go` is **purely an HTTP reverse-proxy client**. The handler (`server/internal/handler/cloud_runtime.go`) forwards to an external service identified by `MULTICA_CLOUD_FLEET_URL`. The OSS repo does not contain a cloud runtime implementation. So "cloud mode" in OSS is *aspirational*: the surface exists, but you must point at an external fleet.

This is a sane separation: it's how Multica monetizes without forcing OSS users into a half-implemented cloud path.

### Bring-your-own-machine semantics

Daemon registration is **workspace-scoped**:
- `agent_runtime` has unique `(workspace_id, daemon_id, provider)` (`004_agent_runtime_loop.up.sql:14`).
- One daemon process can serve **N workspaces** the user belongs to — `syncWorkspacesFromAPI` (`daemon.go:1041`) iterates the user's workspace list and creates a `workspaceState` per workspace.
- Each `agent` (logical persona in the UI) is FK'd to exactly one `agent_runtime`. So "assigning Alice the agent to issue MUL-123" means "give the task to the runtime backing Alice", which means "this user's daemon (or the cloud fleet) executes the work".

---

## 5. Transport stack

Five distinct transport channels — and they matter. ASCII first, table second.

```
Browser/Desktop/Mobile clients                  Daemon (per user machine)
─────────────────────────────                    ─────────────────────────

   ┌───────────────────┐                          ┌──────────────────┐
   │  HTTPS REST       │                          │  HTTPS REST      │
   │  /api/*           │                          │  /api/daemon/*   │
   │  JWT cookie or    │                          │  PAT (mul_) or   │
   │  PAT (mul_) bearer│                          │  daemon token    │
   └────────┬──────────┘                          │  (mdt_) bearer   │
            │                                     └────────┬─────────┘
            │                                              │
            ▼                                              ▼
   ┌───────────────────┐                          ┌──────────────────┐
   │  WSS /ws          │                          │  WSS             │
   │  realtime.Hub     │                          │  /api/daemon/ws  │
   │  cookie or first- │                          │  daemonws.Hub    │
   │  frame `auth` msg │                          │  Authorization   │
   │                   │                          │  header          │
   │  subscribe to     │                          │                  │
   │  task:* / chat:*  │                          │  daemon:heartbeat│
   │  (workspace +     │                          │  daemon:task_    │
   │   user auto-      │                          │  available       │
   │   subscribed)     │                          │                  │
   └───────────────────┘                          └──────────────────┘

                    ▼                                       ▼
            ┌─────────────────────────────────────────────────────┐
            │  events.Bus (in-process)                            │
            │  Subscribe("issue:created", fn)                     │
            │  Publish(events.Event{Type, WorkspaceID, ...})      │
            │                                                     │
            │  + optional Redis Streams relay                     │
            │    (cross-node fan-out for multi-replica deploy)    │
            └─────────────────────────────────────────────────────┘

      Optional 5th transport:
            ┌─────────────────────────────────────────────────────┐
            │  CLI → daemon IPC                                   │
            │  Local HTTP loopback (127.0.0.1:<HealthPort>)       │
            │  e.g. `multica repo checkout` from inside an agent  │
            │  subprocess hits the daemon's health server         │
            └─────────────────────────────────────────────────────┘
```

| Channel | File | Auth | Frame format | Purpose |
|---|---|---|---|---|
| HTTPS REST (user) | `server/internal/handler/*.go` | JWT cookie or `mul_…` PAT | JSON | Resource CRUD, task lifecycle from UI |
| HTTPS REST (daemon) | `handler/daemon.go`, `task_lifecycle.go` | DaemonAuth (`mdt_…` or PAT) | JSON | Register, claim, start, complete, fail, heartbeat, progress, usage |
| WSS user (`/ws`) | `realtime/hub.go:658` (HandleWebSocket) | cookie or first-frame `{type:"auth", payload:{token}}` | JSON `{type, payload}` with `subscribe`/`unsubscribe`/`ping` | Push to UI, scoped to workspace/user/task/chat |
| WSS daemon (`/api/daemon/ws`) | `daemonws/hub.go:122` | Authorization header pre-upgrade | `protocol.Message{Type, Payload}` JSON | `daemon:task_available` wakeup, `daemon:heartbeat` + ack carrying pending actions |
| In-process `events.Bus` | `events/bus.go` | n/a | typed Go struct | Domain pub/sub between handlers and listeners; *not* on the wire |
| Loopback HTTP (CLI ↔ daemon) | `daemon/health.go` + `multica` subcommands | none (127.0.0.1 only) | JSON | `repo checkout`, status reads |

Smart design touches:
- **Two separate WS hubs** — user WS and daemon WS. They have different auth, different origin policies (`daemonws/hub.go:95` allows all origins because daemons don't use cookies), different dedup semantics (`hub.go:151` `markSeen` LRU of 128).
- **WS for daemons is a *wakeup hint*, not the source of truth** — the daemon still calls HTTP `/tasks/claim` for correctness (`daemonws/hub.go:75` "best-effort wakeup hints; the daemon still uses HTTP claim"). Combined: HTTP gives at-least-once with idempotency; WS gives sub-second latency.
- **Heartbeat is bimodal**: WS heartbeats suppress HTTP heartbeats while WS is alive (`daemon.go:531` `wsHeartbeatFreshness`), HTTP resumes automatically when WS dies. Belt-and-braces.
- **Origin checks fall through for native clients**: `realtime/hub.go:92` — when the WS Origin matches the connection target Host, the request is treated as same-origin (native clients have no real page host).

---

## 6. State machine — task lifecycle

The whole platform pivots on `agent_task_queue.status`. From `001_init.up.sql:132` and `055_task_lease_and_retry.up.sql`:

```
                ┌──────────────────────────────────────────────────────┐
                │                                                      │
                │                                                      ▼
   ┌──────────┐    enqueue       ┌──────────┐    sweeper      ┌─────────────┐
   │   (new)  │─────────────────▶│  queued  │ ─2h TTL──▶ ─── ▶│  failed     │
   └──────────┘   TaskService.   └────┬─────┘  failure_       │  (timeout)  │
                  EnqueueTask*        │        reason='timeout'└─────────────┘
                                     │
                                ClaimTaskForRuntime (daemon HTTP)
                                     │
                                     ▼
                                ┌─────────────┐ ◀── ReclaimStaleDispatchedTask
                                │ dispatched  │     (re-claim if response lost,
                                │             │      90s recovery window —
                                │             │      task.go:85)
                                └─────┬───────┘
                                     │ StartTask (daemon HTTP)
                                     │   5min dispatch TTL — sweeper
                                     │   fails as 'failed' if exceeded
                                     ▼
                                ┌─────────────┐ ── 2.5h timeout ──▶  failed
                                │  running    │      (sweeper)
                                │             │
                                │             │ ── server cancel ──▶ cancelled
                                │             │      (issue reassign,
                                │             │       user cancel,
                                │             │       comment retract)
                                └─────┬───────┘
                  ┌──────────────────┼──────────────────┐
       CompleteTask         FailTask                CancelTask /
                                                    runtime gone
                  ▼                  ▼                  ▼
              ┌────────────┐  ┌──────────┐    ┌────────────┐
              │ completed  │  │  failed  │    │ cancelled  │
              └────────────┘  └─────┬────┘    └────────────┘
                                     │
                              attempt < max_attempts
                              + failure_reason in
                              ('runtime_offline','runtime_recovery')
                                     │
                                     ▼
                              auto-rerun → new task row,
                              parent_task_id = old.id
                              (055_task_lease_and_retry)
```

Who owns each transition (sources: `service/task.go:742`, `944`, `970`, `1148`, `715`):

| From | To | Trigger | Authority |
|---|---|---|---|
| (new) | queued | `EnqueueTaskForIssue` / `…Mention` / `…SquadLeader` / `EnqueueQuickCreateTask` / `EnqueueChatTask` | Server (handler) on issue/comment/chat events |
| queued | dispatched | `ClaimTaskForRuntime` → `ClaimTask` | Server, called by daemon HTTP `/api/daemon/runtimes/{id}/tasks/claim` |
| dispatched | running | `StartTask` | Server, called by daemon HTTP `/api/daemon/tasks/{id}/start` |
| dispatched | queued | `ReclaimStaleDispatchedTaskForRuntime` (90s window) | Server-side query inside `ClaimTaskForRuntime` |
| running | completed | `CompleteTask` | Server, called by daemon HTTP `/api/daemon/tasks/{id}/complete` |
| running | failed | `FailTask` | Server, called by daemon (or sweeper) |
| running | cancelled | `CancelTask` / `CancelTasksForIssue` / `CancelTasksByTriggerComment` / `CancelTasksForAgent` | Server, triggered by user action or downstream logic |
| running | failed (timeout) | `sweepStaleTasks` (`runtime_sweeper.go:40` — 9000s) | Server background goroutine |
| queued | failed (TTL) | `sweepExpiredQueuedTasks` (2h TTL, batch 500) | Server background goroutine |
| failed | (new attempt) | auto-retry — new row with `parent_task_id = old.id` | Server, on heartbeat-driven runtime recovery |

Invariants worth highlighting:
- **At most one pending task per issue**: enforced by partial unique index `022_task_lifecycle_guards.up.sql:3` (`idx_one_pending_task_per_issue` WHERE status IN ('queued','dispatched')). This is the queue idempotency guarantee — re-fired enqueues coalesce.
- **`agent_task_queue.runtime_id` is the dispatch key**, not `agent_id`. `004_agent_runtime_loop.up.sql:76` added it and `067_task_queue_claim_candidate_index.up.sql` adds the matching index. The daemon polls "queued tasks for runtime R", and the server applies per-agent `max_concurrent_tasks` only within that filter.
- **Idempotent finalize**: `CompleteTask` (task.go:1010) does a `GetAgentTask` re-read if the UPDATE matched zero rows; when the task is already in a terminal state it returns success rather than erroring. Same pattern in `CancelTask`. This is what makes the lifecycle robust under WS-vs-HTTP races.

---

## 7. Multi-tenancy & auth model

**It is a multi-tenant system from day one.** Not single-user OSS-with-bolt-on-multi-tenant.

Every domain table has `workspace_id` (workspace is the tenant root) — confirmed across migrations 001–098. Cross-workspace queries do not exist; every query filters by workspace.

```
Identity flow:

User ──login──► JWT cookie OR PAT (mul_…) issued by server
       │
       ├──── browser/desktop: cookie-based, refreshes via /api/me
       └──── CLI: PAT in ~/.multica/config.json
                  │
                  ▼
              passes Authorization: Bearer mul_…
                  │
                  ▼
         middleware.Auth resolves → user_id
         middleware.RequireWorkspaceMember(queries) checks
            X-Workspace-ID header against member table
                  │
                  ▼
         every handler sees an authenticated (user_id, workspace_id) tuple

Daemon path:

Daemon ──login (multica login) ──► same PAT as the CLI
       OR
Daemon ──pairing flow (legacy 005_daemon_pairing, retired in 029_drop_daemon_pairing)
       OR
Daemon ──daemon_token (mdt_…) issued via /api/daemon-tokens
       │
       ▼
middleware.DaemonAuth resolves the bearer to a (user, workspaces[]) scope
       │
       ▼
daemon handlers gate by runtime ownership (requireDaemonRuntimeAccess)
```

Key entities and their boundaries:

| Aggregate | Tenancy | Notes |
|---|---|---|
| `workspace` | root | unique `slug` (`001_init.up.sql:18`); reserved slugs in `handler/reserved_slugs.json` |
| `member` | per workspace | role ∈ {owner, admin, member} |
| `user` | global | not workspace-bound; participates via `member` rows |
| `agent` | per workspace | `visibility ∈ {workspace, private}`, `owner_id` optional |
| `agent_runtime` | per workspace | unique `(workspace_id, daemon_id, provider)` |
| `issue` / `comment` / `inbox_item` | per workspace | always filtered by `workspace_id` |
| `agent_task_queue` | per workspace via `runtime.workspace_id` | not directly carried; resolved through FKs |

**Polymorphic actors** — the elegant bit. `assignee_type ∈ {member, agent}`, `creator_type ∈ {member, agent}`, `author_type ∈ {member, agent}`, `recipient_type ∈ {member, agent}`. From `001_init.up.sql:61-64, 100, 113, 160`. The same UI can show a human and an AI agent in any slot. The DB doesn't FK these (they'd need polymorphic FK support that Postgres lacks), so the server enforces via type-aware reads.

The CLAUDE.md call-out reinforces this:
> "Assignees are polymorphic — can be a member or an agent. `assignee_type` + `assignee_id` on issues. Agents render with distinct styling (purple background, robot icon)."

**Pattern worth copying for ainb**: don't separate `human_assignee_id` and `ai_assignee_id`. One pair `(actor_type, actor_id)`. It's the difference between "agents bolted on" and "agents are first-class".

---

## 8. Persistence design

### Schema themes from the 98 migrations

```
THEME                                       MIGRATIONS                             SHAPE
─────                                       ──────────                             ─────
Core resources (workspace/user/issue/       001                                    aggregate-per-table
  member/agent/comment/inbox/activity)
Agent config & runtime decoupling           002, 004, 023, 032, 048               agent → agent_runtime FK
Task lifecycle hardening                    022, 026, 029, 055, 067, 080         partial indexes, retry, lease
Search                                       032, 033, 036, 039                    pg_trgm + tsvector + lower idx
Real-time / collaboration                    015, 016, 017, 018, 025, 026, 027    subscriber + reactions + parent
Skills (structured)                          007, 008                              skill, skill_file, agent_skill
Personal Access Tokens                       011                                   sha256(token) row
Daemon auth evolution                        005 → 029 (drop pairing) → 029_daemon_token
Workspace repos & projects                   014, 034, 035, 058, 065              workspace_repo, project,
                                                                                  project_resource
Autopilots                                   042, 043, 079, 091, 093, 096, 097   trigger types: schedule/webhook/api
Chat (chat-with-agent feature)               033, 040, 060, 062, 063, 066         chat_session + chat_message
Usage / billing telemetry                    013, 032, 046 (drop), 072–078        task_usage + pgcron rollup
Squads (group routing)                       084–090                               squad, squad_member, leader role
Pinning, projects, attachments              038, 029_attachment, 083              user-customization layer
Reserved slug audits                         043, 045, 047, 049, 056              defensive: re-check slug table
                                                                                  on every relevant column change
```

### Dominant aggregates

- **Workspace** (root). Everything belongs to one.
- **Issue** (~Linear-style). Owns `comment[]`, `issue_to_label[]`, `issue_dependency[]`, `attachment[]`, `pull_request[]`, `issue_reaction[]`, `issue_subscriber[]`.
- **AgentTaskQueue** — the workhorse. Carries `agent_id`, `runtime_id`, `issue_id` (nullable for chat/autopilot/quick-create), `chat_session_id`, `autopilot_run_id`, `trigger_comment_id`, `result` (JSONB), `session_id` (resume pointer), `work_dir`, `attempt`, `max_attempts`, `parent_task_id`, `failure_reason`. This is a *huge* table by column count. The model said "use one queue table for everything" — issue tasks, chat tasks, autopilot tasks, quick-create tasks — and accepted the column sprawl as the cost of a unified state machine. Hard to argue with at this scale.
- **AgentRuntime** — splits compute from persona (`agent_runtime` is the binding to a daemon/cloud node; `agent` is the persona).
- **AutopilotRun** — execution log for the autopilot scheduler.

### Consistency model

- **Strongly consistent**: everything in Postgres — issues, comments, tasks, memberships, runtime registrations.
- **Eventually consistent**:
  - Runtime liveness (Redis vs DB — `runtime_sweeper.go:30` comment explains 105s worst-case DB lag).
  - Empty-claim cache (`server/internal/service/empty_claim_cache.go`) — Redis pre-filter; protected by a version sample (`task.go:872`) so a concurrent enqueue invalidates it.
  - PAT, daemon token, membership caches (`auth/pat_cache.go` etc.) — Redis-fronted with DB fallback.
  - Realtime fan-out across replicas — Redis Streams (`realtime/redis_relay.go`).
- **Idempotent writes via partial unique index**: `idx_one_pending_task_per_issue` (migration 022).

---

## 9. Observability

Three layers, no OTEL:

| Layer | Stack | Notes |
|---|---|---|
| Structured logging | `log/slog` everywhere (Go stdlib) | Every interesting code path has `slog.Info/Debug/Warn/Error` with structured kv pairs. Heavy use of attribute keys like `runtime_id`, `task_id`, `workspace_id`, `daemon_id`. |
| Metrics | Prometheus (`server/internal/metrics/`) | `client_golang/prometheus` registry built in `metrics/registry.go:27`. Custom collectors for `realtime.Hub`, `daemonws.Hub`, pgxpool DB, HTTP middleware, build_info. Scraped via `/health/realtime` JSON or a Prometheus endpoint elsewhere. |
| Product analytics | PostHog (`analytics/posthog.go`) | Batched HTTP sender, queue size 1024, batch 64, flush every 10s. Events enumerated in `analytics/events.go:6-34` — covers signup, agent task lifecycle, autopilot, onboarding. |

**No OTEL traces.** No distributed tracing instrumentation in the repo. For a system this large, that's a gap; presumably their cloud edge layer adds it, but the OSS code doesn't. If you copy this, add OTEL early — backfilling traces into a half-million LOC Go service is misery.

**Activity log table** (`001_init.up.sql:156`) is also doing observability double-duty as a domain construct — it powers the timeline UI, but it's also the audit trail.

---

## 10. Extension / plugin model

Two distinct layers.

### Built-in agent CLIs (NOT pluggable in the OSS sense)

The supported providers are **hardcoded** in `daemon/config.go` and the CLI detection list. From the README: claude / codex / copilot / openclaw / opencode / hermes / gemini / pi / cursor-agent / kimi / kiro-cli. Adding a new provider means writing a Go file in `server/internal/daemon/execenv/` (e.g. `codex_home.go`, `openclaw_config.go`) plus extending the agent registry in `server/pkg/agent/`. **There is no runtime extension API**.

Provider-specific work concentrates in `execenv/` (Codex needs a per-task CODEX_HOME; OpenClaw needs a synthesized config to pin its workspace dir). That's a clean encapsulation pattern — each provider's quirks live in one file. But it means **adding a new agent CLI is a backend code change, not a config**.

### Skills (genuinely pluggable, data-driven)

Skills are first-class workspace entities (`008_structured_skills.up.sql`):
- `skill (id, workspace_id, name, description, content, config JSONB)`
- `skill_file (skill_id, path, content)` — supporting files inside the skill
- `agent_skill (agent_id, skill_id)` — many-to-many binding

At task dispatch (`execenv.PrepareParams.Task.AgentSkills`), the daemon **writes the skill files into the per-task env in provider-native locations** (e.g. Codex's `~/.codex/skills/`, OpenClaw's scanner dir). So skills are *data the server pushes to the daemon*, which then materializes them onto disk in whatever shape the local CLI expects.

Skills can be imported from external sources (skills.sh, GitHub URLs) — `handler/skill.go:detectImportSource`. There's a "curated skills" notion via agent templates (see below) but no marketplace, no skill version pinning, no skill sandbox.

### Agent Templates (curated combos)

`server/internal/agenttmpl/` — 25+ JSON files (`adr-writer.json`, `brainstormer.json`, `bug-fixer.json`, ...) embedded into the binary at build time:

```go
//go:embed templates/*.json
var templateFS embed.FS
```

Each template bundles an `Instructions` block + a list of `TemplateSkillRef` (URLs to fetch). Picking a template imports the referenced skills into the workspace and creates the agent in one transaction. From `types.go:7-10`:
> "Templates are intentionally repo-only: their content is part of the product (the 'curated best-practice combos') and changes go through normal PR review. No runtime mutation, no admin UI."

**Pattern**: ship curated templates *in the binary*, not the database. Easy to A/B between releases, simple to audit, can never drift.

### Verdict on extensibility

It's **product-data extensible (skills, templates, autopilots)**, not **runtime extensible (no plugin SDK, no MCP-style plugin host, no out-of-process plugins)**. For a product that *uses* extensible LLM CLIs, that's a reasonable choice — Multica delegates the open-ended extension surface to the underlying agent (Claude Code's slash commands, OpenClaw's plugins, etc.).

---

## 11. Failure modes / scaling assumptions

What happens when…

### …N concurrent tasks

Daemon caps via `MaxConcurrentTasks` (default 20; `MULTICA_DAEMON_MAX_CONCURRENT_TASKS`). Per-runtime pollers acquire slots from a semaphore *before* claiming a task (`daemon.go:1854` `runRuntimePoller`). This is deliberate: claiming first and waiting for a slot would let dispatched tasks pile up and the 5min sweeper would kill them.

Server side: each agent's `max_concurrent_tasks` (default 1; migration 023) is enforced in `ClaimTask` (`task.go:761` `CountRunningTasks`). So the bottlenecks are:
- per-agent concurrency cap (configurable in UI, default 1),
- per-daemon concurrency cap (default 20),
- Postgres write throughput on `agent_task_queue` (every claim is a row update; `067_task_queue_claim_candidate_index.up.sql` is the optimization for the SELECT side).

### …daemon crashes

- Heartbeat stops → runtime sweeper flips runtime to `offline` after 150s (`runtime_sweeper.go:30`).
- Tasks stuck in `dispatched` longer than 5min fail with `failure_reason='timeout'`.
- Tasks stuck in `running` longer than 2.5h fail same way.
- On daemon restart, `recover-orphans` (called per runtime in `daemon.go:1119`) tells the server which tasks the previous process was on. The server can mark them failed or, for retry-eligible failures, auto-rerun (migration 055).

### …server crashes

- Connections drop. Daemons fall back to HTTP heartbeat + exponential backoff on WS reconnect (`wakeup.go:50`). The next successful heartbeat re-syncs state.
- WS clients (browsers) lose their subscriptions; on reconnect they re-`subscribe` to the scopes they care about.
- Postgres is the only durable thing; nothing in Redis is treated as authoritative (caches and pre-filters only).

### …Postgres goes away

- Hard fail. No graceful degradation. The OSS deployment assumes Postgres availability.

### …Redis goes away

- API server logs warnings (`router.go:244` "rate limiting disabled: REDIS_URL not configured"), and falls back: empty-claim cache disabled, PAT cache disabled, realtime relay disabled, rate limiter disabled. Single-node deployments work without Redis; multi-node deployments need it for the relay (otherwise event fan-out is per-node only).

### …Network partition between daemon and server

- Daemon keeps trying to heartbeat. Tasks stay in `queued`/`running` for their respective TTLs, then sweeper cleans up. New tasks pile up in `queued`; the 2h queued TTL is the safety valve (`runtime_sweeper.go:52`).

### Bottlenecks first to bite

1. **Single-instance `events.Bus`**: in-process pub/sub doesn't cross nodes. The Redis Streams relay was added (MUL-1138, mentioned in `events/bus.go:18`) precisely because in-process events don't reach other server replicas. Until that's wired everywhere, multi-replica deployments will silently miss events.
2. **`agent_task_queue` row contention** on hot enqueue + claim cycles. The partial unique index on pending tasks per issue serializes on the same issue. Probably fine; would feel it at thousands of issues each enqueueing many tasks.
3. **WS dedup is per-Hub**, capacity 128 (`hub.go:146`). Bursty senders to the same client could blow past this. With Redis relay turned on, dedup is critical because the same event may come in twice (local fast path + Redis relay).

---

## 12. Anti-patterns / smells

What's already showing strain:

1. **`agent_task_queue` is overloaded.** It carries `issue_id`, `chat_session_id`, `autopilot_run_id`, `quick_create_*`, `trigger_comment_id`. Five different task "kinds" share one table. The columns added in migrations 020, 026, 042, 055, 058, 061, 091 are mostly null-on-the-wrong-kind. This is a classic "single-table-multi-aggregate" smell. **Lesson for ainb**: split issue-task vs chat-task vs autopilot-task or use a `kind` column with a JSONB `params` instead of N nullable typed columns.

2. **Heartbeat-as-action-queue.** `handleHeartbeatActions` (`daemon.go:1279`) reads `PendingUpdate`, `PendingModelList`, `PendingLocalSkills`, `PendingLocalSkillImport[]` from a heartbeat response. Each is a different async report flow with its own retry table. This grew organically — `runtimeReportBackoffs` in `daemon.go:1449` is reused across all of them — but the server is essentially using HTTP heartbeat polling as a control channel. If a new control verb shows up, it gets bolted onto the heartbeat ack instead of getting its own endpoint.

3. **CLI sub-command sprawl.** `server/cmd/multica/cmd_*.go` has 30+ files. The CLI is now its own product surface; conventions are inconsistent (e.g. `cmd_setup_test.go` is much larger than `cmd_agent_test.go`).

4. **Compat shims for desktop versioning** (`handler/onboarding_shim.go`) — the API has dated v3 shims because installed Electron apps lag the server. This is *correct* — they explicitly document the rule in CLAUDE.md ("API Response Compatibility" section, with `parseWithFallback` + zod). But it does mean the API has parallel paths now, and removal requires monitoring `X-Client-Version` telemetry. Cost of a desktop app.

5. **Reserved-slug list maintenance**. Five separate audit migrations (043, 045, 047, 049, 056) re-check that the reserved-slug list still excludes new routes. This is brittle; every new top-level route is a future audit migration. The CLAUDE.md note says "New global routes MUST use a single word or `/{noun}/{verb}` pair" precisely because they got bitten.

6. **Multiple "stores" per concern.** `LocalSkillListStore`, `LocalSkillImportStore`, `ModelListStore`, `UpdateStore`, `LivenessStore`, `WebhookRateLimiter`, `WebhookIPRateLimiter` — each is an interface with an in-memory implementation for single-node and a Redis implementation swapped in by router.go:147-153. This is a *good* pattern but it's repeated by hand 7 times. A cleaner abstraction (a single `KVCache[T]` with typed accessors) would compress that.

7. **`triggerRestart` for self-update from inside the same process.** `daemon.go:1712` shells out to a new binary path after cancelling its own context. It works, and the auto-update barrier (`pauseClaims`/`claimsInFlight`) is tight, but the dance to detect brew vs direct-download + Linux Cellar path quirks (`daemon.go:1721-1735`) is genuinely intricate code that exists only because the daemon updates itself.

8. **Two-layer secrets**: server has its own JWT secret + PAT secret + daemon token. CLI has its own config. Desktop has its own creds-via-IPC bridge. There is no single secret manager.

---

## 13. Architectural verdict — what to copy, what to refuse

### Copy wholesale

1. **The two-WS-hub pattern.** One Hub for user clients (scoped subscribe to workspace/user/task/chat), one Hub for daemons (scoped to runtime_id with `runtime_gone` recovery). This is *the* right shape for a system that has long-lived browser clients AND long-lived agent runners.

2. **Polymorphic `assignee_type` + `assignee_id`.** `(actor_type, actor_id)` is how you make agents first-class. Worth the FK loss.

3. **The task lifecycle state machine with idempotent finalize.** Specifically:
   - `queued → dispatched → running → completed/failed/cancelled`,
   - 90s "reclaim stale dispatched" window for lost claim responses,
   - 5min `dispatched` TTL via background sweeper,
   - 2h `queued` TTL safety valve,
   - partial unique index `WHERE status IN ('queued','dispatched')` for at-most-one-per-issue,
   - `attempt`/`max_attempts`/`parent_task_id`/`failure_reason` for retries.

   Every one of those exists because of a real failure mode. Take all of them.

4. **Per-task isolated env directories with `.gc_meta.json`** for daemon GC. Cheap, surprisingly powerful. Cleaner than containers when you don't need network isolation.

5. **Skills as DB rows materialized into the env at dispatch time.** Lets users curate skills in the UI and run them under different agent CLIs without bespoke per-provider config.

6. **Curated agent templates embedded in the binary.** No DB seed, no admin UI, PR-reviewed. `agenttmpl/loader.go` is 100 lines and does exactly the right thing.

7. **Workspace-scoped multi-tenancy with `X-Workspace-ID` header routing** and `member.role` for RBAC. Linear-style. Boring, correct.

8. **Dual heartbeat (WS suppresses HTTP, HTTP resumes on WS drop)**. Belt-and-braces redundancy with no double-counting.

9. **The "API Response Compatibility" rule from CLAUDE.md** — `parseWithFallback` with zod schemas at every API boundary, defensive optional-chaining everywhere, never `as T`. This is the only sane defense for installed desktop apps.

10. **Idempotent task finalization**: if `CompleteTask`'s update matches zero rows, look up the row, and if it's already in a terminal state, return success. Same for `CancelTask`. Saves you from the WS-vs-HTTP race every time.

### Copy with modification

1. **`events.Bus` + Redis Streams relay**. The in-process bus is fine. The Redis relay (envelope with `event_id` ULIDs, per-scope streams, per-node node-set hash, heartbeat keys with TTL) is *correct* but overengineered for ainb's likely scale. Start with in-process; add the relay only when you have a multi-node deployment. Keep the `Broadcaster` interface from day one so the swap is local.

2. **Daemon as same-binary-different-verb**. Borrow the idea. But don't make the daemon self-update via the same binary path (`triggerRestart`'s brew/Cellar dance is too much). Defer auto-update to the OS package manager and just exit.

3. **Hardcoded provider list in `daemon/execenv/`**. Acceptable for v1, but design with the assumption that you *will* need a provider registry. Their `agent.go` already half-abstracts this (`DetectVersion`, `CheckMinVersion`, `ListModels`, `ModelSelectionSupported`); push that further so a provider is a struct, not a switch statement.

4. **PAT + daemon token + JWT trifecta**. Simplify to two: `user_token` (PAT, opaque) and `service_token` (daemon, cluster-shared if needed). The third (JWT cookie) is a web-app convenience that mobile/CLI don't use.

### Refuse to copy

1. **Single `agent_task_queue` for five task kinds.** Split: `issue_task`, `chat_message_task`, `autopilot_task`, `quick_create_task`. Share a common base view if you need a unified board. Don't accept the column sprawl.

2. **Heartbeat ack carrying pending actions.** Give each control verb its own endpoint or its own WS message type. Don't bolt control commands onto heartbeats.

3. **Reserved-slug list pattern**. Use a single `/v/<word>` or `/w/<slug>/...` prefix and dodge the problem. Don't audit a JSON list per migration.

4. **One huge sqlc package** (`server/pkg/db/generated`). Multica has 98 migrations and a single regenerated `db/queries/` directory. Split by aggregate from the start — `db/issue/`, `db/task/`, `db/agent/`, etc. — so the regeneration blast radius stays small.

5. **`runtimeReportBackoffs`-as-shared-retry-policy.** That `reportRuntimeResultWithRetry` pattern (`daemon.go:1488`) is reasonable, but it's reused for 4+ different reports each with different SLAs. Build a typed retry policy upfront.

6. **The "shim handler in /api/me/onboarding/runtime-bootstrap" pattern.** They had to write `onboarding_shim.go` to support pre-v3 desktops. The lesson is *not* "write shims gracefully" — it's "make the onboarding flow a pure-frontend choreography over generic backend primitives so old clients keep working without shims."

7. **PostHog as the only product-analytics path.** It's fine, but it's the only one. Add OTEL traces from day one of ainb — backfilling is brutal at this scale.

### Take-home synthesis

Multica's architecture is **good but not novel**. The novel parts are the *product decisions encoded in schema* — polymorphic actors, per-runtime task routing, skills-as-data-materialized-at-dispatch, curated-templates-in-binary, the bimodal heartbeat. The infrastructure is competent Go with `net/http` + Chi + `sqlc` + `gorilla/websocket` + optional Redis. There is no microservice mesh, no event sourcing, no CQRS — it's a textbook "modular monolith with a workhorse Postgres" and that's part of why it ships.

The thing they got *most right* for v1 OSS is making the **daemon, CLI, and server be one Go binary with three subcommand surfaces**. That's the leverage move. It collapses release management, dependency management, and onboarding into one artifact.

The thing they'll regret most by v2 is **`agent_task_queue`**. It's becoming an event table by accident. The first big refactor I'd predict is splitting it.
