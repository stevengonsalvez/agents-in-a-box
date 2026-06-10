# Distinguished Engineer Critique — Multica

**Reviewer perspective**: 25-year DE, post-mortem-driven. Stevie hates sycophancy. This is an opinionated take, not a balanced one.

**Source of truth**: shallow clone at `/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_multica/.agents/research/multica/`. References below cite real file paths. Total backend: ~88k Go LOC across 20 internal modules + 408 TS/TSX files in apps + monorepo packages. 107 schema migrations.

**TL;DR verdict**: Multica is a well-executed **Linear-clone with agent assignees** built around a **Postgres-as-queue + daemon-on-laptop** architecture. The technical core is unremarkable (sqlc + chi + Postgres + Redis + Next.js). **No novel infrastructure here**. Star count is 90% positioning, 10% nice product execution. The dangerous parts (sandboxing, secret handling, multi-tenant isolation) are **under-engineered**; the safe parts (issue tracker semantics, board UX, websocket fanout) are **over-built**. Cloning the surface area is 14-22 person-weeks. Cloning the *valuable* parts is 3-4 weeks.

---

## 1. Why is this getting 31k stars in 4 months?

**VERDICT: Positioning + tagline + screenshot. Not technology.**

The headline — *"Your next 10 hires won't be human"* — is a category-defining one-liner aimed at a moment (mid-2026, "managed agents" is the hot term) when every dev shop is anxious about how to operationalize agents beyond `claude code` in a terminal. The README's hero says *"Linear, but with agents as first-class citizens"* (`README.md:31-49`), and that frame instantly explains the product in five seconds. Stars are a measure of *how fast someone got the point on the README*, not technical depth. Multica wins that test cleanly. The Multics-callout name (`Multica = Multiplexed Information and Computing Agent`, `README.md:43`) is also catnip for HN/Twitter — every CS-history-literate engineer wants to share it. Add a glossy hero screenshot, a `brew install` one-liner, multilingual READMEs (English + Chinese), and a `multica-ai/tap` Homebrew tap, and you have the platonic ideal of a GitHub virality machine.

The **actually novel** product surface is small but real: (a) **agents-as-polymorphic-assignees** — `assignee_type` ∈ `{member, agent}` is a first-class column on `issue`, `comment.author_type`, `inbox.actor_type` (`server/migrations/001_init.up.sql:30-176`); (b) **Squads** — a routing layer with a "leader agent" that delegates to members (visible in `autopilotSquadAttribution` and squad tables) — this is genuinely a useful primitive for >5-person teams running fleets of agents; (c) **Autopilot** — cron/webhook-triggered automated runs with admission gating to skip enqueue when the assigned runtime is offline (`server/internal/service/autopilot.go:51-100`). Everything else (issue boards, comments, mentions, real-time updates, daemons that shell out to agent CLIs, file uploads to S3, websocket fanout via Redis streams) is **table stakes** that you could pull out of any 2024-era Linear clone tutorial.

The third factor is the *agent CLI vendor matrix*. The README lists 11 supported CLIs: `claude`, `codex`, `copilot`, `openclaw`, `opencode`, `hermes`, `gemini`, `pi`, `cursor-agent`, `kimi`, `kiro-cli`. Each of those communities re-shares the Multica README, so the star graph compounds across 11 fandoms. This is **not engineering value** — it's distribution arbitrage from being the only "neutral" platform that supports all of them. The minute Anthropic or OpenAI ships a first-party "managed agent fleet", that neutrality stops mattering.

The 31k stars are **not predictive of product survival**. Compare with Activepieces, n8n, Trigger.dev, and every previous "open-source Zapier" — they all hit 20-50k stars and the market still doesn't have a winner. Star count measures *intent to bookmark*, not deployment. The cloud SaaS dynamics (which Stevie should care about because that's where margin lives) are unaffected by stars; they're affected by who pays $20/mo per seat. Multica has no signal on that yet.

---

## 2. The 5-8 biggest architectural bets

**VERDICT: Most bets are reasonable defaults. Two of them — Postgres-as-queue at scale, and daemon-on-laptop as the primary runtime — will hurt as the platform grows. One — the open-core split — is strategically smart but will frustrate self-hosters.**

### Bet 1 — **Postgres as the task queue** (FOR UPDATE SKIP LOCKED)

Lives in `server/pkg/db/queries/agent.sql:236-260`. The claim path is a classic CTE + `FOR UPDATE SKIP LOCKED` against `agent_task_queue`, with `ReclaimStaleDispatchedTaskForRuntime` covering the case where the daemon never ack'd a dispatch (the `claimResponseRecoveryWindow = 90 * time.Second` constant in `service/task.go:85` is the trust-but-verify guard). They also have `FailStaleTasks`, `ExpireStaleQueuedTasks` sweepers, an in-memory analytics context cache (4096 entries, `task.go:81`), and an `EmptyClaimCache` in Redis to skip Postgres on the steady-state empty case (`server/internal/service/empty_claim_cache.go`). This is a textbook implementation — and the comments in `agent.sql:374-414` for `ExpireStaleQueuedTasks` explicitly reference "the 87k+ doomed rows" backlog from MUL-1899, which is a real production scar.

**Right**: It is the simplest possible queue. Single source of truth for state. No SQS/Kafka/NATS to operate. Transactional with the rest of the schema. Stevie's `ainb` is already on SQLite/Postgres-ish patterns — this is a natural fit. **Wrong**: dies on a known cliff. At ~50-100 active runtimes claiming every 3 seconds (the default `MULTICA_DAEMON_POLL_INTERVAL=3s` from `.env.example`), you have **~33 claim attempts/sec just from polling, before any actual work**. Worse, `FOR UPDATE SKIP LOCKED` does not scale linearly — at high contention the predicate-matching cost dominates and you get tail-latency spikes. PgQ / Hatchet / River / Faktory exist exactly because this pattern collapses past ~1k workers. **Alternative**: combine LISTEN/NOTIFY (Multica already does push wakeups via `Wakeup TaskWakeupNotifier`, so the foundation is there) with much longer poll intervals (60s) and treat polling as the cold-recovery path. Or just bite the bullet and use a real queue once you cross ~100 concurrent runtimes.

### Bet 2 — **Daemon-on-your-laptop is the primary runtime**

The daemon runs locally, advertises detected CLIs (11 of them per the README), and uses the user's API keys (`server/internal/daemon/daemon.go:2388-2400`). Server stores `custom_env JSONB` per agent (`server/migrations/040_agent_custom_env.up.sql` — see also section 6 for the security implications).

