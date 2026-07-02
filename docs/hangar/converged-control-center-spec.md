# Specification: Converged Control Center

**Generated from:** docs/hangar/converged-control-center-architecture.md (interview-locked v1)
**Interview date:** 2026-07-02
**Version:** 1.0
**Decisions:** D1–D19 (design) + T1–T9 (technology) — all locked; two Stevie overrides (D13 write-mode standup, T8 statig)

## Executive Summary

Converge ainb's four agent-control surfaces (fleet, hangar, web, channels) into one control
plane: the hangar daemon owns all durable state, an attention bus surfaces every input request
from every session on the host, and every surface — TUI, web, phone channels, and a standing
ATC session — is a view over the same store and the same RPCs. Kanban cards launch real agent
sessions with cross-tool agent profiles; everything is traced, costed, and OTel-exportable.

## Objectives

### Primary Goals
- One answerable inbox: every AskUserQuestion, approval, Codex request-user, error, and
  waiting state — from hangar-started AND hand-started sessions — lands in one attention
  table and is answerable from TUI, web, phone, or ATC, with the answer routed back into
  the session's open picker.
- Kanban-driven agent work: user-defined boards where a card's assignee is an agent
  profile and Run launches a real session (headless or interactive, Claude or Codex,
  always in tmux, attachable from the card).
- Autonomy preserved and re-plumbed: autopilots (cron) unchanged in UX; ATC survives as a
  standing attachable session whose mechanics (heartbeat, retry caps, state) move into
  the daemon and whose escalations reach the phone.
- Full traceability: every run recorded (tokens, cost, diff, outcome), per-session
  tool-call timeline, OTLP export, cost rollups.

### Success Metrics
- Zero invisible sessions: every tmux session on the host appears in the session registry
  (full-fidelity when hooked, degraded via pane fallback when not).
- An AskUserQuestion raised in ANY session can be answered from all four surfaces, with
  exactly-once delivery (first answer wins) and misroute-refusal (C1 guard).
- IDLE false-positives from pane polling eliminated (hook-status truth) and the cold
  pane fan-out (~920k tokens over 17 sessions measured) no longer occurs.
- Acceptance harness (extended verify-hangar-goal) green, including the previously
  untested legs: multi-workspace isolation, daemon kill-9 recovery, concurrent-dispatch
  cap, populated-DB migration.

## Scope

### In Scope (phases P0–P11)
- Unix-socket authentication (P0) — 0600 + SO_PEERCRED same-uid + wire existing token verify.
- Durable event bus: `events` outbox table + tokio broadcast; daemon emits HangarEvent at
  every FSM transition; `workspace/subscribe` returns real snapshot + deltas (P1).
- Attention pipeline: `attention` table, `answer` RPC (C1 ambiguity guard, first-answer-wins
  idempotency), verified tmux last-mile, TUI control-center screen with agentpeek cards (P2).
- Provider runner: Claude + Codex, headless + interactive YOLO, daemon-owned spawn in tmux,
  attach-from-card (P3).
- Boards: user-defined columns, column↔FSM-state mapping, per-board auto-move, run-from-card,
  add-task-in-any-column, card-green-on-success (P4).
- Agent profiles: canonical Claude subagent .md masters in `~/.agents-in-a-box/profiles/`
  with DB index + fs watch; Codex down-compile ([profiles.<slug>] + prompts/<slug>.md, WARN
  on dropped tools/color); logical model tiers; compile-on-dispatch; profile-editor TUI
  screen (P5).
- Concurrency: replace `idx_one_pending_task_per_issue` with per-(issue,agent) pending
  claim guard; migrate the task lifecycle onto statig typed transitions (P6).
- Squads: workspace-scoped team primitive (leader profile + member profiles with roles),
  issue-assignable, leader-routing dispatch, Squads TUI screen (P7).
- Web + bridge on the bus: web reads daemon RPC/events, gains answer buttons; bridge gains
  proactive outbound + "reply N" answer routing; Python bridge deleted (P8).
- ATC on the daemon: instance registry, heartbeat = daemon cron, state in store,
  escalation→attention rows, skills-over-socket (needs/broadcast/sequence as RPC wrappers);
  auto-/standup write-mode with guardrails (P9).
- Observability: run_history, per-session timeline, tracing-opentelemetry → OTLP export,
  cost rollups absorbed from fleet cost (P10).
- Acceptance harness extension + resilience legs (P11).

### Out of Scope (v1)
- Providers beyond Claude + Codex (cursor/copilot/gemini later behind the same trait).
- Workspace member management / invites / RBAC (schema stays data-only).
- tmux control-mode (`-C`) integration (deferred; revisit if pane fallback proves weak).
- ntfy.sh channel (later: one more bus consumer).
- Copilot input-request answering (Claude + Codex first, per journey C1).
- Porting `/swarm-create` bash into the daemon (it remains an independent external skill).

