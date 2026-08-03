# Plan: Fleet Chat + Fleet Copilot, both surfaces (buzz-port part 2)

**From spec:** plans/2026-07-31-buzz-port-02-fleet-chat-copilot-spec.md
**Research:** research/2026-07-31_14-56-19_buzz-acp-port.md · research/2026-07-31_acp-resume-steering-spike.md
**Depends on:** plans/2026-07-31-buzz-port-01-chat-bus-acp.md (part 1: chat bus + ainb-acp AgentPool; generating at plan time, see Phase 0 gate)
**Date:** 2026-07-31
**Code roots:** `ainb-tui/crates/` (daemon, proto, fleet-core, plugins) · `apps/ainb-fleet-macos/`

Contract-first: this plan is written against a DRAFT contract (below). Phase 0 reconciles it against part 1's landed plan before any implementation phase starts. Where they disagree, part 1 wins and this file is amended.

## Architecture

```
┌──────────────┐            ┌──────────────┐
│ macOS Fleet  │            │ TUI chat     │
│ sidebar+pane │            │ screen       │
│ +inspector   │            │ (plugin)     │
└──────┬───────┘            └──────┬───────┘
       │ fleet/* over hangar.sock  │ (host/unix_socket_dial)
       ▼                           ▼
┌─────────────────────────────────────────────────┐
│ hangar daemon                                   │
│  chat bus (part 1): fleet/message_* + store     │
│  ┌──────────────────────────────────────────┐   │
│  │ copilot service (NEW)                    │   │
│  │  ACP session (claude|codex, AgentPool)   │   │
│  │  guardrail engine: auto vs confirm       │   │
│  │  activity log → receipts + events        │   │
│  └───────┬─────────────────────┬────────────┘   │
│     ACP stdio             MCP stdio             │
│  ┌────────▼─────────┐  ┌───────▼─────────────┐  │
│  │ claude-agent-acp │  │ ainb-fleet-tools    │  │
│  │ / codex-acp      │  │ (NEW MCP server)    │  │
│  └──────────────────┘  │ status/needs/send/  │  │
│                        │ answer/broadcast/   │  │
│                        │ spawn/interrupt/kill│──┼──▶ existing fleet
│                        └─────────────────────┘  │    action plumbing
└─────────────────────────────────────────────────┘    (tmux + ACP sessions)
```

Key loop: the copilot's MCP tools call BACK into the daemon's own fleet actions, so every copilot action rides the exact receipt/ordering machinery human clients use. Buzz precedent: agent posts via `buzz` CLI, not via ACP passthrough (research doc §2).

## Contract (RECONCILED 2026-07-31 against landed part-1 plan)

Confirmed from part 1 (its Wire contract + Scope/threading sections are normative):