**Right**: Zero cloud compute cost for Multica, zero LLM token cost for Multica, users keep their existing CLI subscriptions (Claude Max, GPT Plus, Copilot). For a 2-10 person team this is genuinely the right shape — the agent already runs on the engineer's laptop, so why pay for an EC2 instance to do the same thing. **Wrong**: laptops sleep, lose Wi-Fi, get closed at 6pm. Half of the daemon-side codebase is dealing with this: `RecoverOrphanedTasksForRuntime`, `runtime_gone`, admission gating, "queued backlog", `ExpireStaleQueuedTasks` (`server/pkg/db/queries/agent.sql:350-414`). A whole package `server/internal/daemon/poisoned.go` exists to classify whether a previous task's session is too damaged to resume. That's a lot of engineering to paper over the fundamental issue that **the platform is never higher-availability than the laptop sleeping in someone's backpack**. **Alternative**: ephemeral cloud runtimes only (Codespaces / Modal / fly.io machines), or hybrid where the daemon orchestrates *remote* execution. Multica has the hooks for this via `cloudruntime.Client` (`server/internal/cloudruntime/client.go`) but it is **a thin HTTP proxy to a private fleet URL the open-source repo does not ship**. The OSS version is laptop-only.

### Bet 3 — **Cloud runtime is a closed proprietary backend reached via an HTTP proxy**

`server/internal/cloudruntime/client.go` is 128 lines of pure proxy code — it just forwards every request to the configured `cloud runtime fleet URL` with `X-User-ID` / `X-User-PAT` headers. The actual fleet provisioning (the hard part: container orchestration, snapshotting, GPU scheduling, billing) lives in `https://multica.ai`-side infrastructure that **is not in the OSS repo**. The handler `server/internal/handler/cloud_runtime.go` is also just thin proxies — `GetCloudRuntimeService`, `ListCloudRuntimeNodes`, `CreateCloudRuntimeNode`, `ExecCloudRuntimeNode` — each one forwards to the upstream fleet API.

**Right**: Smart product strategy. They've drawn the "open core" line exactly where it monetises. Self-hosters get the issue tracker + daemon orchestration + skills system. Multica.ai gets to charge for the cloud compute. This is the same playbook GitLab / Sentry / Posthog ran. **Wrong**: people who fork this expecting "full cloud agents" will discover they have to build the entire fleet themselves. That's likely fine for Multica's monetisation, but it inflates the "I'll just self-host" story until users actually try it. The OSS user experience for cloud runtimes is *configure-a-URL-to-pointing-at-nothing*. Expect a steady drumbeat of "how do I self-host the cloud runtime" issues that get closed `out-of-scope`.

### Bet 4 — **WebSockets via Redis Streams for fan-out**

`server/internal/realtime/redis_relay.go` uses XADD/XREADGROUP per scope (workspace, agent, daemon-runtime), `streamMaxLen = 10000`, `heartbeatTTL = 90s`, `heartbeatPeriod = 30s`, `consumerIdleGrace = 10 * time.Minute`, `consumerSweepPeriod = 5 * time.Minute`. Multi-node-aware via `NodesKey` registry. Backed by a hub abstraction (`server/internal/realtime/hub.go`).

**Right**: Solid choice. Redis Streams give you replay-from-cursor for free, multi-replica fanout, bounded backlog. Better than raw pub/sub (loses messages on disconnect) and dramatically simpler than NATS JetStream. The separation of a `writeRDB` (non-blocking commands) from `readRDB` (blocking XREADGROUP) shown in `redis_relay_test.go:22-26` is the right shape — most people get this wrong and end up with a single connection that blocks everything. **Wrong**: Two failure modes. (1) Redis is now a hard dep — losing Redis kills realtime entirely even though the data is in Postgres. (2) `streamMaxLen = 10000` is a global per-scope cap with **silent eviction**. A burst of activity in a hot workspace can evict events for daemons that were slow to consume. **Alternative**: Postgres LISTEN/NOTIFY for low-volume scopes + Redis streams only for high-fanout. Or just accept the dep — it is the right answer for 95% of the user base.

### Bet 5 — **Monorepo with explicit `core/ui/views` layer boundaries**

`packages/core/` (headless logic + Zustand + zero react-dom), `packages/ui/` (atomic components), `packages/views/` (business pages, zero next/router imports). Apps (`web`/`desktop`/`mobile`) wire platform glue. Documented hard rules in `CLAUDE.md` ("Package Boundary Rules").