### Future Considerations
- croner parser swap if cron-crate DST edges bite (T7 note).
- Webhook-triggered autopilots (daemon has no HTTP ingress today; web could proxy).
- Issue comments/threads write path (schema exists, dead today).

## Technical Requirements

### Architecture
Single binary (`ainb`: tui · cli · daemon). Hangar daemon = single control plane
(D1). Five invariants:
1. Daemon owns all durable state; no surface keeps authoritative state.
2. One send path — `fleet::send` verified multi-line submit; no raw send-keys anywhere else.
3. Every input request reaches the attention table; no invisible sessions.
4. CLI/daemon do; TUI/web/channels show (and route answers).
5. Socket authenticates before any surface connects (P0 gates all).

### Components
| Component | Purpose | Technology |
|-----------|---------|------------|
| ainb-hangar-daemon | Control plane: store, bus, answer router, runner, cron, compiler, OTLP | Rust, tokio, sqlx/SQLite |
| Event bus | Durable pub/sub with replay | tokio broadcast + `events` outbox (seq cursor) (T1) |
| Attention pipeline | Ingest → fan-out → answer → last-mile | attention table + answer RPC + verified send |
| Provider runner | Spawn/resume/cancel agent sessions | ProviderRunner trait; claude + codex impls; portable-pty + tmux |
| Profile compiler | Canonical .md → tool-native files on dispatch | files + DB index + fs watch (T6) |
| Task FSM | Lifecycle typing | statig (T8, override) over SQL claim guard (D3) |
| Scheduler | Autopilot cron + ATC heartbeat | cron crate parser + DB-durable tick loop (T7) |
| TUI control center | Cards, board, attention, history, profiles, squads screens | ratatui plugin over JSON-RPC |
| ainb-web | SSE, VAPID push, PWA, WS terminal, answer buttons | axum; web-push crate (T9) |
| Bridge | TG/Slack/Discord proactive + reply routing | Rust bridge (Python deleted) (D18) |
| ATC | Standing LLM orchestrator session | Claude/Codex session + fleet skills over socket (D12) |
| OTLP exporter | Traces + metrics | tracing-opentelemetry + opentelemetry-otlp (T5) |

### Store additions (daemon SQLite, sqlx)
`attention(id, session_id, workspace_id?, kind, payload, state, created, answered_by, answer)`
with kind ∈ {ask_user_question, approval, codex_request_user, error, waiting, escalation};
`events(seq, ts, type, entity, payload)`; `board(id, workspace_id, name)`;
`board_column(board_id, ord, name, fsm_state?, auto_move)`; `profile(slug, tier, mtime)`
(index only — body on disk); `squad(id, workspace_id, name, leader_profile)`;
`squad_member(squad_id, profile_slug, role)`; `run_history(run_id, task_id?, session_id,
provider, profile, started, finished, outcome, tokens_in, tokens_out, cost, diff_add,
diff_del)`; `session_registry(session_id, source, cwd, tmux_name, hooked, last_status, …)`;
`cost_rollup(day, project, session_id, tokens, cost)`.
Migration: drop `idx_one_pending_task_per_issue`; add `UNIQUE(issue_id, agent_id) WHERE
state='pending'` + NOT-EXISTS claim query (D3). Migration must be tested against a
populated fixture DB (known blind spot).

### Integrations
- Lifecycle hooks (Claude + Codex notify): events.jsonl → ingest → attention/session state.
- notifyd: ingest writes through to attention; remains macOS-notification consumer (D10).
- Grafana Cloud (or any OTLP endpoint): from onboarding's OTel step credentials.
- Telegram/Slack/Discord: existing transports + new outbound worker on attention events.

### Performance Requirements
- Attention fan-out latency (raise → surface visible) < 2s on the hooked path.
- No pane capture on the hot read path for hooked sessions (token cost elimination).
- Event catch-up after daemon restart: subscribers resume from seq without loss.

### Security Requirements
- P0 socket auth before any surface integration (D2). Capability checks per RPC retained.
- Bridge secrets stay env/keychain-resolved, never argv/unit files (existing contract).
- Web bearer-token auth unchanged; answer buttons go through the same authenticated RPC.
- Auto-standup and all daemon writes go through the verified send path with per-session
  opt-out — no unauthorised pane injection.

## User Experience

### User Flows
1. **Board launch (J1–J3):** create card in any column → assignee picker = profile picker →
   Run ▾ (headless | interactive) → daemon compiles profile, spawns tmux session → card shows
   live status + stat strip → attach from card at any time → on success card turns green and
   auto-moves per column mapping.
2. **Answer from anywhere (C1):** session raises AskUserQuestion → card auto-shuffles to top
   in the control center, phone gets "session X asks: … ①②③", web push fires → answer from
   any surface → daemon validates (C1 guard) → verified send lands the pick in the session's
   open picker → all surfaces show answered(by).
