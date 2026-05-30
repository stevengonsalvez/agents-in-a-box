# Hangar TUI — Build Plan (interview in progress)

**Status:** interview in progress · iteration 0
**Updated:** 2026-05-28

> Decisions are recorded here as they are locked. This file feeds `/plan-tdd` and `/make-a-goal` once green-lit.

## Locked Constraints (from Stevie)

- **No web UI.** TUI is the full control plane.
- **Loose coupling** between control plane and execution.
- **Feature replica of Multica** — copy feature set verbatim where sensible.
- **Goal-mode execution** — every decision must be locked before handoff.
- **TDD + e2e mandatory** as explicit deliverables.

## Decisions (round 1 — foundation) ✓

- [x] **Packaging: Hybrid** — core/daemon/store as in-tree crates, TUI ships as ainb plugin via plugin host v2. Dogfoods the SDK.
- [x] **Daemon model: separate binary** — `ainb-hangar-daemon` long-lived in tmux, survives TUI quit, unix socket transport.
- [x] **Persistence: SQLite with sqlx + Postgres-compatible schema** — `~/.ainb/hangar.db` WAL mode, Postgres deferred to multi-machine release.
- [x] **Multi-tenancy: workspace primitive from day 1** — `workspace_id` on every table, default workspace = `local`. No refactor cost later.

### Implied structure

```
ainb workspace
├── crates/
│   ├── ainb-hangar-core    (domain, FSM, services)
│   ├── ainb-hangar-daemon  (binary — long-lived process)
│   ├── ainb-hangar-store   (sqlx + migrations)
│   └── ainb-hangar-proto   (shared wire types)
└── plugins/
    └── hangar-tui/
        ├── manifest.toml
        └── ainb-plugin-hangar  (subprocess plugin)
```

## Decisions (round 2 — runtime + transport) ✓

- [x] **Agent runtime: reuse ainb's worktree pattern** — `git worktree add` per task into `~/.ainb/hangar/workspaces/{ws}/{shortID}/workdir/`, with `output/`, `logs/`, `.gc_meta.json` siblings.
- [x] **Transport: unix domain socket + JSON-RPC Content-Length framing** — `$XDG_RUNTIME_DIR/ainb-hangar.sock` (or `~/.ainb/hangar.sock` on macOS). Reuses plugin host framing crate. Unix-only, matches ainb v1.0 platform decision.
- [x] **Task lifecycle: Multica FSM verbatim** — `queued → dispatched → running → done|failed|cancelled` + TTLs (2h / 5min / 2.5h) + idempotent finalize + retry via new row with `parent_task_id`.
- [x] **Plugin host v2 new caps (all 4):**
  - `host/event_stream_subscribe` — streaming reads
  - `host/spawn_managed_subprocess` — daemon spawn on init
  - `host/unix_socket_dial` — whitelisted hangar socket only
  - `host/secret_store_get` — OS keychain integration (mac Keychain / linux Secret Service)

> Plugin host v2 will need a CTS extension for each new cap (per `reference_ainb_plugin_v2_architecture`).

## Decisions (round 3 — feature scope) ✓

- [x] **Skills: hybrid + importer** — Hangar has its own `skill`/`skill_file`/`agent_skill` tables. Embedded curated `agent_template` JSONs in the binary (Multica pattern). `ainb hangar skills sync` importer pulls from `toolkit/packages/skills/` on demand.
- [x] **Beads: two-way sync adapter** — `hangar_id ⇄ bd_id` mapping table, last-writer-wins, reconcile command. Both UX surfaces stay alive.
- [x] **GitHub: gh CLI fallback only** — agent shells out to `gh pr create`, PR URL captured into `task.result`. No GitHub App, no webhook ingress at v1.
- [x] **Autopilots: cron only.** **Cloud runtime: skip entirely** — no fleet client, no proxy stub. Local execution only at v1.

## Decisions (round 4 — UX + Test + Ops) ✓

