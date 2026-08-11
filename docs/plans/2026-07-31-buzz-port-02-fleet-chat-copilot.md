---
title: "Buzz port part 2: fleet chat + copilot"
---

# Plan: Fleet Chat + Fleet Copilot, both surfaces (buzz-port part 2)

**From spec:** docs/plans/2026-07-31-buzz-port-02-fleet-chat-copilot-spec.md
**Research:** [research, discussion #570](https://github.com/stevengonsalvez/agents-in-a-box/discussions/570) · [spike report, discussion #570 comment](https://github.com/stevengonsalvez/agents-in-a-box/discussions/570#discussioncomment-17880848)
**Explainer:** https://explainers.stevengonsalvez.com/buzz-acp-port/ (committed copy: explainers/buzz-acp-port-research.html)
**Depends on:** docs/plans/2026-07-31-buzz-port-01-chat-bus-acp.md (part 1: chat bus + ainb-acp AgentPool; generating at plan time, see Phase 0 gate)
**Date:** 2026-07-31
**Amended:** 2026-08-04 (distinguished-engineer review, applied to both parts; amendments marked `(DE review 2026-08-04)`)
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

The same loop is the threat model. The copilot reads agent-authored text (transcripts, needs, message bodies) and then acts with fleet-control tools. See Trust boundary below (DE review 2026-08-04).

## Contract (RECONCILED 2026-07-31 against landed part-1 plan; re-reconciled 2026-08-04 after DE review of part 1)

Confirmed from part 1 (its Wire contract, Session identity and Scope/threading sections are normative):

- Part-1 v2 surface (frozen at its Phase 2 checkpoint): `fleet/acp_session_create`, `fleet/message_send`, `fleet/message_list`, `fleet/message_subscribe`, `fleet/transcript_list`, `fleet/transcript_subscribe`.
- **No `fleet/thread_list`** (part-2 draft assumption removed): threading is `origin_message_id` linkage; thread reads are `fleet/message_list {origin_id}`. All part-2 thread views use this.
- Scope grammar: `session:<key>`, `broadcast:<ulid>`; part 2 mints `channel:<id>` strings with NO schema change (part 1 open question resolved: minted strings suffice).
- **Cursors are `seq`, identities are `id`** (DE review 2026-08-04, part 1 graft 9). Part 1's message stream is paged by a SQLite-assigned commit-ordered `seq`; the ULID `id` is stable external identity and is never an ordering key. Every part-2 stream that pages (`fleet/confirm_event`, `fleet/activity_event`, `fleet/activity_list`) MUST copy that shape and MUST NOT introduce a client-minted or wall-clock-derived cursor.
- **ACP sessions carry BOTH a `fleet_session` row and a `fleet_acp_session` row under one `session_key`** (DE review 2026-08-04, part 1 Session identity). Part-2 UIs therefore address an ACP session exactly like a tmux one: it appears in the fleet snapshot, it has a `capabilities` JSON that gates which actions the UI may offer, and it answers `fleet/action`.
- Transcript = `fleet_provider_event` (`source='acp'`), consumed via `fleet/transcript_list`/`_subscribe`; inspector render classes map from `event_type='acp.<kind>'` chunk kinds.
- **ACP permission requests are part-1 machinery**: attention row + answer via existing `fleet/action` Approve/Deny/StructuredAnswer. Part 2 UIs render those as actionable rows; NO new permission method. Part-2 `fleet/confirm_*` is a DIFFERENT concept: guardrail confirm cards for copilot tool calls (daemon-suspended tool results), not ACP permissions.
- **The ACP arms of `fleet/action` are part-1 Phase 5 work and part 2 depends on them landing** (DE review 2026-08-04). They did not exist in the original part-1 draft: today a non-claude provider's Approve/Deny/StructuredAnswer falls to `(Unknown, "authoritative provider request transport is not active")` at `rpc/mod.rs:1614-1619`, and Interrupt/Stop/Kill fall to a sibling Unknown at `:1620-1623`. Part 2's "answer the ACP permission from the UI" and the `interrupt`/`kill` MCP tools are both blocked on those arms.
- **No second protocol bump.** Part 1's v2 bump carries Provider growth + its 6 methods only ("nothing else ever rides it"). Part-2 methods are append-only additions per part 1's own pattern: consts + registry + dispatch, capability id appended to `FLEET_PROTOCOL_CAPABILITY_IDS` only when the handler lands (pre-handler -32601 is correct and unadvertised). Requirement this creates: Swift event decoding must be TOLERANT to unknown event kinds (mirror of part 1's tolerant provider decode) so part-2 event kinds are capability-only.
- Copilot channel = an ACP session whose scope IS the channel: `fleet/acp_session_create {provider, cwd, scope_key: "channel:copilot"}` (supported by part 1's supplied-scope path; idempotent per live scope gives the singleton for free).
- Part 1 has NO per-session model/mode config (static daemon adapter config by design; its "What we're NOT doing" explicitly delegates per-session settings to part 2). `fleet/copilot_configure` therefore requires part 2 to add config columns + re-apply them in the resume path (task in A2).
- **The per-session config override may NOT touch `permission_mode`** (DE review 2026-08-04). Part 1's I13 makes the pinned permission mode an asserted invariant precisely because the spike showed an ambient `bypassPermissions` silently disables the entire permission surface. A `copilot_configure` that could set the mode would be a remote switch for turning the guardrails off, reachable by anyone holding `fleet.copilot.configure`. Config override covers model, reasoning effort and persona only; the mode stays daemon config.
- Steering is explicitly part-2 territory (part 1 excludes it from delivery). Part 2 extends `ainb-acp` client with `_session/steering` (always `idleBehavior: promptRequired`).

Added by part 2 (append-only, no bump):

| Method | Params (sketch) | Capability |
|---|---|---|
| `fleet/channel_create` | `{kind: copilot\|broadcast, name, recipients?}` -> mints `channel:<id>` scope | `fleet.chat.write` |
| `fleet/channel_list` | `{}` | `fleet.chat.read` |
| `fleet/copilot_configure` | `{provider: claude\|codex, model?, persona?}`; floors claude-agent-acp >= 0.64.0, codex-acp >= 1.1.7; MUST NOT accept a permission mode (see above) | `fleet.copilot.configure` |
| `fleet/confirm_list` / `fleet/confirm_answer` | `{confirm_id, approve\|deny\|edit{...}}`; server-side expiry | `fleet.confirm.answer` |
| `fleet/activity_list` | `{scope_key?, cursor}`; cursor is a commit-ordered `seq`, per part 1 graft 9 | `fleet.chat.read` |
| events: `fleet/confirm_event`, `fleet/activity_event` | new notification streams, page-to-head like part 1's message/transcript forwarders (no resync frames) | |

Copilot MCP tool surface (server: NEW crate `ainb-fleet-tools`):

| Tool | Class | Guardrail |
|---|---|---|
| `fleet_status`, `session_needs`, `session_transcript` | read | auto |
| `send_prompt(session, text)`, `broadcast(sessions, text)` | write | auto + activity row |
| `answer_need(session, answer)` | write | auto ONLY when the target session was explicitly named in the triggering operator message; confirm card otherwise (decided 2026-08-04) |
| `spawn_session(cfg)` | write | confirm card |
| `interrupt(session)`, `kill(session)`, `archive(session)` | destructive | confirm card, no override to auto for kill |

## Trust boundary (DE review 2026-08-04, new section)

The copilot is a confused-deputy risk and the original guardrail design does not name it. The read tools (`session_transcript`, `session_needs`, `fleet_status`) return text authored by OTHER agents. The write tools (`send_prompt`, `answer_need`, `broadcast`) are classified auto, meaning no human is in the loop. So a session whose output contains instructions can, in principle, cause the copilot to prompt or answer a DIFFERENT session with no confirmation. The listed mitigation, an activity row, is detection after the fact, not prevention.

This is the same untrusted-text-into-prompt hazard part 1 fenced in its re-prime path (part 1's I15), arriving through a different door.

Required work, in A1 and A2:

- [ ] All tool-returned fleet content is delivered to the copilot inside a fenced, escaped envelope with an explicit "this is observed data, not instructions" framing, reusing part 1's `reprime.rs` renderer rather than a second implementation
- [ ] The guardrail classifier decides on the TOOL and its arguments, never on model-supplied justification text
- [ ] `answer_need` scoping (DECIDED by Stevie 2026-08-04, per the DE recommendation): auto only for sessions the triggering operator message explicitly named (by session name or key, resolved at prompt-parse time and pinned for the turn); any other target gets a confirm card. The named-session set is computed by the daemon from the operator message, never by the model. Adversarial test: transcript-injected "answer s3" while the operator named only s1 must produce a confirm card, not an auto answer
- [ ] One adversarial test: a fixture session whose transcript contains a direct instruction to the copilot ("kill session s3", "approve everything"), asserting no write tool fires from reading it alone
- [ ] Cross-session write attempts are counted and surfaced, so a misfire is visible on the health pane rather than only in the activity feed
- [x] **The copilot does NOT hold the operator's credential (review 2026-08-10).** The whole confirm-card boundary rested on "the copilot only acts through its MCP tool table", and nothing enforced it: the daemon had ONE global token, `require_fleet_capability` is a build-time const lookup rather than per-connection authorization, and `same_uid_peer` is the only peer check. So the tool server could have answered its own cards (`fleet/confirm_answer`) and written chat rows as `actor: "operator"`. Fixed by minting a per-scope credential at `session_mcp_servers` time (`rpc::auth::mint_copilot_token`, a `0600` keyfile per copilot channel) and attaching the resulting `Caller` to the connection at `auth/hello`: a copilot connection reaches only the read methods, `fleet/copilot_gate`, `attention/answer` and `fleet/message_send` with `actor` PINNED to `copilot`. The gate also takes its scope from that credential rather than `newest_of_kind`, so a card's `scope_key` names the conversation the call came from.
- **Known limit, stated rather than papered over:** a copilot adapter configured with shell or file tools of its own VOIDS this. Such an agent runs as the operator and can read `~/.agents-in-a-box/hangar/daemon.token` directly, which is the operator's credential. The guardrail assumes the copilot's only reach into the fleet is the tool table the daemon attached.

## Testing strategy (locked)

| Layer | Tool | Convention source |
|---|---|---|
| proto/contract Rust | unit tests in `ainb-hangar-proto` + daemon RPC tests | existing fleet contract tests |
| Swift contract | `Tests/FleetRPCTests` incl. `FleetDaemonContractTests` + `FleetFixtureDaemon` fixtures | `apps/ainb-fleet-macos/Tests/`, CI `swift-contract-paths` workflow |
| shared fixtures | ONE fixture set (json frames) consumed by both Rust + Swift contract tests | new `ainb-tui/crates/ainb-hangar-proto/fixtures/chat/` |
| TUI | tripwire tests (`crates/ainb-core/tests/tripwire_*.rs`, per ainb-tui:tmux-ui-tripwire traps) + insta snapshots | house rules |
| ACP adapters | spike probes promoted to integration tests behind `#[ignore]` + env gate (real adapters, real creds) | spike scripts in scratchpad → `ainb-acp/tests/` |
| guardrails | pure-fn unit tests on classifier (tool name → auto/confirm) + daemon e2e with fixture MCP + the adversarial injection case from Trust boundary | |

Disclosure rule: every test comments real-adapter vs fixture, matching repo habit.

### The operating-surface rule (added 2026-08-08, learned the hard way)

Part 1 shipped a daemon and a CLI and called itself end-to-end proven. It was
not: nothing ever opened the TUI, and the panel's ACP handling was one
unmapped-token line away from degrading to `unknown` in silence. Stevie caught
it by asking whether only the CLI had been tested.

So for part 2, a phase is NOT done when its daemon methods and CLI verbs are
green. Each phase ships proof on the surface an operator actually uses:

- [ ] TUI: live tmux tripwires driving the REAL `ainb tui` binary, per journey,
      not unit renders of the widget. Open the chat tab, send a message, watch a
      reply arrive, answer a permission, see the transcript stream.
- [ ] macOS: the Swift UI test path (`FleetUITestCase` + fixture daemon), same
      journeys, so the two clients cannot drift apart in what they prove.
- [ ] Recordings: full uncut vhs tapes per journey with frames EXTRACTED AND
      READ, and the exact on-screen assertion text recorded in an
      `EXPECTED-OUTCOMES.md`. A file existing is not evidence; nine zero-byte
      GIFs reached main in part 1 before a CI gate was added for it.
- [ ] The claim in any writeup names the surface it was proven on. "End to end"
      without naming the surface is how part 1's gap survived four reviews.

### Two pool invariants part 2 inherits (added 2026-08-08)

The peer review of part 1 hardened the multiplexed pool in two ways that are now
contracts, not implementation details. Part 2 attaches sessions and issues turns,
so it can break both without any test going red.

- **Every attach goes through `make_room` and holds its guard for the whole
  attach.** Occupancy counts sessions still attaching, not just those holding a
  route, which is what stops two racing arrivals from overshooting the cap. A
  second attach path that skips the reservation makes the reservation meaningless.
- **Nothing goes between the replay drain and the prompt in `start_turn`.** The
  suppression seam closes there deliberately, as late as possible, because the
  supervisor forwards adapter notifications on a different task. Code inserted
  between those two lines reopens the window where replayed history is ingested
  as live output, and no test will catch it: the fixture's timing is what makes
  the current test bite.

## Phase 0: Contract reconciliation gate (blocks A/B/C)

- [x] Read landed part 1 plan; diff its `fleet/message_*`/store/AgentPool surface against the draft; amend this file where they disagree (DONE 2026-07-31: thread_list removed, no second bump, permission split clarified, copilot scope via acp_session_create, per-session config delegated here, broadcast rides message_send; see Contract section)
- [x] Re-reconcile against part 1's DE-review amendments (DONE 2026-08-04: seq-vs-id cursor rule, dual session rows, fleet/action ACP arms as a hard dependency, permission mode excluded from config override, retention/observability obligations inherited; see Contract section)
- [ ] IMPLEMENTATION GATE: part 1 Phase 2 wire-freeze checkpoint approved before part-2 proto work starts (part-2 methods append AFTER the frozen v2 surface lands)
- [ ] IMPLEMENTATION GATE: part 1 Phase 5 landed, specifically its ACP arms on `fleet/action`. Phase A's `interrupt`/`kill` tools and both UIs' permission-answer rows are dead without them (DE review 2026-08-04)
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
- [ ] Classifier decides on tool identity and arguments only, never on model-supplied justification (DE review 2026-08-04, Trust boundary)
- [ ] Read tools return fleet content inside the fenced, escaped envelope from part 1's `reprime.rs`, framed as observed data (DE review 2026-08-04, Trust boundary)
- [ ] Keyfile hygiene for daemon token (buzz-dev-mcp pattern: env stripped, 0600 file); the MCP child gets the same ALLOWLISTED environment part 1's adapter children get (part 1 I13), not the daemon's inherited env (DE review 2026-08-04)
- [ ] Adversarial injection test: a fixture transcript instructing the copilot to kill or approve produces no write-tool invocation on its own

### A2 daemon: copilot service
- [ ] Standing ACP session via part-1 AgentPool, created with `fleet/acp_session_create {scope_key: "channel:copilot"}` (idempotent per live scope = singleton); provider claude|codex, version floor asserted from `agentInfo` and persisted to `fleet_acp_session.provider_version` (part 1 schema), `ainb-fleet-tools` passed via `session/new mcpServers`
- [ ] Per-session config columns migration (next free number at implementation time; chat bus landed as 0079: `fleet_acp_session_config` or columns on `fleet_acp_session`): model/reasoning/persona for `fleet/copilot_configure`; part 1 deliberately shipped static daemon config only and delegated this here. `permission_mode` is NOT among them (DE review 2026-08-04, Contract section)
- [ ] Persona is a privileged field: it is a system prompt for an agent holding destructive tools. Gate it behind `fleet.copilot.configure`, log every change to the activity feed, and bound its length (DE review 2026-08-04)
- [ ] Resume path integration: per-session config (when set) OVERRIDES static daemon adapter config in part 1's Phase 6 re-apply-after-load step; amend that step to read the override; codex `session/set_config_option` uses `configId` param name (spike). The permission-mode re-assertion in that same step (part 1 I13) is NOT overridable and its failure still fails the spawn
- [ ] PIN permission mode explicitly at session/new; never inherit ambient (spike security flag: bypassPermissions leaked from env). This is part 1's I13 and part 2 inherits the assertion rather than re-implementing it
- [ ] Extend `ainb-acp` client with `_session/steering` (part-1 exclusion lifted here): always `idleBehavior: promptRequired` (spike: ghost detached turns otherwise); gate on initialize `_meta.steering.supported`
- [ ] Resume: `session/load` primary (proven both adapters); transcript re-prime fallback; resume marker row emitted to channel
- [ ] Confirm flow: tool call classified confirm → `confirm_open` event + suspended tool result; `fleet/confirm_answer` resolves; server-side expiry (10 min, logged expired) with the spec's open question resolved here
- [ ] State the confirm/turn interaction explicitly (DE review 2026-08-04): a suspended tool result holds the copilot's ACP turn open for up to the expiry window, which also holds that scope's part-1 FIFO queue. The expiry MUST be shorter than part 1's per-turn deadline (default 30 min) or the deadline converges the turn out from under a pending confirm card. Test both orderings: confirm answered before expiry, and confirm left to expire
- [ ] Confirm cards are single-use and idempotent: answering an already-answered or already-expired `confirm_id` returns a typed error, never a second execution (DE review 2026-08-04)
- [ ] Transcript: full session/update stream persisted via part-1 pipeline; channel timeline gets final message only
- [ ] Activity log growth: `fleet_activity` rows are append-only per copilot action and inherit part 1's Retention and growth discipline (header-doc growth contract + revisit trigger). Do not add a table with no stated growth story (DE review 2026-08-04)
- [ ] Observability: copilot fields on `hangar/daemon_health` (session live yes/no, resume path taken, open confirm count, oldest open confirm age, tool invocations by class) and a `copilot.tool` span per invocation carrying tool, class, target session and outcome. "Why is the copilot stuck" is answered by part 1's pool fields plus the open-confirm age here (DE review 2026-08-04)

### A3 macOS UI
- [ ] Sidebar channels section (#copilot first entry); chat pane with timeline (`fleet/message_list` + `_subscribe`) + composer; inspector column driven by `fleet/transcript_subscribe`, rendering chunks by `event_type='acp.<kind>'` render class (message/thought/tool/plan/permission/usage)
- [ ] ACP permission requests render as actionable rows from attention events, answered via existing `fleet/action` Approve/Deny/StructuredAnswer (part-1 machinery, no new method), and the row is offered only when the session's `capabilities` JSON enables that action, since `action_capability` rejects it otherwise (DE review 2026-08-04)
- [ ] Confirm cards in-channel: approve / edit / deny + "always allow" per-tool toggle (writes override config); activity feed view. The toggle must not be offerable for `kill` (non-overridable per the tool table)
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
- [ ] Adversarial (restated 2026-08-10, the old wording overclaimed): a session transcript containing instructions to the copilot produces no DESTRUCTIVE call without a human, and any write it does produce is attributed to `copilot` and lands on the activity feed. `send_prompt` and `broadcast` are auto-class, so "no write tool invocation" was never the guarantee the design holds (DE review 2026-08-04, Trust boundary)
- [ ] Confirm expiry: an unanswered card expires, is logged expired, the tool result resolves as denied, and the copilot's turn ends cleanly without tripping part 1's turn deadline

## Phase B: Per-session chat threads

- [ ] Daemon: per-session threads are part 1's `session:<key>` scopes as-is (no new tables); thread reads via `fleet/message_list {scope_key}` and reply joins via `{origin_id}`; deliveries surface as receipts inline
- [ ] macOS: session detail composer becomes a thread view (transcript + receipts inline); one-shot composer removed
- [ ] TUI: session picker → thread view in chat screen
- [ ] Needs/ASK events render as actionable rows in the thread (answer inline)
- [ ] Receipt details render the enumerated reason codes part 1 defines (`target_unknown`, `queue_full`, `breaker_open`, `turn_deadline`, ...), not raw free text, so "why did this not deliver" is answerable in the UI (DE review 2026-08-04)
- [ ] Tests: thread replay fixtures both suites; tripwire for inline answer journey
- [ ] Success: prompt a tmux session from chat thread, reply + receipt appear in-thread on both UIs

## Phase C: Broadcast channels

- [ ] Daemon: named channels = minted `channel:<id>` scopes with recipient sets (part-2 table); fan-out via part 1's `fleet/message_send {targets: [N session_keys]}` (replies land in each recipient's own scope with `origin_message_id` per part 1's R7 rules); legacy `fleet/broadcast` untouched, chat channels do NOT use it
- [ ] Bound N: a channel's recipient set has a stated maximum, and a fan-out is one `message_send` with N delivery legs, not N sends. Part 1's per-scope queues are bounded, so a fan-out to more sessions than the pool can serve produces REJECTED legs with `queue_full` rather than unbounded queueing (DE review 2026-08-04)
- [ ] macOS: channel create sheet (recipients checklist reuses broadcast form), per-recipient thread columns or grouped timeline
- [ ] TUI: channel view with per-recipient thread fold
- [ ] Copilot integration: `broadcast` tool posts into a channel so results are browsable, not just receipts
- [ ] Tests: fan-out fixtures (N recipients, partial failures: Delivered|Failed|Unknown per recipient); tripwire broadcast journey
- [ ] Success: "run tests" to 3 sessions → 3 threaded replies + 3 receipts, identical on both UIs

## Cross-cutting tasks

- [ ] CLI parity (part 1's CLI surface rule applies here too): every part-2 method ships its CLI verb with its dispatch arms. `ainb fleet channel create|list`, `ainb fleet confirm list|answer <id> --approve|--deny`, `ainb fleet activity [--scope]`, `ainb fleet copilot configure --provider [--model] [--persona-file]`; same JSON/exit-code contract, `docs/tui/cli.md` updated per phase

- [ ] Capability gating: steer/broadcast-steer surfaced per-adapter capability flag (gemini future: no steering); probe fallback on -32601 is safe per spike, assert version floors
- [ ] Config: pinned adapter versions in daemon config; `fleet/copilot_configure` validation
- [ ] Docs: `docs/tui/` chat page + `docs/toolkit` update; amendment section in this file per house convention
- [ ] Migration 0076 inherits part 1's Rollback and rollout rule: forward-only, no in-place downgrade, back-out is a database file restore, stated in the landing PR (DE review 2026-08-04)

## Risks

| Risk | Mitigation |
|---|---|
| Part 1 lands different contract | Phase 0 gate; nothing after Phase 0 starts before reconciliation commit. Re-run at every part-1 amendment, as on 2026-08-04 |
| Autonomous misfire | guardrail classifier tests, destructive-only confirm, activity feed, kill non-overridable |
| Indirect prompt injection through fleet content | Trust boundary section: fenced envelope on every read tool, classifier blind to model justification, adversarial test, cross-session write counter. The activity row is detection, not prevention, and was the only listed control before this review (DE review 2026-08-04) |
| Config override becomes a guardrail off-switch | `permission_mode` excluded from `copilot_configure`; part 1's I13 assertion is not overridable (DE review 2026-08-04) |
| Confirm card holds a turn and a queue open | expiry strictly shorter than part 1's turn deadline, both orderings tested (DE review 2026-08-04) |
| Adapter drift on npm | version floors from agentInfo, pinned versions, spike probes as CI-ignorable integration tests, `provider_version` persisted per session |
| Two-surface drift | one fixture set, both contract suites in CI, shared golden transcripts |
| TUI chat ergonomics underestimated | Phase A4 scoped to copilot channel only; B/C reuse the widget |

## Open questions

- [ ] session/load with well-formed nonexistent UUID on claude-agent-acp (one probe, fold into Phase 0)
- [ ] Copilot default persona: hardcoded system prompt v1 vs early .persona.md (leaning hardcoded until part 5)
- [ ] Broadcast channel UI shape (grouped timeline vs per-recipient columns): decide with mockups in Phase C, /interview if contested
- [x] **`answer_need` auto class: RESOLVED 2026-08-04.** Stevie accepted the DE recommendation: auto only for sessions the operator's triggering message explicitly named, confirm card otherwise. Guardrail table and Trust boundary amended; the headline flow ("answer s1's ASK") stays zero-click while the fleet-wide blast radius is closed. Part 1's multiplexed-pool decision (see its resolved questions) does not change this rule.
