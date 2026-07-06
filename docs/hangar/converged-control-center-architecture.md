# Converged Control Center — Systems Architecture

**Status:** interview-locked draft v1 — pending Stevie review pass
**Date:** 2026-07-02
**Inputs:** ultracode digest (hangar⇆multica parity matrix, agent-deck-vs-fleet assessment,
agentpeek UX, fleet plugin skill inventory, agent-profile landscape, squads/workspaces gap),
19-fork interview (all resolved).

---

## 1. One-line thesis

The **hangar daemon becomes the single control plane** for every agent on the host —
managed (board-launched) and unmanaged (hand-started) — and every surface
(TUI, web, phone channels, ATC session) becomes a **view over one store and one event bus**,
with fleet's discovery/classification/send code demoted to libraries the daemon uses.

---

## 2. Target topology

```
                 ┌───────────────────────────────────────────────────────────┐
                 │                 ainb (ONE binary)                         │
                 │  tui · cli verbs · `ainb daemon` (hangar control plane)   │
                 └───────────────────────────┬───────────────────────────────┘
                                             │
     PRODUCERS                        ┌──────▼──────────────────────────┐        CONSUMERS / VIEWS
┌───────────────────┐                 │      HANGAR DAEMON              │   ┌───────────────────────────┐
│ lifecycle hooks   │──events.jsonl──▶│  ┌──────────────────────────┐   │   │ TUI control center        │
│ (claude + codex)  │   (ingest)      │  │ SQLite store             │   │──▶│  board·attention·history  │
├───────────────────┤                 │  │  issues·tasks·runs·      │   │   ├───────────────────────────┤
│ needs classifier  │──lib call──────▶│  │  squads·profiles·boards· │   │──▶│ ainb-web                  │
│ (fleet lib)       │                 │  │  attention·events·cost   │   │   │  SSE·push/PWA·terminal    │
├───────────────────┤                 │  └──────────────────────────┘   │   ├───────────────────────────┤
│ task FSM          │──internal──────▶│  event bus (HangarEvent push)   │──▶│ bridge TG/Slack/Discord   │
│ (claim/finalize)  │                 │  answer router (answer RPC)     │   │  proactive + reply-back   │
├───────────────────┤                 │  provider runner (claude·codex) │   ├───────────────────────────┤
│ pane fallback     │──degraded──────▶│  cron scheduler (autopilots)    │──▶│ ATC session (standing)    │
│ (unhooked sess.)  │                 │  profile compiler              ─┤   │  claude/codex, attachable │
└───────────────────┘                 │  OTLP exporter                  │   └───────────────────────────┘
                                      └──────────────┬──────────────────┘
                                                     │ spawns / sends (ONE send path)
                                      ┌──────────────▼──────────────────┐
                                      │ tmux sessions                   │
                                      │  managed: board/autopilot/squad │
                                      │  unmanaged: hand-started        │
                                      └─────────────────────────────────┘
```

**Invariants (write into ainb-tui/CLAUDE.md):**

1. The daemon owns all durable state. No surface keeps authoritative state of its own.
2. One send path (`fleet::send` with verified multi-line submit). No surface does raw send-keys.
3. Every input-request from any session reaches the attention table. No invisible sessions.
4. CLI/daemon **do**; TUI/web/channels **show** (and route answers).
5. Socket is authenticated before any surface connects (P0).

---

## 3. Decisions register (interview-locked)

