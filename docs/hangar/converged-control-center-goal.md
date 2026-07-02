# GOAL: Converged Control Center — one plane for every agent on the host

> Drop this file into a fresh Claude Code session on the `agents-in-a-box` repo to run
> end-to-end without hand-holding. Everything below is decided — 28 interview-locked
> decisions (D1–D19 design, T1–T9 tech). Do not relitigate them; build them.

## Outcome (definition of shipped)

The hangar daemon is the single control plane for every agent session on this host.
A TUI control-center screen shows every session as an agentpeek-style card (live status,
token/diff/tool/age strip, LAST REPLY, tool-call timeline), auto-shuffles needs-input to
the top, and lets Stevie answer any AskUserQuestion / approval / Codex request-user
inline — with the same answer possible from web, phone (Telegram/Slack/Discord), or the
standing ATC session, routed back into the target session's open picker exactly once.
Kanban boards with user-defined columns launch real agent sessions (headless `claude -p` /
`codex exec` OR interactive YOLO — both in tmux, attachable from the card) under
cross-tool agent profiles. Squads fan multiple agents onto one issue. Autopilots keep
working. Every run is recorded (tokens, cost, diff, outcome) with OTLP export. The
extended acceptance harness is green including the resilience legs.

## Canon (read before any code)

1. `docs/hangar/converged-control-center-architecture.md` — topology, 19 decisions,
   store schema, subsystems, phase DAG, tech register (§8), risks. THE source of truth.
2. `docs/hangar/converged-control-center-spec.md` — objectives, edge cases, flows,
   success metrics, open questions.
3. `ainb-tui/CLAUDE.md` + repo conventions (worktrees, tripwires, additive-TUI rule).
4. Explainer (visual): https://explainers.stevengonsalvez.com/ainb-control-center-arch/

## Hard invariants (violating any = stop and fix)

- INV-1 Daemon owns all durable state; no surface keeps authoritative state.
- INV-2 One send path: `fleet::send` verified multi-line submit. No raw send-keys anywhere.
- INV-3 Every input request reaches the `attention` table. No invisible sessions.
- INV-4 CLI/daemon do; TUI/web/channels show (and route answers).
- INV-5 Socket auth (P0) lands before ANY surface integrates. Non-negotiable gate.
- TUI features are additive — never regress an existing screen (repo standing rule).
- tmux safety: kill sessions by exact name only; never kill-server/pkill/wildcards.

## Stevie overrides already locked (do not "improve" them back)

- D13: auto-`/standup` WRITES into stagnant panes — guardrails: fires only when hook
  status says idle-at-prompt, global toggle, per-session opt-out, 60-min cooldown,
  max 1 concurrent.
- T8: task FSM migrates to `statig` typed transitions (SQL claim guard stays the
  DB-level enforcer).

## Phases (dependency-ordered; each independently shippable)

Execute as a beads epic: one bead per phase, sub-beads per deliverable. Per-bead loop:
claim → /plan-tdd → implement → code-review (opus) → fix findings → full test gate →
close. Never bulk-commit; atomic commits per concern via named paths.

### P0 — Authenticate the unix socket (GATE)
- chmod 0600 on bind; SO_PEERCRED same-uid check per connection; wire the existing
  `core::token` mint/verify (currently test-only) into serve()/handle().
- Accept: unauthorised local process gets refused (tripwire proves it); all existing
  CTS conformance tests still green.

### P1 — Event producer + hook-status read path
- Emit `HangarEvent` at every FSM claim/finalize/scheduler transition (producer is
  absent today — consumers already decode). Durable `events(seq, ts, type, entity,
  payload)` outbox; `workspace/subscribe` returns real snapshot + deltas; subscribers
  resume `FROM seq>N` (T1).
- Hook status files become the session-state truth; capture-pane demoted to fallback
  adapter for unhooked sessions (D5, D11).
- Accept: TUI receives live events without polling; restart-resume from seq proven.

### P2 — Attention bus + answer RPC + control-center screen
- `attention` table (kinds: ask_user_question | approval | codex_request_user | error |
  waiting | escalation); ingest from hooks pipeline (notifyd writes through) + needs
  classifier + pane fallback.
- `answer` RPC: C1 cwd-ambiguity guard (lift from fleet panel), first-answer-wins
  idempotency, last-mile via verified send into the open picker.
- TUI control-center screen: agentpeek cards — live status line, auto-shuffle
  needs-input to top (no focus steal), token/diff/tool/age strip, LAST REPLY pane,
  tool-call TIMELINE with durations (D9). Inline answering.
- Accept: ASK raised in a manual session answered from the TUI and delivered; tripwire
  per card element; double-answer returns "already answered".

### P3 — Provider runner (Claude + Codex)
- `ProviderRunner` trait: headless (`claude -p` / `codex exec`) + interactive YOLO,
  both spawned by the daemon in tmux; resume/cancel; portable-pty conventions (D6, D7).