- Part-1 v2 surface (frozen at its Phase 2 checkpoint): `fleet/acp_session_create`, `fleet/message_send`, `fleet/message_list`, `fleet/message_subscribe`, `fleet/transcript_list`, `fleet/transcript_subscribe`.
- **No `fleet/thread_list`** (part-2 draft assumption removed): threading is `origin_message_id` linkage; thread reads are `fleet/message_list {origin_id}`. All part-2 thread views use this.
- Scope grammar: `session:<key>`, `broadcast:<ulid>`; part 2 mints `channel:<id>` strings with NO schema change (part 1 open question resolved: minted strings suffice).
- Transcript = `fleet_provider_event` (`source='acp'`), consumed via `fleet/transcript_list`/`_subscribe`; inspector render classes map from `event_type='acp.<kind>'` chunk kinds.
- **ACP permission requests are part-1 machinery**: attention row + answer via existing `fleet/action` Approve/Deny/StructuredAnswer. Part 2 UIs render those as actionable rows; NO new permission method. Part-2 `fleet/confirm_*` is a DIFFERENT concept: guardrail confirm cards for copilot tool calls (daemon-suspended tool results), not ACP permissions.
- **No second protocol bump.** Part 1's v2 bump carries Provider growth + its 6 methods only ("nothing else ever rides it"). Part-2 methods are append-only additions per part 1's own pattern: consts + registry + dispatch, capability id appended to `FLEET_PROTOCOL_CAPABILITY_IDS` only when the handler lands (pre-handler -32601 is correct and unadvertised). Requirement this creates: Swift event decoding must be TOLERANT to unknown event kinds (mirror of part 1's tolerant provider decode) so part-2 event kinds are capability-only.
- Copilot channel = an ACP session whose scope IS the channel: `fleet/acp_session_create {provider, cwd, scope_key: "channel:copilot"}` (supported by part 1's supplied-scope path; idempotent per live scope gives the singleton for free).
- Part 1 has NO per-session model/mode config (static daemon adapter config by design; its "What we're NOT doing" §7 explicitly delegates per-session settings to part 2). `fleet/copilot_configure` therefore requires part 2 to add config columns + re-apply them in the resume path (task in A2).
- Steering is explicitly part-2 territory (part 1 excludes it from delivery). Part 2 extends `ainb-acp` client with `_session/steering` (always `idleBehavior: promptRequired`).

Added by part 2 (append-only, no bump):

| Method | Params (sketch) | Capability |
|---|---|---|
| `fleet/channel_create` | `{kind: copilot\|broadcast, name, recipients?}` -> mints `channel:<id>` scope | `fleet.chat.write` |
| `fleet/channel_list` | `{}` | `fleet.chat.read` |
| `fleet/copilot_configure` | `{provider: claude\|codex, model?, persona?}`; floors claude-agent-acp >= 0.64.0, codex-acp >= 1.1.7 | `fleet.copilot.configure` |
| `fleet/confirm_list` / `fleet/confirm_answer` | `{confirm_id, approve\|deny\|edit{...}}`; server-side expiry | `fleet.confirm.answer` |
| `fleet/activity_list` | `{scope_key?, cursor}` | `fleet.chat.read` |
| events: `fleet/confirm_event`, `fleet/activity_event` | new notification streams, page-to-head like part 1's message/transcript forwarders (no resync frames) | |

Copilot MCP tool surface (server: NEW crate `ainb-fleet-tools`):

| Tool | Class | Guardrail |
|---|---|---|
| `fleet_status`, `session_needs`, `session_transcript` | read | auto |
| `send_prompt(session, text)`, `answer_need(session, answer)`, `broadcast(sessions, text)` | write | auto + activity row |
| `spawn_session(cfg)` | write | confirm card |
| `interrupt(session)`, `kill(session)`, `archive(session)` | destructive | confirm card, no override to auto for kill |

## Testing strategy (locked)

| Layer | Tool | Convention source |
|---|---|---|
| proto/contract Rust | unit tests in `ainb-hangar-proto` + daemon RPC tests | existing fleet contract tests |
| Swift contract | `Tests/FleetRPCTests` incl. `FleetDaemonContractTests` + `FleetFixtureDaemon` fixtures | `apps/ainb-fleet-macos/Tests/`, CI `swift-contract-paths` workflow |
| shared fixtures | ONE fixture set (json frames) consumed by both Rust + Swift contract tests | new `ainb-tui/crates/ainb-hangar-proto/fixtures/chat/` |
| TUI | tripwire tests (`crates/ainb-core/tests/tripwire_*.rs`, per ainb-tui:tmux-ui-tripwire traps) + insta snapshots | house rules |
| ACP adapters | spike probes promoted to integration tests behind `#[ignore]` + env gate (real adapters, real creds) | spike scripts in scratchpad → `ainb-acp/tests/` |
| guardrails | pure-fn unit tests on classifier (tool name → auto/confirm) + daemon e2e with fixture MCP | |

Disclosure rule: every test comments real-adapter vs fixture, matching repo habit.

## Phase 0: Contract reconciliation gate (blocks A/B/C)

- [x] Read landed part 1 plan; diff its `fleet/message_*`/store/AgentPool surface against the draft; amend this file where they disagree (DONE 2026-07-31: thread_list removed, no second bump, permission split clarified, copilot scope via acp_session_create, per-session config delegated here, broadcast rides message_send; see Contract section)
- [ ] IMPLEMENTATION GATE: part 1 Phase 2 wire-freeze checkpoint approved before part-2 proto work starts (part-2 methods append AFTER the frozen v2 surface lands)
- [ ] Write the shared fixture set for the merged contract (`ainb-hangar-proto/fixtures/chat/*.json`): message frames, thread-by-origin sequences, confirm lifecycle, activity rows, replay sequences (shared with part 1's Swift suite per its testing strategy)
- [ ] Land part-2 proto types append-only: consts + `ALL_METHODS` tail + `declared` mirror + typed params/results for the 6 part-2 methods and 2 event streams; capability ids DEFINED here, each advertised only when its handler lands (part 1's Phase 2/3 rule)
- [ ] Swift: tolerant event-kind decoding (unknown event kind ignored, mirror of tolerant provider decode) + `FleetDaemonContractTests` cases for part-2 fixtures
- [ ] Rust proto round-trip tests green; Swift contract suite green in CI (`swift-contract-paths` workflow fires on fleet proto paths)
- [ ] Success criteria: both contract suites green from one fixture set; no version bump in any part-2 diff

## Phase A: Fleet copilot channel

### A1 daemon: `ainb-fleet-tools` MCP server (new crate)
- [ ] Tools per table above; each write tool calls the daemon's existing fleet action path (same receipts), never tmux directly
- [ ] Typed tool errors (dead session, unknown session) so the copilot can react; never free-text-only
- [ ] Pure-fn guardrail classifier + unit tests; per-tool override config (kill non-overridable)
- [ ] Keyfile hygiene for daemon token (buzz-dev-mcp pattern: env stripped, 0600 file)

### A2 daemon: copilot service
- [ ] Standing ACP session via part-1 AgentPool, created with `fleet/acp_session_create {scope_key: "channel:copilot"}` (idempotent per live scope = singleton); provider claude|codex, version floor asserted from `agentInfo`, `ainb-fleet-tools` passed via `session/new mcpServers`
- [ ] Per-session config columns migration (0076: `fleet_acp_session_config` or columns on `fleet_acp_session`): model/mode/reasoning/persona for `fleet/copilot_configure`; part 1 deliberately shipped static daemon config only and delegated this here
- [ ] Resume path integration: per-session config (when set) OVERRIDES static daemon adapter config in part 1's Phase 6 re-apply-after-load step; amend that step to read the override; codex `session/set_config_option` uses `configId` param name (spike)
- [ ] PIN permission mode explicitly at session/new; never inherit ambient (spike security flag: bypassPermissions leaked from env)
- [ ] Extend `ainb-acp` client with `_session/steering` (part-1 exclusion lifted here): always `idleBehavior: promptRequired` (spike: ghost detached turns otherwise); gate on initialize `_meta.steering.supported`
- [ ] Resume: `session/load` primary (proven both adapters); transcript re-prime fallback; resume marker row emitted to channel
- [ ] Confirm flow: tool call classified confirm → `confirm_open` event + suspended tool result; `fleet/confirm_answer` resolves; server-side expiry (10 min, logged expired) with `- [ ]` open question from spec resolved here
- [ ] Transcript: full session/update stream persisted via part-1 pipeline; channel timeline gets final message only

### A3 macOS UI
- [ ] Sidebar channels section (#copilot first entry); chat pane with timeline (`fleet/message_list` + `_subscribe`) + composer; inspector column driven by `fleet/transcript_subscribe`, rendering chunks by `event_type='acp.<kind>'` render class (message/thought/tool/plan/permission/usage)
- [ ] ACP permission requests render as actionable rows from attention events, answered via existing `fleet/action` Approve/Deny/StructuredAnswer (part-1 machinery, no new method)
- [ ] Confirm cards in-channel: approve / edit / deny + "always allow" per-tool toggle (writes override config); activity feed view
- [ ] Provider picker on copilot channel settings (claude | codex)
- [ ] Swift tests: reducer tests for new event kinds, contract tests already green from Phase 0, UI tests via `FleetUITestCase` + fixture daemon

### A4 TUI
- [ ] Chat screen: new plugin `ainb-plugin-chat` on the hangar-tui template (`ainb-plugin-hangar/src/plugin.rs:821` pattern: daemon client over `host/unix_socket_dial`, async push repaint), `captures_text` composer
- [ ] Core registration (compile-time, unavoidable): screen id const + `PLUGIN_SCREENS` + keybinding (`ainb-core/src/app/screens/mod.rs`, `builtin.rs`)
- [ ] Slash commands in composer (`/broadcast`, `/answer`, `/kill`...) mapping to the same RPC; numbered actionable rows for BOTH guardrail confirms (`fleet/confirm_answer`) and ACP permissions (attention + `fleet/action`), `[1] approve [2] edit [3] deny`
- [ ] Tripwire test per journey (open screen, send message, confirm card answer) + insta snapshots of timeline rendering

### Phase A success criteria
- [ ] "what's blocked?" e2e: copilot reads fleet, answers an ASK need automatically (activity row + receipt), proposes kill (confirm card), both UIs render the identical sequence
- [ ] Daemon SIGKILL + restart mid-conversation: conversation resumes, context intact (secret-word test promoted from spike)
- [ ] Zero unlogged copilot writes (assert: every write tool invocation has matching activity row)

## Phase B: Per-session chat threads

- [ ] Daemon: per-session threads are part 1's `session:<key>` scopes as-is (no new tables); thread reads via `fleet/message_list {scope_key}` and reply joins via `{origin_id}`; deliveries surface as receipts inline
- [ ] macOS: session detail composer becomes a thread view (transcript + receipts inline); one-shot composer removed
- [ ] TUI: session picker → thread view in chat screen
- [ ] Needs/ASK events render as actionable rows in the thread (answer inline)
- [ ] Tests: thread replay fixtures both suites; tripwire for inline answer journey
- [ ] Success: prompt a tmux session from chat thread, reply + receipt appear in-thread on both UIs

## Phase C: Broadcast channels

- [ ] Daemon: named channels = minted `channel:<id>` scopes with recipient sets (part-2 table); fan-out via part 1's `fleet/message_send {targets: [N session_keys]}` (replies land in each recipient's own scope with `origin_message_id` per part 1's R7 rules); legacy `fleet/broadcast` untouched, chat channels do NOT use it
- [ ] macOS: channel create sheet (recipients checklist reuses broadcast form), per-recipient thread columns or grouped timeline
- [ ] TUI: channel view with per-recipient thread fold
- [ ] Copilot integration: `broadcast` tool posts into a channel so results are browsable, not just receipts
- [ ] Tests: fan-out fixtures (N recipients, partial failures: Delivered|Failed|Unknown per recipient); tripwire broadcast journey
- [ ] Success: "run tests" to 3 sessions → 3 threaded replies + 3 receipts, identical on both UIs

## Cross-cutting tasks

- [ ] Capability gating: steer/broadcast-steer surfaced per-adapter capability flag (gemini future: no steering); probe fallback on -32601 is safe per spike, assert version floors
- [ ] Config: pinned adapter versions in daemon config; `fleet/copilot_configure` validation
- [ ] Docs: `docs/tui/` chat page + `docs/toolkit` update; amendment section in this file per house convention

## Risks

| Risk | Mitigation |
|---|---|
| Part 1 lands different contract | Phase 0 gate; nothing after Phase 0 starts before reconciliation commit |
| Autonomous misfire | guardrail classifier tests, destructive-only confirm, activity feed, kill non-overridable |
| Adapter drift on npm | version floors from agentInfo, pinned versions, spike probes as CI-ignorable integration tests |
| Two-surface drift | one fixture set, both contract suites in CI, shared golden transcripts |
| TUI chat ergonomics underestimated | Phase A4 scoped to copilot channel only; B/C reuse the widget |

## Open questions

- [ ] session/load with well-formed nonexistent UUID on claude-agent-acp (one probe, fold into Phase 0)
- [ ] Copilot default persona: hardcoded system prompt v1 vs early .persona.md (leaning hardcoded until part 5)
- [ ] Broadcast channel UI shape (grouped timeline vs per-recipient columns): decide with mockups in Phase C, /interview if contested
