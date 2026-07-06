# Hangar ⇄ Multica — feature & user-journey comparison

**What this is.** Hangar is a TUI-first managed-agent control plane built inside `ainb`, modelled feature-for-feature on **Multica** (the open-source web app, `github.com/multica-ai/multica`). Multica is a Next.js 16 + Go (Chi/sqlc/pgvector) web product with desktop (Electron) and iOS (Expo) clients. Hangar replaces that whole web stack with a single **daemon** (`ainb-hangar-daemon`, SQLite) plus a **plugin** TUI (`hangar-tui`) that talks to it over a unix-socket JSON-RPC contract.

**Honesty note / method.** The in-repo parity review (`docs/hangar/parity-review.html`, 113 features mapped) was written at the *start* of the `agents-in-a-box-e38` "Hangar parity" epic — its many `gap`/`partial` verdicts describe Hangar *before* that epic. **All 35 e38 child beads are now closed**, so this comparison reconciles the original review against the *current, shipped* code. Where the old review said "gap" and the code now has it, this document says so and cites the migration / RPC / file that built it. The `docs/hangar/architecture.md` summary ("17 RPC methods / 35 features") is itself the *pre-e38* snapshot; the live RPC catalogue is now **39 methods** (`ainb-hangar-proto/src/methods.rs`), **13 TUI screens**, **16 CLI noun-groups**, and **23 migrations** (`ainb-hangar-store/migrations/0001..0023`).

Verified against:
- Multica source: `/tmp/multica-src` (shallow clone — succeeded). Backend entities from `server/migrations/001..119`, routes from `server/internal/handler/`, web surface from `apps/web/app/[workspaceSlug]/(dashboard)/`.
- Hangar source: `ainb-tui/crates/ainb-hangar-{core,proto,store,daemon,sandbox,secrets}`, plugin `plugins/hangar-tui/`, CLI `ainb-tui/crates/ainb-core/src/cli/hangar/mod.rs`.

```
       MULTICA (web)                         HANGAR (TUI)
┌──────────┐  ┌──────────┐          ┌──────────┐   ┌──────────────┐
│ Next.js  │─▶│ Go + WS  │          │ ainb host│──▶│ hangar-tui   │
│ web/ios/ │◀─│ Chi      │          │ + plugin │◀──│ plugin (TUI) │
│ electron │  │ sqlc     │          │ runtime  │   └──────┬───────┘
└──────────┘  └────┬─────┘          └──────────┘   unix-sock JSON-RPC
                   │                                       │
          ┌────────┴────────┐                       ┌──────┴───────┐
          │ Postgres+pgvec  │                       │ hangar-daemon│
          └────────┬────────┘                       │ + SQLite     │
                   │                                 └──────┬───────┘
            ┌──────┴───────┐                          ┌─────┴──────┐
            │ Agent Daemon │ (your machine)           │ provider   │
            └──────────────┘                          │ runner     │
                                                      └────────────┘
```

---

## 1. Feature comparison matrix