- Accept: both modes × both providers spawn, appear in session_registry, attachable.

### P4 — Boards (needs P3 + P5)
- `board`/`board_column` tables; user-defined columns; column↔FSM-state mapping;
  per-board auto-move toggle; add-task-in-any-column; Run ▾ from card; green-on-success;
  attach-from-card (D8).
- Accept: custom 5-column board e2e tripwire — create card → run → succeeded → card
  green + auto-moved.

### P5 — Agent profiles
- Canonical Claude subagent `.md` masters in `~/.agents-in-a-box/profiles/` + DB index
  (slug, tier, mtime) + fs watch (T6). Codex down-compile: `[profiles.<slug>]` +
  `~/.codex/prompts/<slug>.md`, WARN on dropped tools/color (D14). Logical model tiers
  resolved per provider at launch (D15). Compile-on-dispatch into the task env (D16).
- Profile-editor TUI screen: create/edit/preview both compile targets.
- Accept: same profile drives a Claude and a Codex run; edit-on-disk picked up by watch.

### P6 — Concurrency + statig
- Drop `idx_one_pending_task_per_issue`; add `UNIQUE(issue_id, agent_id) WHERE
  state='pending'` + NOT-EXISTS claim (D3). Migration tested on a POPULATED fixture DB.
- Migrate task lifecycle onto statig typed transitions (T8); acceptance tests are the net.
- Accept: two agents run one issue in parallel (tripwire); all lifecycle tests green.

### P7 — Squads
- `squad`/`squad_member` tables; leader profile receives the brief and routes members
  via daemon dispatch (D17). Squads TUI screen (members, roles, live status).
- `/swarm-create` bash is untouched — independent external skill.
- Accept: issue assigned to a squad → leader session briefs → ≥2 member tasks claimed
  in parallel (needs P6).

### P8 — Web + bridge on the bus
- ainb-web reads daemon RPC/events (drop its own polling); answer buttons on ASK cards;
  terminal/push/PWA unchanged (D18).
- Bridge outbound worker on attention events (proactive: "session X asks … ①②③");
  inbound "reply 2" → answer RPC. Bridge's private tmux send replaced with fleet::send
  (INV-2). Delete `plugins/ainb-fleet/bridge/` (Python). Rename the colliding fleet
  workflow `workflows/hangar.js` (candidate: `jarvis`).
- Accept: phone receives an escalation push and answers it; web button answers an ASK.

### P9 — ATC on the daemon + auto-standup
- ATC instance registry in store; heartbeat = daemon cron (retire launchd side-files);
  retry caps + state in store; task-log.md kept as audit. Skills-over-socket: needs →
  attention query, broadcast/sequence → daemon send RPCs (D12). Escalations →
  attention rows (kind=escalation) → phone/web push.
- Auto-standup per D13 with ALL guardrails.
- Accept: ATC answers a confident ASK via RPC; escalation reaches the phone; standup
  fires on an idle session and never on a busy one (tripwire both).

### P10 — Observability (incremental, alongside)
- `run_history` rows; per-session timeline; `cost_rollup` absorbing fleet cost;
  tracing-opentelemetry bridge + opentelemetry-otlp exporter with token/cost/duration
  metrics at run boundaries (T5, D19). Endpoint from onboarding's OTel step.
- Accept: a board-launched run appears in history with tokens+cost; OTLP span visible
  at a local collector.

### P11 — Acceptance harness + resilience (last)
- Extend verify-hangar-goal.md → verify-converged-goal.md: existing F-legs + attention/
  answer legs + the blind spots: multi-workspace isolation (seeded cross-tenant leak
  assertion), daemon kill-9 restart recovery, concurrent-dispatch cap, populated-DB
  migration.
- Accept: full harness green on mac; CI wired.

## Working rules for the run

- Branch off `feat/hangar-parity` (or its successor); PR per phase into the epic branch;
  merge commits, never squash. Merges to main are Stevie-only.
- Tests: tripwire per user-visible behaviour (follow /tmux-ui-tripwire gotchas);
  `cargo test --workspace --no-fail-fast` gate before every phase close; never trust a
  subagent's "green" claim — re-run yourself.
- No `cargo fmt` at crate/workspace level (rustfmt single files only); no
  `cargo clippy --workspace -- -D warnings`.
- Commits human-authored style, conventional format, no AI mentions.
- When a genuine decision fork appears that the canon does not cover: AskUserQuestion.
  Everything covered above is NOT a fork.

## Anti-goals

- Do not build a second daemon/broker (T1). Do not adopt jsonrpsee/tonic (T3).
- Do not unify the sqlite layers (T2). Do not port swarm bash into the daemon (D17).
- Do not add providers beyond Claude+Codex in v1 (D7). Do not build member RBAC (v1).
- Do not weaken any auto-standup guardrail (D13) or skip the P0 gate (D2/INV-5).