- [x] **TUI surfaces v1: all 4** — Core 5 (Issue list / Task detail / Agent picker / Skill manager / Settings) + Autopilot manager + Kanban board + Daemon health pane. Accepted +4w to roadmap (16w → 20w).
- [x] **Test harness: 3-layer pyramid matching existing ainb-tui pattern**
  - **Unit:** `rstest` parametrized + `insta` snapshots (`crate/tests/*.rs`)
  - **Integration:** ephemeral SQLite via `tempfile::tempdir` + `sqlx migrate fresh` + `ENV_LOCK` mutex + isolated `$HOME` per test
  - **E2E:** `tripwire_*.rs` that spawn `ainb hangar tui` / `ainb-hangar-daemon` inside tmux, seed config + DB, `tmux send-keys`, poll `tmux capture-pane` for substring assertions
  - **Determinism:** inject `HangarClock` trait + ulid generator + isolated `$HOME` tempdir per test
- [x] **Observability: tracing + OTEL exporter + JSONL fallback** — `~/.ainb/hangar/logs/daemon.jsonl` by default; `OTEL_EXPORTER_OTLP_ENDPOINT` switches to OTLP. No HTTP scrape; no PostHog. TUI "logs" screen tails the JSONL.
- [x] **Security: allowlist env + OS keychain + trust-the-user-with-warnings** — env allowlist (12 vars default, user-configurable). LLM keys via `host/secret_store_get` cap → mac Keychain / linux Secret Service. No container sandbox at v1; explicit `danger-full-access` warning at first run + first provider invocation per session.

## Decisions (round 5 — sequencing) ✓

- [x] **Naming: `ainb hangar <verb>` namespace** — `init`, `daemon start|stop|status`, `tui`, `issue create|list`, `task list|cancel|retry`, `workspace`, `skill sync|list`, `autopilot list|create|run`, `config`. No collisions with `ainb skill`, `ainb plugin`, `ainb tmux`.
- [x] **Phases: 10-phase plan, ~2w each, total 20w** — every phase = one swarm wave.

| Phase | Weeks | Title | Deliverable |
|-------|-------|-------|-------------|
| ✅ P0 | W1-2 | Schema + crates skeleton | 4 crates compile; SQLite migrations run; workspace + member + issue + task tables exist — **DONE** (7 beads, 41 tests) |
| ✅ P1 | W3-4 | Daemon + task FSM | `ainb-hangar-daemon` claims/starts/completes a task; TTL sweepers + idempotent finalize + retry rows — **DONE** (7 beads, 115 tests, e2e tmux green) |
| ✅ P2 | W5-6 | Beads sync adapter | Two-way sync: create issue in hangar → mirrors to beads; `bd close` → marks task done — **DONE** (6 beads, 61 tests, live-bd round-trip green). ⚠ CLI namespace wiring tracked as 174.11 |
| ✅ P3 | W7-8 | Plugin host caps + hangar-tui plugin scaffold | 4 new caps land in plugin host v2; `hangar-tui` plugin connects to daemon over unix socket via dial cap — **DONE** (8 beads, CTS A15-A18 green, plugin scaffold + connect tripwire) |
| ✅ P4 | W9-10 | Core 5 TUI screens | Issue list / Task detail / Agent picker / Skill manager / Settings — all wired to daemon streams — **DONE** (10 beads incl. render integration; daemon unix-socket JSON-RPC server + 4 snapshot RPCs; host `g` nav to HANGAR screen; 6 per-screen tmux tripwires real-green; slug→id workspace resolution fix so CLI/onboarding data renders; asciinema proof `docs/hangar/proofs/p4-tui.cast`) |
| P5 | W11-12 | Auth + workspace + secret store | OS keychain integration for LLM keys; allowlist env enforcement; workspace switching |
| P6 | W13-14 | Skills + curated templates | Embedded `agent_template` JSONs; `ainb hangar skills sync` importer from `toolkit/packages/skills/` |
| P7 | W15-16 | Autopilots + cron scheduler | Schedule-only autopilots; daemon scheduler thread; autopilot manager screen |
| P8 | W17-18 | Kanban + Daemon health + observability polish | Kanban board screen; daemon health pane; tracing + OTEL exporter wired |
| P9 | W19-20 | gh integration + e2e pass + release | `gh pr create` integration; full tripwire pass; brew formula bump; v1.0 release |

- [x] **Acceptance criteria framework (every phase):**
  - User-visible proof (asciinema clip or screenshot in PR body)
  - Tripwire test(s) under `crates/ainb-hangar-*/tests/tripwire_*.rs`
  - `cargo test --workspace` green
  - `cargo clippy --workspace --all-targets -- -D warnings` clean
  - This file's checkbox flipped in the merge commit
  - PR opened + merged (no force-push, no squash per `feedback_merge_commit`)