| # | Area | Decision | Rejected alternative |
|---|------|----------|---------------------|
| D1 | Spine | Hangar daemon = single control plane; fleet → libraries; claude-peers broker retired; `AINB_FLEET_TRANSPORT` duality removed | Two planes + shared bus |
| D2 | Security | P0 socket auth (0600 + SO_PEERCRED same-uid + wire existing token verify) is bead #1 and gates all surface integration | Parallel with phase 1 |
| D3 | Concurrency | N agents per issue: replace `idx_one_pending_task_per_issue` with per-(issue,agent) claim guard (multica semantics) | One agent per issue |
| D4 | Scope | Converge all three legacy layers (fleet plugin, swarm bash, host). Swarm bash **survives as an independent skill outside ainb** — not the ainb-internal team mechanism, not ported | Fleet+hangar only |
| D5 | Events | Daemon emits `HangarEvent` at every FSM transition (producer currently absent); hook status files = session-state truth; capture-pane demoted to fallback | Snapshot-poll |
| D6 | Launch | Daemon-owned task→session spawn: headless (`claude -p` / codex exec) **and** interactive YOLO, both in tmux; attach-from-card; FSM-driven column auto-move | Caller-side spawn |
| D7 | Providers | One shared provider runner: Claude + Codex in v1 (headless + interactive each); cursor/copilot/gemini later behind same trait | Claude-only v1 |
| D8 | Board | User-defined columns per board; column↔FSM-state mapping; per-board auto-move toggle; add task in any column | Fixed 4-col FSM |
| D9 | Cards | All four agentpeek elements v1: live status line + auto-shuffle (needs-input to top, no focus steal); token/diff/tool/age stat strip; LAST REPLY pane; tool-call TIMELINE with durations | Subset |
| D10 | Bus | Attention table lives in the hangar store; notifyd's hook-ingest pipeline becomes a producer; notifyd remains the OS-notification consumer | notifyd sqlite as bus |
| D11 | Coverage | Hooks-primary (Claude + Codex notify) + pane-fallback adapter for unhooked sessions (degraded fidelity, still visible) | Hooked-only |
| D12 | ATC | Standing, attachable Claude/Codex session; consumes bus via skills over the socket; daemon owns heartbeat/retry/state mechanics; ATC = thin LLM policy that can broadcast/correct/answer | Absorb into daemon |
| D13 | Standup | Auto-`/standup` **written into stagnant panes** (Stevie's call). Guardrails: fires only when session idle at prompt (never mid-turn), global toggle, per-session opt-out, cooldown per session | Read-only standup |
| D14 | Profile format | Claude subagent `.md` = canonical lossless master; Codex down-compiles to `[profiles.<slug>]` + `~/.codex/prompts/<slug>.md` with WARN on dropped fields (tools, color) | Neutral third format |
| D15 | Profile model | Logical tier (`premium/balanced/fast`) resolved per provider at launch | Concrete model / per-tool map |
| D16 | Profile wiring | Board assignee slug **is** the profile slug; daemon compiles tool-native files **on-dispatch** into the task's execution env; profile editor = its own TUI screen | Pre-sync |
| D17 | Teams | Daemon-native squads (leader + member profiles, workspace-scoped, issue-assignable, leader-routing) are the ainb team mechanism. `/swarm-create` remains an unrelated portable skill | Bash-driven squads |
| D18 | Web+bridge | Web + bridge become daemon consumers. Bridge gains proactive outbound (attention items → phone; "reply 2" → answer RPC). Python bridge dir deleted | Read-retarget only |
| D19 | Observability | ALL v1: run history + per-session timeline in TUI; OTLP export (traces+metrics); cost rollups in store/cards; acceptance harness extended (converged surface + workspace-isolation + daemon-restart legs) | Phased |

---

## 4. Component architecture

### 4.1 Store (SQLite, daemon-owned)

Existing hangar tables (workspace, issue, task, agent, autopilot, member) plus:

```
attention(id, session_id, workspace_id?, kind, payload, state, created, answered_by, answer)
  kind: ask_user_question | approval | codex_request_user | error | waiting | escalation
events(seq, ts, type, entity, payload)          -- durable event log (bus history)
board(id, workspace_id, name)                    -- boards
board_column(board_id, ord, name, fsm_state?, auto_move bool)
profile(slug, body_md, model_tier, updated)      -- canonical agent profiles
squad(id, workspace_id, name, leader_profile)
squad_member(squad_id, profile_slug, role)
run_history(run_id, task_id?, session_id, provider, profile, started, finished,
            outcome, tokens_in, tokens_out, cost, diff_add, diff_del)
session_registry(session_id, source, cwd, tmux_name, hooked bool, last_status, ...)
  source: board | autopilot | squad | manual | atc
cost_rollup(day, project, session_id, tokens, cost)
```

Migration note (D3): drop `idx_one_pending_task_per_issue`, add
`UNIQUE(issue_id, agent_id) WHERE state='pending'` claim guard + NOT-EXISTS claim query.

### 4.2 Event bus

- Producer: every FSM transition, attention insert/answer, autopilot fire, squad dispatch,
  session status change → `HangarEvent` frame on the existing (currently consumer-only)
  `hangar/event` notification channel + row in `events`.
- Consumers: TUI plugin stream (already decodes), web SSE fan-out, bridge outbound worker,
  ATC feed, notifyd (OS notifications), OTLP exporter.
- Snapshot + subscribe: `workspace/subscribe` returns a real snapshot then deltas
  (today it acks empty — this is the producer-absent hole being closed).

### 4.3 Attention pipeline (the control-center core)

```
session emits AskUserQuestion / approval / request-user / error / waiting
      │ (hook event → events.jsonl → ingest)          (unhooked: pane classifier)
      ▼
attention row (state=open) ──HangarEvent──▶ all surfaces
      │
      ├─ TUI control center: card jumps to top (auto-shuffle, no focus steal)
      ├─ web: SSE update + VAPID push (if not focused)
      ├─ bridge: proactive message "main is asking: … ① ② ③"
      └─ ATC: bus feed line in its session
      ▼
answer arrives from ANY surface ──▶ answer RPC (daemon)
      │  guard: target-session ambiguity check (C1), idempotent (first answer wins)
      ▼
last-mile delivery: tmux send into the target session's open picker / prompt
      ▼
attention row state=answered(by=surface) ──HangarEvent──▶ surfaces update
```

- AskUserQuestion structured answering (pick option N) — lifted from the TUI fleet panel
  into the daemon `answer` RPC; every surface calls the same RPC.
- Codex `request-user` and approval prompts are additional `kind`s with their own
  payload shapes; same routing.
- The last mile is session-side by nature (picker must receive keys in the pane);
  the daemon owns dispatch + verification (multi-line verified submit).

### 4.4 Provider runner

```
trait ProviderRunner {
    headless(task, profile, env) -> tmux session   // claude -p / codex exec
    interactive(task, profile, env) -> tmux session // claude YOLO / codex yolo
    resume(session), cancel(session)
}
impl: claude (v1), codex (v1); cursor/copilot/gemini later
```

- Both modes run in tmux (attach always possible, even for headless).
- On-dispatch profile compilation (D16): daemon writes the Claude `.md` /
  Codex profile+prompt into the task's execution env before spawn.
- Model tier resolution (D15) at spawn time per provider.

### 4.5 Boards & kanban

- Board config per D8; card = issue; task launch from card via `Run ▾`
  (headless / interactive) using assignee profile.
- FSM state changes emit events → auto-move card when the column mapping says so;
  card turns green on `succeeded`.
- N-agents-per-issue (D3) renders as multiple task chips on one card.

### 4.6 Squads (D17)

- Squad assignable to an issue: leader profile receives the brief (a real session),
  routes work to member profiles as tasks (daemon dispatch, not bash).
- Squad screen: members, roles, per-member live status (from session_registry).
- `/swarm-create` bash remains an unrelated standalone skill; its sessions appear
  on the bus like any manual session (hooks), but ainb does not manage them.

### 4.7 ATC (D12)

- `ainb fleet atc setup` migrates to daemon-managed: instance registered in store,
  heartbeat = daemon cron (not launchd/systemd side-files), state in store
  (task-log.md kept as human-readable audit).
- ATC session consumes the bus: skills (`ainb-fleet` plugin) talk to the daemon socket —
  `needs` = attention query, `broadcast/sequence` = daemon send RPCs.
- Escalation = attention row (kind=escalation) → proactive bridge push + web push
  (today: dead-ends in task-log.md).
- ATC feed options (brainstorm outcome to refine in spec): heartbeat prompt injection
  (today's model) vs socket-streamed feed the session polls via skill; v1 keeps
  heartbeat injection, adds `atc feed` skill for on-demand pull.

### 4.8 Auto-standup (D13, Stevie-overridden)

- Trigger: session stagnant N min (default 15) **and** status=idle-at-prompt
  (hook status = turn ended; never mid-turn).
- Action: daemon sends `/standup` via the verified send path.
- Guardrails: global `autostandup.enabled`, per-session opt-out,
  per-session cooldown (default 60 min), max concurrent standups (1),
  result captured from JSONL end-turn → attention/card "standup ready".

### 4.9 Observability (D19)

- `run_history` row per run; per-session timeline (tool calls + durations) read from
  JSONL, rendered in TUI (agentpeek TIMELINE) and stored summary in history.
- OTLP exporter: spans task→run→(tool-call events), metrics (tokens, cost, durations);
  endpoint config reuses onboarding's OTel step (Grafana Cloud creds).
- Cost: fleet `cost` verbs fold into daemon rollups; card stat strip shows tokens/cost.
- Acceptance: extend verify-hangar-goal.md → verify-converged-goal.md
  (existing F01-F44 + bus/answer legs + workspace-isolation proof + daemon-restart
  + concurrent-dispatch legs — the current blind spots).

### 4.10 Web & channels (D18)

- ainb-web keeps: WS terminal, VAPID push, PWA, SSE — repointed to daemon RPC/events;
  ADDS structured answer buttons on ASK cards (answer RPC), terminal remains for free-form.
- Bridge (Rust): TG/Slack/Discord; gains outbound worker consuming attention events;
  inbound "reply 2" / free text routed via answer RPC (target inference by
  conductor-prefix routing as today). Python `plugins/ainb-fleet/bridge/` deleted.

---

## 5. Fleet plugin skills disposition

| Skill | Disposition |
|---|---|
| `ainb-fleet` (overview) | keep — routes to sub-skills, text updated |
| `standup` | keep — becomes thin wrapper over daemon session/attention query |
| `needs` / `fleet-needs` | keep — wrapper over attention RPC (was CLI classify) |
| `broadcast` | keep — wrapper over daemon send RPC (targeting flags unchanged) |
| `sequence` | keep — wrapper over daemon ack-gated send |
| `daemon` | fold — daemon lifecycle = `ainb daemon` verbs; skill updated |
| `hangar` workflow (`workflows/hangar.js`) | **rename** (name collides with the control plane; e.g. `jarvis` / `fleet-panel`) |
| Python bridge dir | delete (D18) |

CLI verbs (`ainb fleet …`) all survive — same UX, daemon-backed internals.

---

## 6. User journeys → components (traceability)

| Journey (Stevie) | Served by |
|---|---|
| J1 kanban w/ custom columns, task in any column, agent + profile pick | Boards (4.5) + profiles (D14-16) + provider runner |
| J2 run headless (`-p`) or interactive YOLO, per provider | Provider runner (4.4) |
| J3 attach from card; tmux always; green on done; auto-move | D6 + D8 + event bus |
| J4 autopilot cron kept | Existing autopilot + scheduler (unchanged surface, daemon cron) |
| J5 full history/traceability + OTel | 4.9 |
| J6 explainer feature set | Parity matrix beads (35 proposed) folded into phases |
| C1 every input surfaced + answerable, ALL sessions | Attention pipeline (4.3) + coverage D11 |
| C2 web/channels same ecosystem | 4.10 |
| C3 ATC session notified + broadcast/correct via skills | 4.7 + skills disposition |
| C4 agentpeek UX (shuffle, standup) | D9 cards + D13 auto-standup |
| C5 squads/workspaces purposed | D17 squads; workspaces kept (already parity), member-mgmt stays schema-only v1 |

---

## 7. Phases (each independently shippable)

```
P0  auth socket (D2)                                  ── gate ──┐
P1  event producer + hook-status read (D5)                      │
P2  attention bus + answer RPC + TUI control center (4.3, D9)   │ core
P3  provider runner claude+codex + daemon-owned launch (D6,D7)  │
P4  boards: custom columns + auto-move + profile-on-card (D8,D16)
P5  profiles: store + editor screen + compilers (D14-16)
P6  concurrency claim guard swap (D3)
P7  squads (D17)
P8  web+bridge retarget + proactive outbound (D18)
P9  ATC on daemon + skills-over-socket (D12) + auto-standup (D13)
P10 observability: history/timeline/OTel/cost (D19)
P11 acceptance harness extension + resilience legs (D19)
```

Dependencies: P0→everything; P1→P2; P3→P4; P5→P4(profile pick); P2→P8,P9.
P6 anytime after P1. P10 incremental alongside.

---

## 8. Technology register (interview-locked, 2026-07-02)

Buy-vs-build decisions, grounded against what the workspace already ships.

| # | Concern | Decision | Notes |
|---|---------|----------|-------|
| T1 | Event bus | **tokio::sync::broadcast + SQLite outbox** — durable `events` table with monotonic seq; subscribers catch up `FROM seq>N` then go live | Broker already exists in `ainb-hangar-daemon/src/events.rs` (producer-less today). No external broker (NATS/iggy/zenoh rejected: second daemon on a laptop control plane) |
| T2 | SQLite layer | **Daemon = sqlx, host = rusqlite, RPC boundary between them** | The two layers never meet: convergence forbids host code touching DBs directly (INV-1). No unification churn |
| T3 | RPC | **Keep custom JSON-RPC + locked CTS catalogue** | jsonrpsee/tonic rejected — would rewrite server+SDK+CTS for zero capability. New RPCs (`answer`, `attention/*`, `board/*`) extend the catalogue |
| T4 | tmux | **send-keys with verified submit for writes; hooks+JSONL = state truth; capture-pane fallback** | tmux control mode (`-C`) deferred — would duplicate hook-derived state; revisit only if the unhooked-session fallback proves too weak |
| T5 | OTel | **tracing-opentelemetry bridge + opentelemetry-otlp exporter** | Existing `tracing` instrumentation becomes OTLP traces; token/cost/duration metrics via the meter at run boundaries. No dual instrumentation |
| T6 | Profile storage | **Files on disk (`~/.agents-in-a-box/profiles/<slug>.md`) + DB index row (slug, tier, mtime) + fs watch** | Human-editable, git-able, matches the `~/.claude/agents` workflow. DB-only rows rejected |
| T7 | Scheduler | **Keep `cron` crate parser (wrapped) + own DB-durable tick loop** (`autopilot.next_tick_at`) | The loop must stay ours — restart survival lives in the DB. `croner` parser swap is a cheap later change if DST edges bite. tokio-cron-scheduler rejected (in-memory jobs duplicate the DB layer) |
| T8 | Task FSM | **Adopt `statig`** — typed compile-time state machine for the task lifecycle (Stevie override of the keep-hand-rolled recommendation) | The SQL per-(issue,agent) claim guard remains the DB-level enforcer; statig types the in-process transitions. Migration folds into the P6 concurrency work; existing acceptance tests are the safety net |
| T9 | Push / channels | **As-is** — `web-push` crate (crypto delegated) + p256 keygen; the attention bus is the unified notification service (web push worker, bridge outbound, notifyd OS-notify are all just consumers) | ntfy.sh possible later as one more consumer; not v1 |

## 9. Risks

| Risk | Mitigation |
|---|---|
| Codex hook fidelity < Claude (request-user detection) | notify-hook parity work already landed (PR #366); pane fallback catches the rest; Codex answering may lag Claude one phase |
| Auto-standup interrupts a busy session | idle-at-prompt gate from hook status (not pane heuristics) + cooldown |
| Store migration on populated DBs (claim-guard swap) | migration test on populated fixture (current blind spot — explicit acceptance leg) |
| Two-store transition period (notifyd ↔ hangar) | notifyd ingest writes-through to attention table from day one; no dual-authority window |
| Multi-workspace isolation unproven | isolation tripwire in P11 (seeded cross-tenant leak assertion) |
| Event-push perf with many sessions | events table + fanout is local unix socket; measured before web SSE repoint |