Status legend: **parity** (Hangar genuinely matches) · **partial** (subset / approximation) · **gap** (not built, but TUI-expressible) · **hangar-extra** (Hangar has it, Multica doesn't) · **oos** (out-of-scope by design — web/SaaS form-factor with no terminal equivalent).

### Issues & board

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Create issue (title, desc, status, priority, assignee, due dates, labels) | `hangar issue create` + Issues screen `c`; migration `0014_issue_priority_due_labels` | **parity** | e38.9 added priority (0..3 = P3..P0), `due_date`, labels to the issue model — closing the old "title/desc/state/assignee only" gap. |
| Board & list views | Issues list screen (`1`) + Kanban (`K`) | **parity** | Filter chips (All/Members/Agents/Mine) + 4-column card board with drag (mouse) and `Shift+←/→` card-move → task transition. |
| Edit / update issue (state, priority, assignee, project, dates) | `hangar/issue_update` RPC + `issue update` CLI | **parity** (minus project) | e38.8 wired the write RPC (state/assignee/priority/dates). Project field absent (no project model — see Projects). |
| Label issues (create/attach/detach) | `0016_issue_label`; `hangar/issue_label_attach` / `_detach`; `issue label` CLI; chips | **parity** | e38.10. Label table + M:N join + chips. |
| Priority + expedite ordering | `0013_task_priority` + `0014` issue priority; claim drains `ORDER BY priority DESC, created_at, id` | **parity** | e38.4 adopted Multica's exact `priority DESC, created_at` claim ordering. |
| Comment on issues | `0003` comment table + `hangar/comment_add` RPC + `repo/comment.rs` + compose key | **parity** | e38.5 wired the write path the old review flagged as schema-only-no-producer. |
| @-mention an agent in a comment to trigger a task | `ainb-hangar-daemon/src/mentions.rs` (parse) + comment-trigger path | **parity** | e38.7. `@handle` parser + resolution + task spawn — Multica's core collaboration loop. |
| Agent posts durable progress/blocker comments | system-authored comments via daemon (e38.6) + live transcript | **parity** | Both the live 5-colour transcript stream AND durable issue comments. |
| Issue full-text search | `hangar/issues_search` RPC + `repo/search.rs` (ranked title+desc+comment) | **parity** | e38.12. Replaces the old client-side title-only `/` filter. |
| Sub-issues / parent-child issue hierarchy | task retry parent/child only | **gap** | Multica has `issue.parent_issue_id` + `issue_dependency(blocks/blocked_by/related)`. Hangar's parent/child is on *tasks* (retries), not issues. |
| Batch update / delete issues (multi-select) | none | **gap** | No multi-select on the issues screen or a batch CLI verb. |
| React to issues / comments (emoji) | none | **gap** | Marginal; renders fine in a TUI but unbuilt. |
| Subscribe / unsubscribe to an issue | none (per-issue) | **gap** | `workspace/subscribe` is event-stream scope, not per-issue notification opt-in. |
| Resolve / unresolve comments; threaded replies | none | **gap** | Comment model has no `parent_id` / `resolved_at`; flat comments only. |
| Upload / view attachments | none | **gap** | No file-attach to issues/comments. Image preview would be oos, but path-attach + listing is TUI-expressible. |

### Tasks & execution

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Task lifecycle FSM (queued→dispatched→running→done/failed/cancelled, timeouts, retries, orphan reclaim) | Task FSM in `ainb-hangar-core` + finalize services + TTL sweepers + runner timeout | **parity** | Strict FSM, idempotent finalize, parent/child retry capped by `max_attempts`, stale-row sweeps (queued 2h / dispatched 5min / running 2.5h). |
| Assign issue to agent → enqueue task | Agent picker modal (`a`) + `--assign` enqueue | **parity** | The core assign-and-queue flow. |
| Per-agent concurrency limit | `agent.max_concurrent_tasks` (`0006`); claim SQL enforces it | **parity** | e38.3 settled the concurrency model. |
| Per-(issue, agent) concurrency | partial-unique index `idx_one_pending_task_per_issue_agent` (`0012`) + claim `NOT EXISTS` guard | **parity** | e38.3 adopted Multica's `ClaimAgentTask` model: different agents parallelise one issue; same agent's dup fires coalesce. |
| Rerun / cancel task | `r` retry / `x` cancel in task detail; `task retry` / `task cancel` CLI | **parity** | |
| Live task progress streaming | Task detail (`2`) 5-colour live transcript over the event subscription | **parity** | `TaskMessage` events stream over the socket. |
| Multiple coding-agent backends | claude + codex exec paths (`runner.rs`: `run_claude` / `run_codex`, `ProviderSpec`); gemini/copilot scaffolded | **partial** | e38.16 added the codex exec path + provider abstraction. Multica advertises ~12 providers; Hangar ships 2 live (claude, codex) with the SDK shape for more. |
| OS-level agent sandbox | `ainb-hangar-sandbox` crate (`imp_linux.rs` / `imp_macos.rs` + `policy.rs`) | **hangar-extra/parity** | e38.23 added a real OS sandbox before shipping non-claude providers — Multica relies on the provider CLIs' own sandboxing. |
| Retry/resume poisoned-terminal taxonomy | `service/retry.rs` + reason taxonomy (e38.24) | **parity** | Retryable (infra) reasons spawn a child; `agent_error` does not retry. |
| View per-issue run history (list of runs + per-run logs) | single live transcript per task | **partial** | One transcript is shown; a paged per-issue run-history list is not fully surfaced. |
| Session resumption (reuse `session_id` + `work_dir` across same agent/issue) | runner pins first `session_id`; per-task worktree | **partial** | Session pinned + persisted but not fed back into a later run for the same (agent,issue) pair. |
| 1:1 agent chat (chat sessions + messages) | none | **gap** | Multica has `chat_session` / `chat_message` tables + a chat screen. Hangar's transcript is task-scoped only — no standalone chat surface. |

### Agents & runtimes

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Create agent from template | `templates list/show/use`; 10 curated embedded templates | **parity** | `templates use` materialises a live agent + skills. |
| Create / edit / archive agents + config (model, args, env, MCP, thinking) | `hangar/agent_update` + `hangar/agent_archive`; `0015_agent_archive_and_config`; `agent` CLI | **parity** | e38.15 added the CRUD + config knobs (model/args/MCP/thinking/env) the old review flagged as missing. |
| Live agent presence | presence dots in the agent picker | **parity** | |
| Unified runtimes dashboard (list, status, manage) | Daemon health pane (`D`) lists runtimes + status; runtime auto-register on boot | **partial** | e38.20 added boot-time runtime auto-register; full manage verbs (set-visibility, delete) still thin. |
| Inspect runtime models / trigger CLI update | none | **gap** | No query of a runtime's available models, nor a runtime-CLI-update trigger. |
| Cloud runtime node control (create/start/stop/reboot) | `runtime_mode` enum only | **oos** | Multica's cloud control proxies a closed SaaS fleet (and is itself waitlisted). Deliberate scope-out (`build-plan.md`). |
| Set agent avatar (uploaded image) | presence dots | **oos** | Images have no terminal rendering; presence dots are the TUI identity cue. |

### Skills

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Assign / unassign skills to agents | Skill manager (`4`) `i`/`d`; `agent_skill` M:N join | **parity** | |
| Import / sync skills | `skills sync` importer (idempotent upsert from the toolkit tree) | **partial** | Local-tree import only; importing from arbitrary URLs (clawhub / skills.sh) is "future". |
| Dispatch-time skill materialisation | daemon copies skills to provider-native paths (`.claude/skills/`, `.codex/skills/`…) outside the git root | **parity** | |
| Skill file CRUD (list/upsert/delete) | `skill_file` model + file-tree view | **partial** | List + view present; in-TUI per-file edit/delete not surfaced. |
| Search / filter skills | filter chips (All/Used/Unused/Mine) | **parity** | Matches Multica's own chip-only skills UI (no free-text in Multica either). |

### Autopilots / cron / webhooks

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Create / edit / delete autopilots | Autopilots screen (`5`) `a`/`e`/`d`; `autopilot` CLI | **parity** | |
| Cron schedule | `0009_autopilot_next_tick` + scheduler loop; 6-field cron (5-field normalised) | **parity** | |
| Manual fire-now | `hangar/autopilot_fire_now`; screen `r` | **parity** | |
| Execution modes (`create_issue` vs `run_only`) | `0019_autopilot_execution_concurrency` (`execution_mode` enum) | **parity** | e38.19 — matches Multica's exact `create_issue`/`run_only` CHECK. |
| Concurrency policies (`skip` / `queue` / `replace`) | `0019` (`concurrency_policy` enum) | **parity** | e38.19 — matches Multica's exact `skip`/`queue`/`replace` CHECK. Old review only had skip. |
| Webhook-triggered autopilots (HTTP ingress + signing secret) | `0018_autopilot_webhook` + `webhook_ingress.rs` (hand-rolled HTTP/1.1 over TcpListener) + HMAC-SHA256 verify | **parity** | e38.18. `POST /hangar/webhook/<id>` with `X-Hangar-Signature` constant-time-verified; delivery audit log. |
| View autopilot runs + deliveries | run list on the screen; `autopilot_webhook_delivery` audit log | **parity** | |
| Autopilot preset templates (one-click) | none | **gap** | No built-in autopilot presets (news digest / PR review / bug triage); create is freeform. |

### Squads & members

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Squad entity + CRUD | `0017_squad`; `hangar/squad_create` / `squads_list`; `squad` CLI | **parity** | e38.17 built the whole squad surface the old review marked entirely absent. |
| Squad membership | `hangar/squad_member_add` / `_remove` | **parity** | |
| Assign issue to squad → leader routing | `hangar/squad_assign` + `service/squad_assign.rs` (resolves leader → leader's runtime → enqueues to leader) | **parity** | e38.17. Leader routing actually takes effect (not just a resolver). |
| Member & role management (list / set-role / remove) | `hangar/members_list` / `member_set_role` / `member_remove`; `member` CLI | **parity** | e38.11. Closed the old "schema-only role, no surface" gap. |
| Invite members / accept-decline invitations | none | **gap** | No invitation pipeline (single-operator local daemon; invite/accept is multi-tenant web CRUD). |
| Leave workspace | none | **gap** | No self-removal verb. |

### Search, palette, notifications, usage

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Global cross-entity search / command palette (Cmd+K) | Command palette screen (`command_palette.rs`) + `hangar/search` RPC | **parity** | e38.13. Cross-entity ranked search; the old review had only per-screen chips. |
| Notification inbox (unread aggregation) | Inbox screen (`I`) + `0021_inbox_entry` + `hangar/inbox_list` / `inbox_mark_read` | **parity** | e38.14. |
| Notification preferences | none | **gap** | No per-event preference toggles in settings. |
| Usage dashboard (token/cost + per-agent rollup) | Usage screen (`U`) + `0022_task_usage` + `hangar/usage_rollup` | **parity** | e38.35. Persists `input/output_tokens` + `total_cost_usd`, per-agent + per-workspace rollup. Old review only had a throughput sparkline. |
| Daemon-health sparkline (throughput) | Daemon health pane (`D`) dual-dim green/red sparkline | **hangar-extra** | A TUI-native operational view Multica's web usage page doesn't have. |

### Settings, workspaces, onboarding

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Create / switch / list workspaces | Settings (`,`) `n`/`s`/`d`/`r`; `workspace/list` RPC | **parity** | |
| Multi-workspace data isolation | every by-id query workspace-scoped (`resolve_workspace_id` IDOR guard); slug-or-id resolution | **parity** | e38.26 added a data-isolation tripwire (not just an active marker). |
| Per-workspace context prompt + repo whitelist + issue prefix | `0020_workspace_config`; `workspace config` CLI | **parity** (config persisted) | e38.21. `context_prompt` injected as `CLAUDE.md` at dispatch; `issue_prefix` prepended; `repo_whitelist` persisted+validated (the checkout flow that consumes it lands later). |
| First-run onboarding (source/role/use-case questionnaire) | `firstrun.rs` wizard (e38.33) + danger-full-access first-run warning | **partial** | Has a first-run flow + questionnaire shape; runtime-provisioning onboarding is thinner than Multica's. |
| Edit user profile / prefs (name / language / timezone) | none | **gap** | `user` table is id/email only; no i18n / timezone editing. |
| Appearance / theme (light/dark/system) | host `ainb` Settings → Appearance (Dark/Light/System) | **parity** (host-level) | Lives in the host shell, not the hangar plugin's own pane. |
| Multi-language UI / localized CLI | none | **gap** | English only; no i18n string tables. |

### Auth & secrets

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Authenticated RPC server | unix-socket token verify + peer-cred check (`0011_daemon_socket_token`) | **parity** | e38.1 closed the old "daemon authenticates no requests" hole. |
| PAT / daemon tokens (hash-only, shown once) | `0005` pat + daemon_token (sha256 only) + constant-time verify; `auth token` CLI | **parity** | |
| OS keychain secret store | `ainb-hangar-secrets` (macOS Security.framework / Linux), zeroized bytes, `secrets:read` capability gate | **parity** | |
| Env allowlist (block `LD_PRELOAD` etc.) | `env_policy` allowlist enforced in the runner | **hangar-extra** | A TUI-native hardening Multica leaves to the provider. |
| Email verification-code login | PAT / daemon tokens | **oos** | Email magic-code presupposes a multi-tenant mail server. Token auth is the local equivalent. |
| Google OAuth login | none | **oos** | OAuth callback is browser-bound. |
| Logout / token revoke | `auth token revoke` (hard-deletes) | **parity** | Stateless PAT bearer; revoke is the full credential-invalidation. |

### gh / GitHub integration

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Link PR to issue + CI / merge-conflict status + auto-move-to-Done | `ainb-hangar-proto/src/pr_status.rs` (`PrStatus`: ci rollup, `Mergeable::{Mergeable,Conflicting,Unknown}`) + `hangar/pr_status_refresh` RPC + PR badge | **parity** | e38.34. The old review had URL-capture only; CI + conflict status + badge now present. |
| PR-URL capture from transcript | `pr_url.rs` scrapes `gh pr create` URL into `result.pr_url`; badge + `o` open-in-browser | **parity** | |
| Connect GitHub (App install) | none | **oos** | GitHub App install is an OAuth web consent flow + hosted `github_installation` mirror. No terminal equivalent. |
| Lark / Feishu chat integration | none | **oos** | Hosted third-party SaaS webhook/OAuth flow. |

### CLI & daemon

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Daemon lifecycle (start/stop/status/restart/setup) | `hangar daemon` {Status, Run, Start, Stop, Restart, Setup} | **parity** | e38.20 + e38.36. Old review had Status only; now full lifecycle incl. background `start` (records exact PID) + one-command `setup`. |
| Offline empty-state + start-daemon action | Hangar empty-state with `[s]` start-daemon (e38.36) | **parity** | |
| Entity CLIs (issue/agent/skill/squad/project/label/autopilot/runtime/member) | 16 noun-groups: issue, task, beads, daemon, auth, config, skills, templates, agent, member, squad, autopilot, workspace, logs (+ issue label sub-CLI) | **parity** (minus project) | Only `project` has no CLI (no project model). |
| One-command setup (configure + auth + start) | `daemon setup` | **parity** | |
| Self-update CLI | `ainb plugin update` (host-level) | **partial** | No `hangar update` verb; self-update deferred to the OS package manager / host. |
| Workspace GC (reclaim disk) | TTL task sweepers + scheduled `OrphanScan`/`Full` GC loop (e38.22) | **parity** | e38.22 wired the previously-unscheduled disk GC. |
| CLI repo checkout / repo whitelist | `repo_whitelist` persisted (`0020`); checkout flow not yet wired | **partial** | Whitelist stored + validated; the checkout flow that consumes it is deferred. |
| Daemon profiles (multiple isolated daemons) | none | **gap** | Single-instance-per-home (O_EXCL pidfile); `$AINB_HANGAR_HOME` is a test seam, not a profile. |

### Infra & platform

| Multica feature | Hangar equivalent | Status | Note |
|---|---|---|---|
| Realtime WebSocket feed | `workspace/subscribe` event stream; daemon **pushes** `hangar/event` (TaskStarted/Finished, AutopilotRunChanged…) | **parity** | e38.2 wired the event-push emission the old review found decode-only. |
| Operational telemetry | tracing JSONL sink + optional OTLP exporter (`otlp` feature) + 8 instrumented spans | **partial** | Strong ops telemetry; no PostHog product-analytics funnel / Prometheus counters. |
| Self-host via Docker Compose / Kubernetes | single local daemon binary | **oos** | Hangar is one local binary; no multi-service web stack to orchestrate. |
| Desktop app (Electron) | none | **oos** | The TUI *is* the client. |
| Mobile app (iOS / Expo) | none | **oos** | No terminal form factor. |
| Cloud billing / Stripe top-ups | none | **oos** | Hosted PCI payment flow. |
| Projects (status, priority, lead, grouped issues) | none | **gap** | Multica has a full `project` table (status / lead_type / lead_id) + project screen + project CLI. Hangar has no project model; the Kanban "Project" sidebar label aliases `workspace_id`. |
| Contact sales / feedback submit | none | **oos** / **gap** | Contact-sales is marketing-web (oos); in-product feedback submit is a marginal gap. |

---

## 2. Gaps — with specific reasons

Each gap is categorised by *why* it is a gap.

**Deliberate scope-cut (web/SaaS form-factor — no terminal equivalent, intentional `oos`):**
- **Desktop (Electron) + mobile (iOS/Expo) clients.** Hangar's entire premise is the terminal; a GUI shell defeats the point. Multica ships both (`apps/desktop`, `apps/mobile`).
- **Cloud runtime node control + cloud billing (Stripe).** Multica's cloud panel proxies a closed SaaS fleet, and billing is a hosted PCI checkout. Hangar runs only local runtimes; `build-plan.md` records "Cloud runtime: skip entirely."
- **Email-code login + Google OAuth.** Both presuppose a multi-tenant server (mail infra / OAuth callback). The local daemon proves identity with PAT/daemon tokens instead — the *capability* (authenticate to the daemon) is met; the web *mechanism* is intentionally absent.
- **GitHub App install + Lark/Feishu integration.** OAuth/web consent handshakes that mirror state into hosted tables (`github_installation`, `lark_*`). Hangar does the TUI-native slice — PR-URL capture + CI/conflict badge via `gh` — without the App-install web flow.
- **Self-host Docker Compose / Kubernetes.** There is nothing to orchestrate: Hangar is one binary + one SQLite file.
- **Agent / workspace avatars (uploaded images).** No terminal rendering surface; presence dots are the identity cue.

**Genuinely missing — TUI-expressible, not yet built (where it would live):**
- **1:1 agent chat (chat sessions + messages).** Confirmed absent in current Hangar (no `chat_session`/`chat_message` migration, no `Screen::Chat`). Multica has persistent chat (`033_chat`). Would need a new migration + a `Chat` screen + a chat-message event topic. The transcript stream is task-scoped, not a standalone conversation.
- **Projects (status / priority / lead / grouped issues).** No `project` table anywhere in `0001..0023`. Issues carry no `project_id`; the Kanban "Project" label aliases `workspace_id`. Would need a project migration + RPCs + a project screen + a `project` CLI noun-group. The Kanban already proves the list/board form factor, so this is "not built," not "can't build."
- **Sub-issues / issue dependencies.** Multica has `issue.parent_issue_id` + `issue_dependency(blocks/blocked_by/related)`. Hangar's parent/child lives on *tasks* (for retries), not issues. Would extend the issue model + add a dependency join.
- **Autopilot preset templates.** Autopilot create is freeform (name + cron + agent). No embedded one-click presets (PR review / bug triage / news digest). Would mirror the existing 10 curated *agent* templates but for autopilots.
- **Member invitations + accept/decline + leave-workspace.** Member/role *management* now exists (e38.11), but the invitation pipeline does not — low urgency for a single-operator local daemon, but it is CRUD that a TUI could express.
- **Batch issue update/delete, emoji reactions, per-issue subscribe, threaded/resolvable comments, attachments, notification preferences, user profile/timezone/i18n, daemon profiles, runtime model introspection, self-update verb.** Each is a small TUI-expressible surface that simply wasn't in scope for the parity epic; several are marginal (reactions, i18n).

**Partial — present but a subset of Multica's capability:**
- **Multi-provider execution.** Two live exec paths (claude, codex) + a `ProviderSpec` abstraction; Multica advertises ~12. The architecture is provider-agnostic — each new provider is one more `ProviderSpec`, not a fork — but only 2 ship today.
- **Skills import.** Local toolkit-tree import only; URL import (clawhub / skills.sh) is flagged "future" in `skills_sync.rs`.
- **Repo checkout.** The repo whitelist is persisted + validated (`0020`), but the checkout flow that *consumes* it is deferred.
- **Per-issue run history.** One live transcript per task; a paged per-issue run-history list is not fully surfaced.
- **Session resumption across runs.** `session_id` is pinned + persisted but not fed back into a later run for the same (agent, issue) pair.
- **Telemetry.** Tracing JSONL + OTLP spans (operational) but no PostHog product-analytics funnel / Prometheus counters.

**Architecturally different (not a gap — TUI/daemon constraint, deliberately reshaped):**
- **WebSocket feed → unix-socket JSON-RPC event push.** Same realtime fabric, different transport. Snapshots reconcile authoritatively, so a dropped event self-heals.
- **Postgres + pgvector → SQLite (Postgres-compatible schema).** A single local file; the schema is kept Postgres-shaped for a future server backend.
- **Web route guards → capability gating + workspace-scope IDOR guard.** No routes to redirect; instead the daemon gates every privileged plugin call on a declared capability and scopes every by-id query to the resolved workspace.
- **Appearance theme** lives in the host `ainb` shell, not the hangar plugin's own settings pane.

---

## 3. User-journey coverage

Coverage legend: **full** · **partial** · **none**.

| Multica user journey | Hangar coverage | How in Hangar | Gap if any |
|---|---|---|---|
| Create an issue (title/desc/priority/due/labels) | **full** | `hangar issue create` or Issues screen (`1`) `c`; priority/due/labels via `0014`/`0016` | — |
| Assign an issue to an agent → task enqueues | **full** | Agent picker (`a`) or `--assign`; enqueues an `agent_task_queue` row claimed by the daemon | — |
| Watch the task execute live | **full** | Task detail (`2`) streams the 5-colour transcript over the event subscription; task-started banner | — |
| Agent reports progress / blockers | **full** | Live transcript + durable system-authored comments (e38.6) | — |
| @-mention an agent in a comment to kick off work | **full** | `mentions.rs` parses `@handle`, resolves, spawns a task (e38.7) | — |
| Move a card across the board → drive a state change | **full** | Kanban (`K`) `Shift+←/→` or mouse drag → `hangar/task_transition` → re-render | — |
| Review the resulting PR (link + CI + conflict status) | **full** | PR-URL capture + PR badge with CI rollup + `Mergeable/Conflicting` + `o` open-in-browser (e38.34) | Auto-move-to-Done on merge wired via PR status refresh; GitHub *App* install remains oos |
| Schedule recurring work with an autopilot (cron) | **full** | Autopilots (`5`) `a`; cron validated, scheduler fires, `create_issue`/`run_only` mode + skip/queue/replace policy (e38.19) | — |
| Trigger an autopilot from an external webhook | **full** | `POST /hangar/webhook/<id>` with HMAC-SHA256 signature → fires (e38.18) | — |
| Search across issues / agents / skills (Cmd+K) | **full** | Command palette + `hangar/search` (cross-entity ranked) + `issues_search` (e38.12/13) | — |
| Triage from a notification inbox | **full** | Inbox screen (`I`) + unread aggregation + `inbox_mark_read` (e38.14) | Notification *preferences* absent |
| Assign work to a squad → leader routes it | **full** | `squad_assign` resolves leader → leader's runtime → enqueues (e38.17) | — |
| Manage members & roles | **full** | `members_list` / `member_set_role` / `member_remove` + `member` CLI (e38.11) | Invitations / accept-decline absent |
| Curate & assign reusable skills | **full** | Skill manager (`4`) sync + `i`/`d` attach/detach; materialised at dispatch | URL import is local-tree-only |
| Create / edit / archive agents + tune config | **full** | `agent_update` / `agent_archive` + config knobs (e38.15); or `templates use` | — |
| See token/cost usage + per-agent rollup | **full** | Usage screen (`U`) + `usage_rollup` (e38.35) | — |
| Set up the tool from scratch (one command) | **full** | `hangar daemon setup` (store + token + background start); offline empty-state `[s]` action (e38.20/36) | — |
| Onboard with a guided questionnaire | **partial** | `firstrun.rs` wizard + danger-full-access warning (e38.28/33) | Runtime-provisioning onboarding thinner than Multica's |
| Organise issues under a project | **none** | — | No project model; the Kanban "Project" label aliases `workspace_id` |
| Hold a 1:1 chat with an agent (outside a task) | **none** | — | No chat sessions/messages or chat screen; transcript is task-scoped only |
| Nest sub-issues / declare issue dependencies | **none** | — | Parent/child is on tasks (retries), not issues; no `issue_dependency` |
| Multi-select batch-update issues | **none** | — | No multi-select / batch CLI |
| Invite a teammate to the workspace | **none** | — | No invitation pipeline (single-operator local daemon) |
| Use Hangar from a phone / desktop GUI | **none** | — | oos — the TUI is the only client by design |

---

## Bottom line

Hangar is a **faithful TUI replica** of Multica's core managed-agents loop, and after the `agents-in-a-box-e38` parity epic it reaches genuine parity on the headline journeys: file an issue → assign/@-mention an agent → watch it execute → review the PR (with CI/conflict status) → schedule it on cron or a webhook → search, triage from an inbox, route through a squad, and read usage. The deltas that remain split cleanly into (a) **honest scope-cuts** that have no terminal form factor (web/desktop/mobile clients, OAuth/email login, GitHub-App install, Lark, cloud nodes, Stripe billing) and (b) **genuinely-unbuilt-but-buildable** surfaces — chiefly **chat**, **projects**, **sub-issues/dependencies**, **member invitations**, and **autopilot presets** — plus a handful of **partials** (≤2 of ~12 providers live, local-only skill import, deferred repo-checkout). Nothing in the remaining gap list is blocked by the TUI architecture; it is simply work not yet scoped.