- [x] **Green-light: GO.** Next: `/plan-tdd` per phase → `/make-a-goal` for the autonomous run.

## Architecture summary (consolidated)

### Process model

```
┌──────────────────────────────────────────────────────────────────┐
│                       USER MACHINE                               │
│                                                                  │
│  user terminal                                                   │
│  └─ ainb tui                                                     │
│      └─ hangar-tui plugin (subprocess via plugin host v2)        │
│           │ JSON-RPC over unix socket                            │
│           │ host/event_stream_subscribe                          │
│           ▼                                                      │
│   ~/.ainb/hangar.sock                                            │
│           │                                                      │
│   tmux session: hangar-daemon-{ts}                               │
│   └─ ainb-hangar-daemon (long-lived)                             │
│        ├─ sqlx pool → ~/.ainb/hangar.db (SQLite WAL)             │
│        ├─ scheduler thread (cron)                                │
│        ├─ TTL sweeper threads                                    │
│        ├─ beads-sync adapter                                     │
│        └─ per-task supervisor                                    │
│            └─ git worktree add                                   │
│                ~/.ainb/hangar/workspaces/{ws}/{tid}/workdir/     │
│                └─ exec() agent CLI (claude/codex/copilot/…)      │
└──────────────────────────────────────────────────────────────────┘
```

### Crate layout

```
ainb workspace (root)
├── crates/
│   ├── ainb-hangar-core    (domain types, FSM, services, no IO)
│   ├── ainb-hangar-store   (sqlx, migrations, repository pattern)
│   ├── ainb-hangar-proto   (JSON-RPC types shared daemon ↔ plugin)
│   └── ainb-hangar-daemon  (binary, tokio, supervises agents)
├── plugins/
│   └── hangar-tui/
│       ├── manifest.toml
│       └── ainb-plugin-hangar  (subprocess, ratatui)
└── docs/hangar/  (research + build plan + per-phase plans)
```

### Data model (initial tables — copied from Multica's schema themes, SQLite-flavored)

- `workspace (id, slug, name, created_at)`
- `member (workspace_id, user_id, role)` — single user at v1 but schema is right
- `user (id, email, created_at)`
- `agent (id, workspace_id, name, runtime_id, instructions, visibility, owner_id)`
- `agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode)`
- `issue (id, workspace_id, title, description, assignee_type, assignee_id, creator_type, creator_id, ...)`
- `comment (id, issue_id, author_type, author_id, body, ...)`
- `agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id?, status, result JSON, session_id, work_dir, attempt, max_attempts, parent_task_id, failure_reason, ...)`
- `skill (id, workspace_id, name, description, content)`
- `skill_file (skill_id, path, content)`
- `agent_skill (agent_id, skill_id)`
- `autopilot (id, workspace_id, name, cron_expr, agent_id, instructions)`
- `autopilot_run (id, autopilot_id, task_id, started_at, completed_at, status)`
- `beads_mapping (hangar_id, bd_id, hangar_kind, bd_kind, last_synced)` — sync adapter
- `pat (id, user_id, sha256_token, scope, created_at, last_used)`
- `daemon_token (id, sha256_token, runtime_id, created_at)`

### Transport

- TUI plugin ↔ daemon: unix domain socket `$XDG_RUNTIME_DIR/ainb-hangar.sock` (mac fallback: `~/.ainb/hangar.sock`)
- JSON-RPC 2.0 with Content-Length framing (matches plugin host v2)
- Streaming via `host/event_stream_subscribe` cap (long-lived subscriptions for transcript / task progress)
- Daemon ↔ agent CLI: stdin/stdout exec subprocess; loopback HTTP for `repo checkout` callback

### Reuse map (avoid rebuilding what ainb already has)

| Need | Reuse | Notes |
|------|-------|-------|
| Plugin subprocess host | `ainb-plugin-host` v2 | extend with 4 new caps |
| JSON-RPC framing | existing plugin-host framing crate | shared with proto crate |
| Git worktree-per-task | existing `crates/ainb-core` worktree helpers | wrap in `ainb-hangar-daemon::worktree` |
| Multi-provider CLI abstraction | existing | bind providers to `agent_runtime.provider` |
| Skills library | `toolkit/packages/skills/` | importer feeds Hangar's skill table |
| Beads issue tracker | unchanged | sync adapter is the bridge |
| Ratatui base + width-aware panels | existing TUI patterns | plugin embeds same patterns |
| OS keychain | new — via `host/secret_store_get` cap | mac Keychain / linux Secret Service |
| Tracing | existing tracing setup pattern | OTEL exporter feature-flagged |