3. **ATC autonomy (C3):** daemon cron nudges ATC with the attention snapshot → ATC answers
   confident items via the answer RPC, escalates the rest (kind=escalation → phone/web push)
   → Stevie replies from the phone; "reply 2" routes back.
4. **Auto-standup (C4, D13):** session stagnant ≥15 min AND idle-at-prompt → daemon writes
   `/standup` via verified send (global toggle, per-session opt-out, 60-min cooldown,
   max 1 concurrent) → result captured from JSONL → "standup ready" on the card.
5. **Profiles (J1):** profile editor screen creates/edits `~/.agents-in-a-box/profiles/<slug>.md`
   → preview both compile targets → WARN on Codex-dropped fields → used at next dispatch.

### Edge Cases
| Scenario | Expected Behavior |
|----------|-------------------|
| Two surfaces answer the same ASK | First answer wins; second gets "already answered by X" |
| Ambiguous target session (C1) | Answer refused with explanation, never misrouted |
| Unhooked session raises a question | Pane classifier surfaces it (degraded); still answerable |
| Daemon restarts mid-run | Orphan reclaim + subscribers resume from event seq; board state correct |
| Codex session, no picker semantics | request-user payload rendered as free-text answer card |
| Profile edited on disk mid-dispatch | mtime index refresh; in-flight task keeps its compiled snapshot |
| Auto-standup while user typing | Never fires — idle-at-prompt gate from hook status, not pane |
| Column deleted while cards on it | Cards fall back to unmapped pool; no data loss |
| Bridge offline | Attention items queue (bus is durable); outbound worker drains on reconnect |

## Constraints & Dependencies

### Technical Constraints
- No second server process (T1) — everything in the one daemon.
- CTS catalogue is append-only (T3) — existing RPCs keep wire compatibility.
- `.md` profile format must stay Claude-native-compatible (D14).
- tmux safety: exact-name session operations only; all writes via verified send.

### External Dependencies
- statig crate (T8), opentelemetry/tracing-opentelemetry/opentelemetry-otlp (T5) — new deps.
- Codex notify-hook parity (landed PR #366) — fidelity of Codex coverage depends on it.

### Timeline Constraints
- P0 is a hard gate; nothing integrates before it.
- Each phase independently shippable; additive to the TUI (no regression rule).

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Codex answering fidelity lags Claude | Med | High | Pane fallback + phase Codex answering after Claude proves the pipeline |
| statig migration regresses tested lifecycle | High | Med | Fold into P6 with acceptance tests as net; SQL guard is the enforcement anyway |
| Populated-DB migration breaks (claim-guard swap) | High | Med | Migration test on populated fixture — explicit P11 leg |
| notifyd↔daemon dual-write window | Med | Low | Write-through from day one; no dual-authority period |
| Auto-standup interrupts work | Med | Med | Idle-at-prompt gate from hook truth + cooldown + opt-out |
| Event fan-out perf with many sessions | Low | Low | Local unix socket; measure before web SSE repoint |
| Daemon becomes single point of failure | High | Med | P11 resilience legs: kill-9 recovery, dispatch cap, restart-resume |

## Decisions Made

Full register: D1–D19 + T1–T9 in docs/hangar/converged-control-center-architecture.md §3 + §8.
Stevie overrides (deliberate, locked): D13 auto-standup writes into the pane (guardrailed);
T8 statig adopted for the task FSM.

### Deferred Decisions
- tmux control-mode adoption (T4) — revisit on fallback weakness evidence.
- croner parser swap (T7) — on DST failure evidence.
- ntfy.sh consumer (T9) — post-v1.
- fleet `workflows/hangar.js` rename target name (candidate `jarvis`) — cosmetic, pick at P8.

## Implementation Notes

### Priority Order (phases; deps in architecture §7)
1. P0 socket auth (gate)
2. P1 event producer + hook-status read
3. P2 attention bus + answer RPC + control-center screen
4. P3 provider runner (Claude+Codex) · P5 profiles (parallel)
5. P4 boards (needs P3+P5)
6. P6 concurrency guard + statig migration
7. P7 squads
8. P8 web+bridge retarget · P9 ATC re-plumb + auto-standup (parallel, need P2)
9. P10 observability (incremental throughout)
10. P11 acceptance harness + resilience legs (last)

### Technical Debt Accepted
- Two SQLite layers (sqlx daemon / rusqlite host) — permanent, boundary-enforced (T2).
- Workspace member tables stay schema-only (no RBAC) in v1.
- Codex profile compile is lossy (tools/color dropped) — WARN, not solved.

## Open Questions
- [ ] Exact statig state-machine shape for retry/timeout sub-states (design at P6 kickoff).
- [ ] Attention payload schema for Codex approval prompts (inspect real Codex wire at P2).
- [ ] Board column config UX (inline TUI editing vs config file) — decide at P4 with a mock.

---
*Generated through systematic interview (7 rounds, 28 locked decisions) on the plan in
docs/hangar/converged-control-center-architecture.md.*