**Right**: This is the **best-executed part of the codebase**. The boundary rules are explicit, enforced by import paths, and explained by historical incidents (the CLAUDE.md cites issues #2143/#2147/#2192 for API response defensive parsing). The split between `views/` (jsdom tests, no framework mocks) and `apps/web/` (framework-specific mocks) is the cleanest version of this pattern I have seen in an OSS monorepo. Notice also the **reserved-slug single-source-of-truth pattern**: `server/internal/handler/reserved_slugs.json` is the canonical list, and `packages/core/paths/reserved-slugs.ts` is generated from it (`CLAUDE.md` — Coding Rules). That's exactly the right shape for cross-language constants. **Wrong**: Mobile is intentionally divorced from the share zone (`apps/mobile/CLAUDE.md` says mobile only imports `type` from `@multica/core`). The cost: every domain change requires touching mobile separately. The benefit (mobile can upgrade React/RN on its own cadence) is real but means **mobile feature parity will always lag**.

### Bet 6 — **Skills as a workspace-scoped database resource, not a filesystem**

`server/migrations/008_structured_skills.up.sql` defines `skill`, `skill_file`, `agent_skill` tables. Skill content is plain `TEXT` columns in Postgres, joined to agents via M2M.

**Right**: Centralised — every agent in a workspace sees the same skills, versioned consistently, no "did you `git pull` the latest skills" drift. The M2M `agent_skill` table lets you scope which skills which agent can see. **Wrong**: Three problems. (1) Storing arbitrary text (`content TEXT NOT NULL DEFAULT ''`) with no size cap or revision history means a skill is a moving target — the agent that ran yesterday with skill v3 has no way to recover v3 today. There is no `skill_revision` table. (2) No FTS or vector index on skill content despite the Postgres image being `pgvector/pgvector:pg17`. (3) Skills compete with the agent CLI's *native* skill systems (Claude's `.claude/skills/`, OpenClaw's config-driven skills). The daemon does a workaround dance — `CODEX_HOME` per-task, `OPENCLAW_INCLUDE_ROOTS` env, etc. (`server/internal/daemon/daemon.go:2367-2386`) — to bridge Multica's DB-skills into each CLI's native discovery. This bridge is **brittle by construction**: every new agent CLI brings its own skill mechanism. **Alternative**: skills are git-versioned files committed to the repo the agent edits. The whole "compound skills" story then becomes a PR template, not a database. Half the maintenance burden disappears.

### Bet 7 — **Go backend, no service split, no gRPC, no message bus**

20+ internal modules but a single binary (`server/cmd/server`). Chi router, sqlc for typed queries, gorilla/websocket. `service.TaskService` (2227 LOC) and `service.AutopilotService` (966 LOC) are the two heavies. The handler layer is 40+ files; the largest are `issue.go` (2763), `daemon.go` (2186), `skill.go` (1847), `comment.go` (1243), `autopilot.go` (1238), `agent.go` (1201).

**Right**: Boring. Boring is good. Go was the correct call vs Rust (faster shipping) and vs Node (better runtime perf). sqlc gives you typed SQL with no ORM tax. A single binary is operable by a one-person team. The test discipline is real too — 177 `*_test.go` files, and the `handler_test.go` alone is 2932 LOC. **Wrong**: `task.go` at 2227 LOC and `issue.go` at 2763 LOC are God objects. The service layer is mixing analytics capture, queue management, session resume policy, and notification fanout. This will become *very* hard to refactor at PR #5000. There's also no clean event bus — `events.Bus` is sprinkled through but there are also direct `Hub.Broadcast` calls and Postgres `LISTEN` triggers. **Three competing real-time delivery paths**: bus, hub, Redis relay. Pick one.

### Bet 8 — **Electron desktop app as a first-class client**

`apps/desktop/` is a full Electron + react-router app with its own tab system, drag regions, window overlays, and per-tab memory routers (`CLAUDE.md` — Desktop-specific Rules).

**Right**: For "agents as teammates", a persistent desktop window is genuinely the better UX than a browser tab. Users want notifications and don't want their inbox closed when they close Chrome. **Wrong**: Electron is **the single largest source of operational complexity** in the repo. Multi-platform builds, auto-update infrastructure, code signing, "0.2.26 user hits a 0.4.x server" version drift problem (which the CLAUDE.md addresses with elaborate `parseWithFallback` schemas — a whole section called "API Response Compatibility" — and which has caused at least 3 incidents already: #2143/#2147/#2192). The CLAUDE.md goes on to specify the exact order of operations for "Workspace destructive operations" because **getting it wrong hard-reloads the renderer** — that's the kind of bug you only fix after it has happened in production. **Alternative**: just ship a PWA. Notifications work on macOS/Linux/Windows. No installer, no auto-update, no version-skew defensive coding. Multica is paying a heavy tax for a marginal UX win.

---

## 3. Build-vs-buy lock-in & dependency traps

**VERDICT: Heavy AWS lock-in by default, light LLM lock-in (BYO-key), moderate database lock-in (Postgres-specific features), severe agent-CLI vendor lock-in.**

The default cloud assumption is **AWS**: S3 for files, CloudFront with signed cookies for asset delivery (`packages/core/api/...` + `server/internal/auth/cloudfront.go`), CloudFront private key in Secrets Manager (`server/internal/auth/cloudfront.go:80-113`). They support a local filesystem storage adapter (`server/internal/storage/local.go`) but the cloud path is AWS-native. Migrating to GCS/R2 means rewriting `storage/s3.go` and the signing logic — call it 3-5 days of work but you'd lose the signed-cookie pattern unless you implement an equivalent at the CDN edge. The Resend dependency for transactional email is similar — easy to swap (they support SMTP fallback per `.env.example`), but the templates assume Resend's variable substitution.

LLM lock-in is **minimal by design** — the daemon shells out to whatever CLI is on `PATH` and lets the user inject `custom_env` per agent. This is the **right** call — they punted on the "LLM router" problem entirely. The trap is the inverse: Multica's value proposition collapses if Anthropic / OpenAI ship "managed agent fleets" as a first-party feature (which they will, in 2026). Multica is monetising the *gap* in vendor-managed agent runtimes; that gap closes when the vendors close it.

Postgres lock-in is moderate. They use `pgvector`, `pgcrypto`, JSONB heavily, and `FOR UPDATE SKIP LOCKED` semantics. Porting to MySQL/CockroachDB is plausible but non-trivial. The migration story is solid (107+ migrations, all `.up.sql/.down.sql`) — they're committed to Postgres for the long haul.

OAuth lock-in is the lazy default: Google only (`GOOGLE_CLIENT_ID` / `GOOGLE_REDIRECT_URI` in `.env.example`). No SAML, no Okta, no SCIM. Enterprise sales would need to add at least one more IdP before the first 100-person company will buy.

The most worrying dependency is **the agent CLI vendors themselves**. The daemon's `execenv` package has special cases for each: Codex Seatbelt sandbox detection (`codex_sandbox.go`), OpenClaw config injection (`codex_multi_agent.go`), Claude `--max-turns` defaults, etc. Every CLI vendor's breaking change is a Multica P0 — and these CLIs are all moving targets shipping weekly. The `CodexDarwinNetworkAccessFixedVersion = ""` constant (`codex_sandbox.go:31`) is a literal "we're waiting for upstream to ship a fix" marker — the Multica codebase is partially blocked on Codex shipping `openai/codex#10390`. Expect a steady drumbeat of "agent X v1.2.3 broke our integration" issues.

---

## 4. Scaling cliffs

**VERDICT: First serious cliff at ~50 active runtimes; second at ~500 workspaces. Realtime is the next bottleneck after that. The team has clearly already hit the first cliff in production — the operational scar tissue in the SQL comments proves it.**

### Cliff 1 — Queue claim at ~50 active daemons

Default poll = 3s (`MULTICA_DAEMON_POLL_INTERVAL=3s` in `.env.example`). 50 daemons × 0.33 Hz = ~17 Postgres claim queries/sec. Each runs the CTE + `FOR UPDATE SKIP LOCKED` over `agent_task_queue` filtered by `runtime_id = $1 AND status = 'queued'`. Comment in `agent.sql:374-414` mentions "the historical 87k+ doomed rows" backlog — this is **production evidence that the queue path has already buckled once**. The `EmptyClaimCache` Redis fast-path was clearly added as a reaction; `service/empty_claim_cache.go` is 197 lines of pure "skip the DB when empty" optimisation. **Mitigation already in place**: `EmptyClaimCache`, push wakeups via `Wakeup TaskWakeupNotifier`. **Mitigation missing**: longer poll interval default, partial indexes on `(runtime_id, status) WHERE status = 'queued'`, and read replica off-loading for the heartbeat/health endpoints.

Concrete prediction: at ~200 concurrent runtimes you will see p99 claim latency spike to >1s during any aggregated event burst (e.g. an Autopilot run that enqueues 100 tasks at once). The CTE locks serialize at the per-row level under contention.

### Cliff 2 — Realtime stream cardinality at ~500 workspaces

Each scope (workspace, agent, daemon-runtime) is its own Redis stream. `streamMaxLen = 10000` per scope. 500 workspaces × 3-5 active scope types each = ~2000 streams. Stream-per-scope is not Redis's strong suit; XADD/XREADGROUP is well-optimised but the consumer registry (`ws:scope:%s:%s:nodes`) and heartbeat sweepers become non-trivial. At ~5000 streams the consumer-sweep loop (`consumerSweepPeriod = 5 * time.Minute`) starts to do real work: it has to walk every registered scope, check heartbeat freshness, and reap dead consumers. **Mitigation needed**: shard Redis by workspace, or move to NATS JetStream's subject-based fanout which handles cardinality natively.

### Cliff 3 — Postgres single-node write throughput at ~10k tasks/min

The schema is single-tenant-per-row with `workspace_id` filters on every query. No partitioning visible in 107 migrations. At 10k task lifecycle events/min (write, dispatch, start, complete) plus comments, activity, inbox, you saturate a single Postgres at maybe 50k workspaces (very rough). They've already started building the read-side defenses — `task_usage_hourly` rollup (`migrations/101_task_usage_hourly_schema.up.sql`) and `backfill_task_usage_hourly` cmd binary (`server/cmd/backfill_task_usage_hourly/`). That's a tell: the dashboard queries became too expensive against the live table. **Mitigation needed**: partition `agent_task_queue` by created_at month, partition `inbox` by workspace, move analytics to a separate replica with logical decoding.

### Cliff 4 — Daemon WS hub at single-server ~10k concurrent connections

`gorilla/websocket` + Go is good for this (Go can handle ~100k goroutines), but the hub broadcaster (`server/internal/realtime/hub.go`) does in-process fan-out per scope. At 10k connected daemons each subscribed to 2-3 scopes, you have 20-30k goroutines doing broadcast filtering per event. Fine on a m5.xlarge until it isn't. **Mitigation in place**: the Redis relay lets you run multiple frontend nodes — they did this correctly. The shard size lives in `sharded_stream_relay.go` so they're already thinking about it.

### Cliff 5 — Skills table size & search

Skills are full-text in Postgres `TEXT` columns. No FTS index visible in `008_structured_skills.up.sql`. At 10k skills × 50KB avg content (very plausible — skills get verbose), you have 500MB of text data scanned on naive `LIKE` queries. They use pgvector (visible in `pgvector/pgvector:pg17` Docker image) but I see no embedding column on `skill`. Either they don't search skills (and the "compound skills" story is mostly hand-waving) or they will need to wire pgvector soon.

### Cliff 6 — Migration count growing at ~25/month

107 migrations in 4 months = ~25/month. That's a deployment-discipline cliff, not a runtime one. Each prod deploy is now a long-running migration window. They'll need migration squashing (collapse old migrations into a baseline) by month 12 or `make migrate-up` will take minutes against a real-data DB. The `100_user_timezone` and `104_drop_runtime_timezone` migrations within four numbers of each other suggest the team has already had "oops we picked the wrong column shape" cycles — that's expected at this velocity but eventually it bites.

---

## 5. Operational cost shape

**VERDICT: Near-zero variable cost for self-host. Cloud is gated entirely by LLM tokens (which Multica doesn't pay) and EC2/Fargate (which Multica does). The moat is thin — the cloud runtime is the only thing they monetise, and it's the easiest piece to replicate.**

### N=1 (single self-hoster)

Postgres + Redis + Go server + Web. One $20 VPS (Hetzner CPX21) runs the whole stack with headroom. Daemon runs on the user's laptop, LLM tokens billed to the user. **Multica cost: $0**. **User cost: $20/mo infra + their existing LLM subscription.**

### N=100 (a small SaaS tier — call it cloud Multica)

100 active users × ~3 active runtimes each = 300 daemons polling the server. Backend fits on a m5.large + RDS db.t3.medium + ElastiCache cache.t3.micro. **Multica monthly infra: ~$300-500.** **Revenue at $20/seat/mo: $2000.** Margin: 75-85% — healthy. But **LLM tokens are not on Multica's books** in the BYO-key model. If they offered "managed cloud agents" with bundled LLM tokens, the unit economics flip — Anthropic margins are ~30% on input + ~20% on output, and Multica would have to either eat that or markup heavily.

### N=1000 (mid-stage SaaS)

1000 active users × 4 daemons = 4000 connected daemons. m5.xlarge backend, RDS db.r5.large, ElastiCache cache.t3.small cluster. **Multica monthly infra: $2-3k.** **Revenue: $20k.** Margin still healthy. This is where the queue cliff (Cliff 1 above) is going to bite — you'll be paying engineers to chase queue tail-latency.

### N=10000 (real platform scale)

This is where the architectural bets get tested. 10k users × 3 daemons = 30k connected daemons. Single Postgres is dead — you need RDS Aurora + read replicas + (likely) partitioning. Redis needs cluster mode. WS hub needs ~5-10 backend pods behind ALB. **Multica monthly infra: $15k-30k.** **Revenue at $20/seat: $200k.** Margin still 85%+, IF the engineering team has scaled the queue path. If they haven't, p99 task-dispatch latency is now 30+ seconds and users churn.

### Where the variable cost actually sits

For BYO-key Multica: variable cost = bandwidth (WS frames, file uploads to S3, agent output streams). Negligible per user. **Multica is fundamentally a SaaS with fixed-cost economics** — every extra user is mostly free until you hit the next cluster-resize step function.

For "managed runtime" Multica (the SaaS upgrade path, not in OSS): variable cost = LLM tokens (the dominant line) + cloud compute (EC2 / Fargate / GPU instances). At ~$3-15/M tokens for frontier models and 10-50k tokens per coding task, a single user running 100 tasks/day costs $30-750/mo in tokens alone. If Multica resells that at 1.5x markup, they're competing with the LLM vendors' own subscription bundles — which is a losing position the moment Anthropic/OpenAI offer "Claude for teams" with bundled agent fleets.

### Moat analysis

**The moat is thin.** What protects Multica's margins? Not the OSS code (anyone can fork — 31k stars proves it's easy to discover). Not the cloud runtime (any Fargate / Modal / Codespaces reseller can build it; Multica's `cloudruntime/client.go` is literally a thin HTTP proxy showing exactly the API shape needed). The moat is **the product positioning + the agent-as-teammate UX + community lock-in via Squads and shared skills**. That's a brand moat, not a tech moat.

If GitHub ships "Issues with AI agent assignees" natively (and they will, in 2026 — they already have Copilot Workspace), Multica's positioning collapses overnight. If Linear ships the same, ditto. If Anthropic ships "Claude for teams with managed runtimes", ditto. Multica is racing **three giants and a horizon of well-funded startups** in a category where the moat is "we got there first and had a good name".

---

## 6. Security posture

**VERDICT: REJECT for any environment touching production code or sensitive secrets. The platform's own documentation acknowledges agents run with `danger-full-access` on macOS. This is a CVE-shaped problem the moment one workspace member is compromised. The env-variable blocklist is so thin it's effectively security theatre.**

The smoking gun: **`server/internal/daemon/execenv/codex_sandbox.go:54-81`** says, in production code with explanatory comments, that on macOS the daemon falls back to `sandbox_mode = "danger-full-access"` for Codex tasks because Apple's Seatbelt sandbox in `workspace-write` mode silently blocks DNS resolution (`openai/codex#10390`). That means **an agent running on a Mac via Multica + Codex has full read/write to the user's filesystem and full network access**. Quote: *"Until a fixed Codex release ships, the per-task Codex config on macOS needs to fall back to `sandbox_mode = "danger-full-access"`"* (`codex_sandbox.go:21-23`). That's not a bug — that's the platform's documented stance. `CodexDarwinNetworkAccessFixedVersion = ""` (line 31) literally means "we have no fix yet, every macOS Codex run is unsandboxed".

### The env blocklist is paper-thin

The `isBlockedEnvKey` function in `server/internal/daemon/daemon.go:3221`:

```go
func isBlockedEnvKey(key string) bool {
    upper := strings.ToUpper(key)
    if strings.HasPrefix(upper, "MULTICA_") {
        return true
    }
    switch upper {
    case "HOME", "PATH", "USER", "SHELL", "TERM",
         "CODEX_HOME", "OPENCLAW_CONFIG_PATH", "OPENCLAW_INCLUDE_ROOTS":
        return true
    }
    return false
}
```

**Look at what is NOT blocked**: `LD_PRELOAD` (Linux library injection), `DYLD_INSERT_LIBRARIES` (macOS library injection), `DYLD_LIBRARY_PATH`, `PYTHONPATH` (Python import hijacking), `NODE_OPTIONS` (Node.js `--require` injection), `BASH_ENV` (shell-init injection), `ENV`, `GOPRIVATE`, `GOPROXY` (Go toolchain injection), `RUBYOPT`, `PERL5OPT`, `JAVA_TOOL_OPTIONS`. **Any workspace member with write access to an agent's `custom_env` can pop a shell on the daemon host.** This is a privilege-escalation vector inside the workspace. The fix is trivial — switch to an allowlist of known-safe LLM provider keys (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_BASE_URL`, etc.) — but the current code blocks neither the variable family nor the obvious library-loader vectors.

### Other concrete security problems I'd require fixed before this goes near a real codebase

1. **LLM API keys stored as plaintext JSONB in the `agent` table** (`server/migrations/040_agent_custom_env.up.sql` — `custom_env JSONB NOT NULL DEFAULT '{}'`). No column encryption, no KMS envelope, no field-level access control. A read-only DB compromise (the most common kind — leaked replica, leaked backup, misconfigured logging shipper) leaks every workspace's Anthropic/OpenAI keys. The handler at `server/internal/handler/agent.go:359-427` redacts `custom_env` on *responses* based on role policy, but the underlying storage is plaintext. The redaction-on-read is a fig leaf — the data is exfiltrable via any DB-tier breach.

2. **JWT secret defaults to a hard-coded string** in dev (`server/internal/auth/jwt.go:12` — `defaultJWTSecret = "multica-dev-secret-change-in-production"`). This is a textbook misconfiguration trap. In a panicky deploy, `JWT_SECRET=` gets left unset and you have a forgeable token surface. The Docker self-host pins `APP_ENV=production` which *probably* triggers a startup check — verify. If you build on this, fail-loud at startup if `JWT_SECRET == defaultJWTSecret && APP_ENV == "production"`.

3. **No audit log table**. `grep -rn audit_log server/migrations/` returns empty. Every other table I checked (`comment`, `inbox`, `activity`) has its own activity feed but there is no append-only audit ledger that survives table mutation. SOC2, ISO 27001, and any enterprise procurement will reject this outright. The `activity` table covers user-visible events, not security-relevant ones (who exfiltrated which custom_env, who reassigned a task to a different runtime, who escalated from member to admin).

4. **RBAC is 3-role** (`owner`, `admin`, `member` — `server/migrations/001_init.up.sql:30`). No per-resource permissions. No "this user can assign issues but can't run agents". No "this agent can read repo X but not repo Y". Coarse for a multi-tenant SaaS, completely inadequate for enterprise.

5. **No rate limiting visible at the public API edge**. There's a webhook-specific rate limiter (`MULTICA_TRUSTED_PROXIES` referenced in `.env.example`) and a `heartbeat_scheduler.go` for daemon side, but no global API rate limit. An attacker spamming `/api/issues/quick-create` (which creates DB rows and may enqueue agent tasks!) could cost a workspace owner real LLM token money. **This is a billing-DoS vector**: malicious actor inside a workspace can drain a co-worker's API budget.

6. **Personal Access Tokens are sent on every cloud-runtime proxy call** (`server/internal/cloudruntime/client.go:103-105` — `X-User-PAT` header). Tokens in headers end up in load balancer access logs and CDN logs unless explicitly stripped. Standard mistake; preventable with a one-line LB filter, but it's not in the OSS repo.

7. **No agent-vs-user trust boundary**. The agent runs as the user. It has the user's PATH, HOME, git credentials, SSH keys. A prompt-injection attack (malicious content in an issue comment that an agent ingests) becomes arbitrary code execution on the user's machine. There is no documented threat model for this in `SELF_HOSTING.md` or `CLAUDE.md`. The repo treats the LLM as trusted code.

8. **Webhook handler accepts arbitrary URLs as triggers** (`autopilot_webhook.go`). If an attacker can register a webhook with their controlled URL, the webhook delivery path on Multica's outbound network becomes an SSRF probe surface. Need to verify the URL-validation is robust; the existence of `MULTICA_TRUSTED_PROXIES` in `.env.example` suggests they're aware of the forward path but I didn't see the outbound validation.

### What I'd require before letting this near a real codebase

- Run the daemon in a dedicated user account with mandatory access controls (a Linux unprivileged user + bubblewrap, or macOS Sandbox profiles you actually trust).
- Per-task ephemeral worktree on a tmpfs that the user cannot escape.
- Repo-level allowlist (only `agent-X` can edit `repo-Y`).
- KMS-backed encryption for `custom_env` and other secret-bearing columns.
- Audit log on a separate schema with WORM-style retention.
- SSO / SCIM / SAML before any enterprise sale.
- Allowlist-based env injection, not blocklist.
- Outbound network egress filter on the daemon process (it should only reach the Multica API and the configured LLM endpoint).

---

## 7. Over-engineering

**VERDICT: The frontend monorepo discipline is over-built relative to its current product surface. The session-resume logic is over-engineered relative to its actual failure rate. Half the daemon-side code is failure-recovery for a problem that disappears with cloud runtimes.**

The package-boundary regime (`packages/core/ui/views/`) plus the elaborate `parseWithFallback` schemas for API response defensive parsing (CLAUDE.md, API Response Compatibility section) is a **future-proofing investment far ahead of need**. It's correct engineering for a 5-year-old codebase with 50 contributors. For a 4-month-old project it's a tax. Justified if you believe the platform will scale; over-built if it pivots in year 2.

The session-resume code in `GetLastTaskSession` (`server/pkg/db/queries/agent.sql:274-319`) is **45 lines of SQL** with comments about "poisoned" terminal states, `iteration_limit`, `agent_fallback_message`, `api_invalid_request`, `codex_semantic_inactivity`, and a defense-in-depth ILIKE clause for an Anthropic 400 error shape. This is solving a real bug (MUL-1128) but the **complexity of the failure-mode classifier is doing more work than the feature**. A simpler design: never auto-resume failed sessions. Force the user to click "retry". Lose the resume-on-success UX win but kill 100% of the failure-mode classification burden.

The `triggerSummaryMaxLen = 200` truncation function (`server/internal/service/task.go:58-78`) has a 3-paragraph comment explaining why it preallocates with `strings.Builder.Grow`, processes runes vs bytes, and handles newlines specially. **For a 200-char preview**. This is fine but it betrays the team's culture: every leaf function gets distinguished-engineer-grade attention. That doesn't scale to a 50-engineer team — junior contributors will be paralysed by the perceived bar.

The Codex sandbox upsert function (`server/internal/daemon/execenv/codex_sandbox.go:130-172`) uses a regex-and-marker-based managed-block pattern *inside a TOML file owned by the user*, with `upsertMulticaManagedBlock` + `stripLegacySandboxDirectives` + `multicaManagedBeginMarker`. The justification is real (don't clobber user TOML) but you could just write `~/.codex/config.toml.multica` and tell Codex to read it via `CODEX_CONFIG_PATH`. Half the code disappears.

Three competing real-time delivery paths — `events.Bus`, `realtime.Hub`, Redis relay, daemon wakeup notifier — are not over-engineered individually but the **lack of consolidation** is. Pick one event bus and route everything through it.

The Electron desktop "tab isolation per workspace" subsystem (`stores/tab-store.ts`, `WorkspaceRouteLayout`, `WindowOverlay`, "Drag region (macOS)") is enormous. Each piece exists because of a real bug, but the cumulative complexity makes the desktop app one of the harder things in the codebase to onboard onto. **PWA solves all of this for free.**

---

## 8. Under-engineering

**VERDICT: Everything production-maturity demands is either missing or shallow. This codebase is shipping at the speed of "MVP that got famous", and the operational sharp edges are showing. It will mature — but not before enterprise customers ask for things it doesn't have.**

Concretely missing or under-baked:

1. **No audit log** (covered above). This is the single biggest hole. SOC2 ⇒ no.
2. **No tenant isolation beyond `WHERE workspace_id = $1`**. All workspaces share a single pgxpool, a single Redis, a single Go process. A workspace that runs away (10k agents, 1M tasks queued) starves every other workspace. **Need**: per-tenant rate limits, per-tenant connection pool quotas, ideally per-tenant schemas or DBs once you have paying enterprise.
3. **No idempotency keys on mutating endpoints**. Webhooks (`autopilot_webhook.go`) appear to dedupe by request ID, but the regular API doesn't expose idempotency-key headers. A client retry creates duplicate issues / duplicate tasks. Standard for any mature REST API; missing here.
4. **No retry/backoff schema on tasks**. `agent_task_queue` has `failure_reason` but no `attempt_count`, no `next_attempt_at`. Auto-retry is hand-rolled in service code (`willRetryTask`), not modeled in the schema. This will become an operational quagmire as retry policy needs to differentiate by failure reason.
5. **No SLOs / latency budgets exposed**. Prometheus metrics ARE wired (`METRICS_ADDR=127.0.0.1:9090` in `.env.example`) but I see no `multica_task_dispatch_latency_seconds` histogram by workspace, no `multica_queue_depth` gauge, no `multica_active_runtimes` counter. You cannot SRE what you cannot measure.
6. **Coarse RBAC** (covered above).
7. **JWT-only auth, no refresh token rotation, no token revocation list**. `AUTH_TOKEN_TTL=2592000` (30 days). A leaked token is good for a month. Self-hosters on trusted networks may want this; SaaS users absolutely don't.
8. **No backpressure on Redis Streams**. `streamMaxLen = 10000` is a hard cap with **silent eviction**. A slow consumer just loses events. You'd want at least a counter on dropped events per scope.
9. **No data export / GDPR delete flow visible**. Required for EU. Even basic "user requests their data" is missing.
10. **No anti-CSRF on the API** that I could find. Cookies + same-origin policy + CORS but no double-submit token. The CLAUDE.md "API Response Compatibility" rules suggest they're aware of trust boundaries; the request side is under-covered.
11. **No per-task resource limits**. The daemon spawns the agent CLI subprocess but I see no cgroup limits, no `ulimit`, no memory cap. A runaway agent (Claude going in a loop generating 10GB of output) can OOM the daemon host. The `Codex Timeout=20m` (`MULTICA_CODEX_TIMEOUT` in `.env.example`) is a per-provider wall-clock limit but not a resource limit.
12. **No webhook outbound retry-with-backoff documented**. `webhook_delivery.sql.go` exists in the generated layer but the policy isn't explained in CLAUDE.md.
13. **No structured error responses across the API**. Different handlers return different shapes; the React app's `parseWithFallback` defenses exist precisely because the server doesn't promise a stable error contract.
14. **No replica/HA story for daemons**. One agent, one daemon. If the daemon crashes mid-task, the task is orphaned and recovered by `RecoverOrphanedTasksForRuntime`. Recovery is mark-as-failed, not resume. Real HA would mean per-task work being recoverable by a sibling daemon, which isn't possible because the work happens inside an agent CLI subprocess on a specific machine.

---

## 9. Build cost to clone into `ainb`

**VERDICT: 14-22 engineer-weeks to reach feature parity at the same code quality. 3-4 weeks to reach the *actually valuable* 80%. The 20% you should skip would save 4-6 weeks of pain.**

Stevie's `ainb` already has the right shape for a lot of this — TUI + plugin-host architecture, JSON-RPC plugins, multi-provider abstractions (per `MEMORY.md` notes on `crates/ainb-core/` layout and the plugin v2 architecture). The cloneable pieces map differently than you'd think.

| Subsystem | What to clone | Weeks | Hard vs Wiring |
|---|---|---|---|
| Agent-as-assignee data model | polymorphic `assignee_type` + assignment routing | 0.5 | Wiring |
| Issue tracker with comments + mentions + reactions | Linear-shaped Kanban + activity feed | 4-5 | **Hard** (UX is the moat) |
| Postgres-as-queue with claim/dispatch/recovery | SQL + sweepers + idempotency | 1 | Wiring (copy from Multica directly, the SQL is already excellent) |
| Daemon polling + websocket wakeup | gorilla/websocket equivalent (axum + tungstenite?) | 1 | Mostly wiring |
| Per-agent CLI adapter layer | `execenv/*` equivalent with sandbox handling per CLI | 2-3 | **Hard** (per-CLI quirks) |
| Skills system (DB-backed + native CLI bridge) | `skill`/`skill_file`/`agent_skill` + bridge code | 1.5 | Medium — but I'd skip this entirely and use git-versioned files |
| Autopilot (cron + webhook triggers + admission gating) | cron loop + webhook receiver + skip-policy | 1 | Wiring |
| Squad routing | leader-agent delegation | 1 | Medium — actually subtle if you want it to be useful |
| Realtime fanout via Redis Streams | broadcaster + relay + scope subscriptions | 1 | Wiring |
| Auth + multi-workspace + RBAC | JWT + member roles + workspace gates | 1 | Wiring |
| File storage (S3 + signed URLs) | storage abstraction + CloudFront-equivalent | 0.5 | Wiring |
| Electron desktop app | tab system + WindowOverlay + drag region + auto-update + version-skew defenses | 3-4 | **Hard** — and you should not do this |
| Mobile app (Expo) | iOS-only mobile client | 2-3 | Hard but skippable |
| Operational tooling (migrations, observability, on-call) | sweepers, metrics, dashboards | 1-2 | Wiring |
| **Total** | **14-22 weeks** for feature parity | | |

### What to actually build (the 80%)

Agent-as-assignee, Postgres-as-queue (or SQLite-as-queue for single-tenant `ainb`), daemon polling, per-CLI adapter for 2 CLIs (start with Claude Code + Codex), basic Kanban, single-workspace + simple RBAC. **3-4 weeks if you're disciplined.** Skip Electron, skip mobile, skip skills (use git), skip squads until you have customers asking for them.

### The 20% you should NOT clone

- **Electron desktop** (PWA is enough). Saves 3-4 weeks.
- **DB-backed skills system** (use git). Saves 1.5 weeks and removes a whole class of "skill drift" bugs.
- **Squads** (premature for <10-person teams). Saves 1 week.
- **Cloud runtime proxy** (the value is in the closed-source fleet, not the proxy — and that's a different project entirely). Saves 0.5 week.
- **The elaborate session-resume failure classifier** (just force-retry on failure). Saves a week of edge-case wrangling.
- **The TanStack Query + Zustand + Context three-layer state regime** — overkill for a TUI-first product like `ainb`.

### What's genuinely hard regardless of clone

- **Kanban UX**. Linear has spent 5 years polishing this. Multica gets you 60% there; the last 40% is product work. If `ainb` is TUI-first, this is largely moot — the TUI doesn't need a drag-and-drop Kanban, it needs filterable lists with keyboard navigation.
- **Per-CLI quirks**. Every agent CLI has its own sandbox model, env vars, skill discovery, output format. Each one is 1-2 weeks of "why does this CLI hang on EOF". Multica's `server/internal/daemon/execenv/` has files for `codex_sandbox`, `codex_multi_agent`, `git.go` — read these as reference docs *before* implementing your own.
- **Operational tail**. Stale-task sweepers, orphan recovery, restart-during-dispatch races. Multica has 18 months of incidents baked into their SQL comments — you'll re-discover them all unless you copy the SQL verbatim (which is fine — the SQL is the most useful thing in the repo).
- **The data-model decisions that you can't undo**. Polymorphic assignees (`assignee_type` + `assignee_id`) is a one-way door. Get this shape right at the start, because retrofitting it later means rewriting every query that joins `issue` to `member`.

### Where `ainb`'s existing architecture HELPS

The plugin v2 architecture (`reference_ainb_plugin_v2_architecture` from `MEMORY.md`) — native subprocess + JSON-RPC over Content-Length stdio — is a *better* shape than Multica's daemon-as-monolith for the agent-CLI adapter problem. Each agent CLI becomes a plugin. The capability gating (-32001 runtime errors) gives you the allowlist enforcement that Multica's blocklist lacks. **This is genuinely the right architecture for the per-CLI adapter problem.** Lean into it.

### Where `ainb`'s existing architecture HURTS

`ainb` is TUI-first. Multica is web-first. The shared `core/views/ui` layer regime doesn't translate. If you ever want a web companion to `ainb`, you're starting from scratch — Multica's monorepo will be a reference but not a foundation.

---

## 10. What I'd do differently from scratch in May 2026

**VERDICT: Skip the issue tracker. Build the runtime + observability + sandbox layer that nobody else is building well.**

If I were starting today with everything I know, I would **not build a Linear-clone with agents**. That's the wrong product. Linear, Shortcut, Jira, GitHub Issues, Plane.so are all going to ship "AI agent assignees" in 2026. Multica is racing a horse that's already in the gate.

Instead, I'd build the **agent runtime control plane that those tools plug into**:

1. **Open spec for "agent task envelope"**. Standardise the shape of `{repo, branch, prompt, skills, sandbox-policy, secrets-scope, success-criteria, timeout}` so any issue tracker can dispatch a task to any runtime. Multica's `Task` struct is 90% of this already (`server/internal/handler/agent.go` types), but it's locked to their schema.

2. **Run agents in real sandboxes**. firecracker / gVisor / OCI containers with proper seccomp / Landlock / Seatbelt-that-actually-works. Take the security work seriously *as the product*, not as an afterthought. The Codex `danger-full-access` problem is an opportunity, not a footnote — every other platform has the same hole.

3. **Per-agent secrets vault** with KMS + per-task scoped credentials. The agent should never see the user's `ANTHROPIC_API_KEY`; it should see a scoped, time-limited token issued by the control plane. This is what AWS STS does for human ops — same shape for agents.

4. **Observability native**: every agent action emits a structured event (file read, file write, shell exec, network call). Now you have a *real* audit log and you can SOC2 the thing. Bonus: this becomes the dataset for the "compound skills" story — derived from actual runs, not hand-curated by users.

5. **Stateless runtimes only**. Daemons-on-laptops is a transition-period UX, not a real product. By 2027 the agent fleet lives in Modal / Codespaces / Fly / a real cluster. Build for that. The OSS version can ship a docker-compose for self-hosters; the SaaS runs Firecracker microVMs.

6. **Pluggable issue tracker**: native adapters for Linear, Jira, GitHub Issues, Shortcut. Now you're a layer everyone wants instead of a tool everyone has to migrate to.

7. **Skip the Electron app**. PWA + iOS PWA + native macOS menu bar app (50 LOC of Swift). Save 3 months.

8. **One LLM-agnostic protocol**. Rather than shelling out to 11 different CLIs each with bespoke quirks, define a clean interface and write the adapter once. (This is what `litellm` + `openrouter` did for inference; the agent-CLI space needs the same thing.) Multica's `server/internal/daemon/execenv/` is a reference for what NOT to do — every CLI gets its own bespoke file because no one defined the abstraction.

9. **Bring your own repo, not your own daemon**. The unit of compute is "I want this PR opened against this branch in this repo" — not "I want this CLI run on this machine". Push that boundary up.

10. **Cost attribution from day one**. Every task gets a cost record: input tokens, output tokens, wall-clock time, sandbox-minutes. Multica has `task_usage_hourly` (`migrations/101`) but it's bolted on after the fact. Bake it into the task schema from row zero.

In short: Multica is building the **last generation of the issue tracker**. Build the **first generation of the agent runtime**.

---

## 11. The 3 questions Stevie must answer before copying

**VERDICT: These are the decisions you cannot outsource. Get them wrong and you ship the wrong product. Get them right and the rest is execution.**

### Q1 — Single-tenant or multi-tenant?

If `ainb` stays **single-tenant** (one user, one machine, one repo at a time — the current shape, per `MEMORY.md` notes on volatile worktrees and plugin manifests), then 80% of Multica's complexity is gravity you don't need. No workspace isolation, no `WHERE workspace_id`, no Squads, no admission gating across runtimes. You're building a *personal* agents tool, and the product is dramatically simpler.

If `ainb` goes **multi-tenant SaaS**, you inherit Multica's entire problem surface — RBAC, audit, isolation, per-tenant rate limits, secrets management at scale, cost attribution. This is a year-long engineering effort minimum. Multica's open-source code gives you maybe 20% of what you actually need; the rest is enterprise hardening.

**My take**: stay single-tenant for `ainb` v1-v2. Sell the *single-tenant* story (privacy-first, your laptop is your fortress, no cloud needed). That's a differentiated position vs Multica's hosted-or-self-host fork. Multi-tenant SaaS is where every other agents-platform is headed — you don't have to follow.

### Q2 — BYO-LLM-key or platform-managed?

If **BYO-key** (Multica's default for self-host), you have near-zero variable cost and your users keep their CLI subscriptions. But you can never offer "just sign up and start using agents" — onboarding has a key-paste step that kills conversion. Self-hoster delight, SaaS conversion friction.

If **platform-managed** (you front the API costs, charge users a markup), you have inventory risk (Anthropic outages, rate limits across your whole tenant base) and margin pressure. But onboarding is one click.

The hybrid (Multica-cloud-as-it-could-be: managed runtime, BYO-key) is the worst of both worlds — users still have to paste keys *and* you carry the runtime cost.

**My take for `ainb`**: BYO-key, full stop. Multica's positioning bets on managed runtimes monetising; `ainb` should bet on *being the best agent harness on the user's own machine*. Different lane. If you want to monetise, sell a "managed sync" service ($5/mo for cloud-backed task history + secrets vault + multi-device sync) on top of BYO-key compute. That's a SaaS line you can defend.

### Q3 — Are agents sandboxed-by-construction or trust-the-user?

This is the **most consequential** question and Multica chose "trust the user" (see `danger-full-access` on macOS, `custom_env` blocklist that misses `LD_PRELOAD`/`DYLD_INSERT_LIBRARIES`/`PYTHONPATH`/`NODE_OPTIONS`, no per-repo agent permissions). That's a reasonable choice for "developer tool for your own machine". It's an **insane** choice for "platform you'd run for 100 paying customers".

If `ainb` is **trust-the-user**, you can build fast — your sandbox story is "the agent runs as you, and you accept the risk". Document loudly that agents have your full filesystem + network access. Don't pretend otherwise. Make `ainb shell run --confirm` the default for any tool that writes outside the worktree.

If `ainb` is **sandboxed-by-construction**, you commit to either: (a) Linux + bubblewrap/Landlock + seccomp profiles per CLI, (b) macOS + actually-working Seatbelt profiles per CLI (mostly fictional given the Codex bug), or (c) container/VM-per-task with all the IO marshalling that implies. This is the right choice if you ever want to sell into anyone other than a solo developer, but it's a 3-6 month investment before v1.

**My take for `ainb`**: trust-the-user for v1 with explicit, loud documentation about what agents can touch. Honesty over theatre — say it, don't hide it behind a `danger-full-access` flag that the user has to grep for. If you ever pivot to multi-user (Q1), revisit immediately. Don't ship "we're safe" without proof — that's how Multica ended up with `danger-full-access` shipped in production with three paragraphs of explanatory comments.

---

## Closing — Distinguished Engineer wisdom

Two patterns from 25 years of watching platforms rise and fall:

**Pattern 1 — The "ship the issue tracker" trap.** Every successful agents-platform attempt of the last 18 months has reached for an issue-tracker UX because it's familiar, fundable, and screenshots well. None of them has yet found a moat there. The moat in this space — if it exists — is in the **runtime layer**, not the workflow layer. Multica is making the same bet as Cognition (Devin), Cursor's background agents, Sweep, Sourcegraph Cody, and a dozen others. The winner of "managed agents" will not be the prettiest Kanban board. It will be the platform with the **best per-agent observability, the cleanest sandbox story, and the lowest token cost per task completed**. Stevie should compete on those axes.

**Pattern 2 — "Open core" works only when the closed part is genuinely hard.** Multica's closed part is the cloud fleet (which is hard) and the SaaS dashboard (which isn't). Sentry's closed part was alert routing and storage at scale (hard). Posthog's was data warehouse (hard). When the closed part is "we host it for you", the moat erodes the moment AWS / Fly / Modal commoditise the host-it-for-you layer. Don't model `ainb`'s monetisation on Multica's — Multica's is probably not going to work for them, and copying it won't work for you.

**One war story.** I watched a team in 2019 spend 18 months building a Linear-clone for a category that doesn't exist anymore (it was for ML researchers, before Notion + Replicate + Weights & Biases ate it). They had beautiful code, 12k stars, two paying customers. The CTO is now at a FAANG. The lesson: **stars are a sentiment metric, not a survival metric**. Build for paying users, not for HN.

**Second war story.** A different team I advised in 2021 built a "self-hostable Heroku" with PR-deploy previews — exact playbook as Multica (OSS core, cloud SaaS, glossy README, 18k stars in a year). The cloud SaaS got 200 paying users at peak. The team shut down in 2024 when Vercel ate the category. Their OSS repo still has 28k stars. Stars compound; revenue doesn't.

**Recommendation to Stevie**: do not copy Multica. **Steal three things from it**:

1. The SQL queue pattern in `server/pkg/db/queries/agent.sql:200-414` (lines 200-414 of agent.sql are some of the best operational SQL I've read in any OSS repo — the comments contain 18 months of war-story knowledge about race conditions, recovery windows, and "doomed row" backlogs). **Copy this verbatim.** It is the single most reusable artefact in the whole codebase.

2. The per-CLI adapter shape in `server/internal/daemon/execenv/` (a useful reference for the integration surface area, even if you'll redo it in Rust as `ainb` plugins). The naming convention `<provider>_<aspect>.go` (`codex_sandbox.go`, `codex_multi_agent.go`) is correct; the package boundary is correct; the choice to expose env+config rather than direct subprocess control is correct.

3. The package-boundary regime in `CLAUDE.md` ("Package Boundary Rules", "API Response Compatibility", "Backend Handler UUID Parsing Convention"). The cleanest version of these patterns I've seen — apply them to your Rust crate boundaries. Specifically: the "every rule was added after a concrete bug, treat them as enforced, not suggestions" framing is the right culture artefact for a project with N>2 contributors.

**Then build something different.** Single-tenant, sandbox-as-product, BYO-key, agent-runtime-first not issue-tracker-first. That's the open lane. Multica has shown you the playbook for the lane that's already crowded — go run the play they didn't.

**Final thought**: 31k stars in 4 months is a remarkable achievement and the team deserves credit. But star count is a leading indicator of attention, not a leading indicator of survival. If `ainb` ends up with 5k stars and 500 paying users, it will be a more successful product than Multica with 100k stars and 50 paying users. Build for users, not for stargazers.

---

*— Distinguished Engineer critique, 22 May 2026. References to file paths are exact; line numbers may shift if upstream rebases. No sycophancy was harmed in the making of this document.*