## Open Risks (surfaced during interview)

| Risk | Severity | Mitigation |
|------|----------|------------|
| SQLite write contention on heavy concurrent claim cycles | Med | WAL mode + queue-claim serialised on single writer task; benchmark in P1 |
| Plugin host v2 cap additions create CTS churn | Med | Land cap PR before P3 starts; bump CTS golden in same PR |
| Tmux required on test runners | Low | Already true for ainb-tui; CI matrix already has tmux |
| OS keychain shims (mac vs linux divergence) | Med | Defer to P5; ship dotenv fallback if keychain unavailable on Linux box |
| Goal-mode budget for 20w of work | High | Split into 2-3 sub-goals (e.g. P0-P4 = "hangar walks", P5-P9 = "hangar runs"); revisit goal scope between sub-goals |
| vt100/tmux capture brittleness on screen rewrites | Low | Substring matching not full snapshots; deterministic clock + ulid injection |
| `agent_task_queue` overload (per Multica DE finding) | Low | Hangar has one task kind (issue-driven) at v1 — chat/autopilot tasks added as separate tables in P7 |

## Beads structure ✓

The plan ships as beads, not just markdown. One epic, 10 phase beads, 5–9 sub-beads per phase (~80–100 beads total).

```
Epic: hangar-v1
  ├─ hangar:P0  Schema + crates skeleton
  │    ├─ hangar:P0.1 workspace + migrations table
  │    ├─ hangar:P0.2 sqlx pool wiring
  │    ├─ hangar:P0.3 ainb-hangar-store crate scaffold
  │    ├─ hangar:P0.4 ainb-hangar-core crate scaffold
  │    ├─ hangar:P0.5 ainb-hangar-proto crate scaffold
  │    ├─ hangar:P0.6 ainb-hangar-daemon binary stub
  │    └─ hangar:P0.7 tripwire skeleton: daemon binary boots
  ├─ hangar:P1  Daemon + task FSM         (~7 sub-beads)
  ├─ hangar:P2  Beads sync adapter        (~6 sub-beads)
  ├─ hangar:P3  Plugin host caps + plugin scaffold  (~8 sub-beads)
  ├─ hangar:P4  Core 5 TUI screens        (~9 sub-beads)
  ├─ hangar:P5  Auth + workspace + secret store     (~6 sub-beads)
  ├─ hangar:P6  Skills + curated templates (~5 sub-beads)
  ├─ hangar:P7  Autopilots + cron scheduler         (~6 sub-beads)
  ├─ hangar:P8  Kanban + Daemon health + observability  (~7 sub-beads)
  └─ hangar:P9  gh + e2e pass + release   (~5 sub-beads)
```

**Labels (every bead):** `hangar`, `hangar-v1`, `phase:P{N}`

**Title convention:** `hangar:P{N}.{M} <terse-task>` (e.g. `hangar:P0.1 workspace + migrations table`)

**Filter usage:**
- `bd list -l hangar-v1` — full epic
- `bd list -l hangar -l phase:P3` — single phase
- `bd ready -l hangar-v1` — next claimable

**Per-bead loop (canonical, from `per_bead_tdd_review_loop_shape`):**
```
claim → /plan-tdd → implement → code-reviewer → fix-same-run → tests → bd close
```

## Handoff Targets ✓ green-lit

1. **Commit** this build-plan to `docs/hangar/build-plan.md`
2. **`/plan-tdd`** per phase to produce TDD-shaped task lists (5–9 tasks per phase)
3. **Create beads** from phase 2 outputs — epic + 10 phase parents + ~70 sub-beads with labels + deps
4. **`/make-a-goal`** to synthesise the autonomous-run mega-prompt at `.agents/goals/hangar-v1.md`
   - **Split risk:** 20w is large for one goal-mode run; split into `hangar-v1a` (P0–P4, "hangar walks") + `hangar-v1b` (P5–P9, "hangar runs") if budget is tight
5. **`/swarm-create`** per phase (or per goal) to dispatch waves
