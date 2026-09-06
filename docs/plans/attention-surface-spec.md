# Spec: Sessions screen as the one attention surface; Fleet panel deleted

**Generated from:** .agents/specs/2026-09-03-fleet-panel-attention-surface-stub.md
**Date:** 2026-09-04
**Format:** diagram-first, table-second, no prose paragraphs

## Problem

| Question | Answer |
|----------|--------|
| What? | Delete the host Fleet panel; the sessions screen becomes the single attention, answer, thread and assistant surface, with a tab strip over its right pane |
| Why? | `fleet_panel.rs` renders the same reducer as the hangar plugin's `F` tab, dies with the same daemon, and had accreted six unrelated verbs |
| Who? | The operator running many agent sessions locally, daemon up or down |

### What was actually wrong

| Finding | Evidence |
|---|---|
| Host panel is a second renderer of the hangar plugin's state machine | `fleet_panel.rs:30` imports `ainb_plugin_hangar::screen::fleet::{reduce_fleet, ...}`; `screen/router.rs:122` binds `'F' => Screen::Fleet` |
| Both banners in the empty-panel screenshot report ONE dead process | `fleet/control.rs:137` `DaemonClient::from_env().fleet_snapshot()` is the only source |
| Premise correction: ATC does not block this. It blocks the DEPRECATED `ainb fleet daemon` watcher, which the panel never talks to | `cli/registry.rs:2542-2556` |
| Five attention surfaces existed | host Fleet panel · host notifyd Inbox (`b`) · hangar Control Center (`C`) · hangar Inbox (`I`, self-declared "the ONE attention surface") · sessions-list `[?]` marker |
| Sessions list already had the marker and no way to act on it | `session_list.rs:22`, `:553`, `:586` |
| `t` was a Codex-only spawn form that predated the remote-control fix | `screen/fleet.rs:1354`; `[codex] app_server` now defaults to `desktop`, `lib.rs:496-499` |

## Users + use cases

| Persona | Goal | Primary use case |
|---------|------|------------------|
| Operator, daemon up | Answer what is blocking, without leaving the session list | ASK chip → `ask` tab → pick option → send |
| Operator, daemon down | Still see and answer tmux-backed sessions | local hooks/notifyd chips → verified tmux send |
| Operator, away | Not come back to a session wedged on a rate limit | daemon retry sweep continues, escalates at cap |
| Operator, thinking | Ask ainb about its own memory/skills/fleet, optionally act | `copilot` tab, engine + model + mode picked in-pane |

## Approach

| Option | Summary | Tradeoff | Picked? |
|--------|---------|----------|---------|
| A | Fold everything into the sessions screen; delete the panel | two new surfaces to build | ✓ |
| B | Strip the panel to an ask queue, keep the shell | keeps 2000 lines of scaffolding, chip still not answerable in place | |
| C | Sessions list only, no cross-session view | no queue when daemon down | |

**Why A:** the sessions screen is where the operator already is, already carries the attention marker, and already has the multi-select and split-pane machinery the other verbs need.

## Architecture

```
producers                        host                          sinks
┌──────────────┐
│ ainb-hooks   │──▶ notifyd sqlite ─┐
└──────────────┘                    │  local, always
┌──────────────┐                    ├──▶ ┌──────────────┐
│ hook-ingest  │──▶ hangar          │    │  Sessions    │
└──────────────┘    attention ──────┘    │  screen      │
                    (wins when up)       └──────┬───────┘
                                                │ answer routes by kind
                              ┌─────────────────┴──────────────────┐
                              ▼                                    ▼
                    session.provider == acp             ordinary tmux pane
                    daemon ACP prompt                   fleet-core verified send
                    PENDING until turn end              terminal at submit
                    (needs hangar)                      (works with hangar down)

┌──────────────────────────────────────────────────────────────┐
│ hangar daemon · retry sweep (new, no ATC instance required)   │
│  fleet needs ──▶ plan(rows, cap, atc_retry)                   │
│    ERR + budget ──▶ verified send "continue"                  │
│    ERR + at cap ──▶ raise_escalation ──▶ attention row (ERR)  │
│    ASK / IDLE / WAIT ──▶ untouched                            │
└──────────────────────────────────────────────────────────────┘
```

| Component | Purpose | Owns |
|-----------|---------|------|
| `session_list.rs` | left pane: tree, chips, checkboxes, attach digits | selection, multi-select, chip render |
| right-pane tab host (new) | switchboard over one pane rect | active tab, per-tab state |
| `ask` view (new) | structured answer sheet | option cursor, free-text buffer, send receipt |
| `thread` view | per-session chat, scope `session:<key>` | reuses `fleet_chat.rs` `ChatTopic::Session` |
| `copilot` view | generic ainb assistant + channels list | engine, model, mode, channel list |
| `log` view (new) | per-session notification history | reads notifications.db filtered by session |
| hangar daemon retry sweep (new) | LLM-free transient recovery | `atc_retry` ledger, escalation |
| `ainb-fleet-core::send` | the one verified send path | unchanged |

### Deleted

| Thing | Path |
|---|---|
| Host Fleet panel screen + renderer + `f` binding + registration | `components/fleet_panel.rs` (2177 ln), `app/events.rs` `handle_fleet_panel_keys`, `screens/builtin.rs` `FleetPanelScreen` |
| Host notifyd Inbox screen + `b` binding | `components/inbox.rs`, `InboxScreen` |
| Managed-Codex start form (`t`) | `screen/fleet.rs:1354` `FleetMode::Start` |
| New-ATC name prompt in the panel | `FleetPanelNewAtc*` events |
| ATC lite mode entirely | `SupervisorMode::Lite`, `Controller::LiteScanner`, `lite_heartbeat_id`, `ainb fleet atc supervise`, `--set lite` |
| JSON retry ledger | `heartbeat-state.json` `continue_counts`, `seal_ledger_for_handoff` |

The canonical Fleet reducer stays in `ainb-plugin-hangar` driving its own `F` tab. Nothing there is deleted.

## Data model

```
┌────────────────┐        ┌──────────────────┐
│ notifications  │        │ hangar attention │
│ (notifyd, WAL) │        │ (migration 0025) │
│  kind, ts,     │        │  kind, options,  │
│  session_id    │        │  scope, state    │
└───────┬────────┘        └────────┬─────────┘
        │ always                   │ when daemon up, wins
        └──────────┬───────────────┘
                   ▼
          ┌──────────────────┐
          │ AttentionRow     │  one merged view model
          │  session_key     │
          │  kind  ASK|APPROVE|ERR|DONE
          │  age_ms          │
          │  options[]       │  empty unless structured
          │  answerable      │  false + reason when no transport
          └──────────────────┘

┌──────────────┐        ┌──────────────┐
│ atc_instance │◀──FK───│ atc_retry    │  ← FK must relax for
└──────────────┘        │ session_key  │    ownerless sessions
                        │ count, ts    │
                        └──────────────┘
```

| Entity | Fields (key only) | Relationships |
|---|---|---|
| `AttentionRow` | session_key, kind, age_ms, options, answerable, source | 1:1 to a session row |
| `atc_retry` | instance_name (FK), session_key, count, last_seen | needs a nullable/synthetic instance for sweep-owned rows |
| copilot channel | scope_key `channel:<ulid>`, kind `copilot` | 1:1 to a live `fleet_acp_session` |

## Interface

### Left pane, chips

```
┌─ Sessions · 2 need you ──────────────────────────┐
│ ▾ agents-in-a-box                       main     │
│  1 ☐ ├─● claude ACP-chat        ASK      40s     │
│  2 ☐ ├─● codex  disk-clean      ERR       9m     │
│    ☐ └─▪ shell                                   │
│ ▾ boxtrack                              main     │
│  3 ☐ ├─● claude api-stats       APPROVE   3m     │
│  4 ☐ ├─● claude site-build      DONE      1m     │
└──────────────────────────────────────────────────┘
```

| Chip | Meaning | Counted in header |
|---|---|---|
| `ASK` | structured question waiting | yes |
| `APPROVE` | permission request waiting | yes |
| `ERR` | failed, a human must see it | no |
| `DONE` | finished | no |

Replaces `[?]` `[!]` `[✓]` (`session_list.rs:586`). Vocabulary matches the hangar Inbox four-code collapse so the two surfaces cannot drift. Header/badge split mirrors that screen deliberately: the block is "what is open", the badge is "what is blocking an agent".

### Right pane, tab strip

```
┌─ Sessions · 1 needs you ────┬ preview │ ask │ thread │ copilot │ log ┐
│ ▾ agents-in-a-box           │ Decide the sqlite path              │
│  1 ☑ ├─● claude ACP  ASK  ◀ │                                     │
│  2 ☑ ├─● codex  disk        │ ▸① data/box.db                      │
│ ▾ boxtrack                  │   repo-root data dir                │
│  3 ☐ ├─● claude api         │  ② api/src/db.sqlite                │
│                             │  ③ other (type it)                  │
└─────────────────────────────┴─────────────────────────────────────┘
 ↑↓ row · ⇥ tab · enter send · B broadcast to 2 · q back
```

| Tab | Enabled when | Content |
|---|---|---|
| `preview` | always | tmux mirror, today's default |
| `ask` | selected row has ASK or APPROVE | option list or free-text composer |
| `thread` | a session row is selected | `session:<key>` chat |
| `copilot` | always | assistant + engine/model/mode + channel list |
| `log` | a session row is selected | that session's notification history |

Disabled tabs render dimmed, never hidden, so the strip does not reflow as state changes.

### Copilot pane header

```
┌─ copilot ──────────────────────────────────┐
│ engine  claude-agent-acp        ◀ e cycle  │
│ model   sonnet-5                ◀ o        │
│ mode    guarded                 ◀ g        │
│ channels  release · qa · ops               │
│────────────────────────────────────────────│
│ you  what did we decide about sqlite?      │
│ cop  memory: repo-root data/               │
└────────────────────────────────────────────┘
```

| Setting | Values | Storage |
|---|---|---|
| engine | any adapter in the daemon's registry | `fleet_acp_session.provider` |
| model, reasoning | adapter `config_options` | `FleetAcpSessionConfig` |
| mode | `help` · `guarded` · `yolo` | per-channel |
| persona | free text | already exists in `copilot_configure` |

### Broadcast

Acts on the left pane's existing checkboxes (`state.selected_sessions`, `ballot_checkbox`). The old recipient picker is deleted. Named channels stay a list inside the `copilot` tab.

### Elsewhere

| Verb | New home |
|---|---|
| new-ATC | Daemons screen (`d`), per the existing "every ainb daemon lives there" rule |
| Codex remote-control | already the default, `[codex] app_server = desktop`; no UI |
| roster, fleet-wide operator views | hangar plugin `F` tab, unchanged |

## Behavior

### Happy path, structured answer

```
[chip ASK] ──⇥ ask──▶ [options shown] ──j/k──▶ [option picked]
                                                    │ enter
                                                    ▼
                                          [chip SENT ⠋, in-flight]
                                                    │
                          ┌─────────────────────────┴──────────┐
                          ▼                                    ▼
                    [chip clears]                     [chip back to ASK]
                     answer landed                     + failure reason
```

### Copilot mode gate

```
copilot proposes a write
        │
        ├─ mode help    ──▶ refused, no write tools in the table
        ├─ mode guarded ──▶ confirm card, y/n, then it fires
        └─ mode yolo    ──▶ fires immediately, red banner on the pane
```

The dial moves the daemon-side copilot guardrail ONLY (`copilot.rs:201`). Adapter `permission_mode` stays pinned at `session/new` and re-asserted after `session/load`; `copilot_configure` keeps rejecting it (`rpc/mod.rs:2871`). Loosening that per session is the documented ambient-bypass bug.

### Retry sweep

```
[session ERR] ──transient pattern──▶ [count < cap] ──▶ send "continue"
                     │                      │
                     │                      └─ count++ in atc_retry
                     │
                     └──off ERR roster for RETRY_RESET_GRACE_MS──▶ [budget restored]

[count == cap] ──▶ raise_escalation ──▶ attention row ──▶ ERR chip ×N
```

Transient classes unchanged, from `fleet-core/read/errors.rs`: `rate_limited` · `overloaded_error` · `internal_server_error` · `request timed out` · `socket hang up` · `API Error`/`fetch failed` · `ECONNRESET`.

### Edge cases

| Scenario | Trigger | Expected behavior |
|---|---|---|
| ACP-backed row, hangar down | no daemon transport and no pane to type into | chip renders greyed, `ask` tab states which call is unavailable; never a silent no-op |
| Enter is ambiguous | Enter attaches today; on `ask` it must send | Enter is scoped to the ACTIVE TAB: preview→attach, ask→send, thread/copilot→send message, log→no-op. Attach digits keep working from any tab |
| Two open states on one row | ASK arrives while ERR is open | both chips shown, ASK first; header counts the ASK only |
| Answer send fails | dead pane, stale tmux identity | chip returns to ASK with the failure reason; never renders as answered |
| Producers disagree | hangar says answered, notifyd row still open | daemon row wins while the daemon is up; on reconnect the local row is reconciled, not merged |
| Copilot channel absent | first `copilot` tab open on a fresh install | pane shows the create step explicitly, not an empty composer (this is symptom 1 today) |
| ACP turn never ends | leg stays PENDING | pane shows PENDING with elapsed time and the pool's turn deadline, and says a cancel is available (this is symptom 2 today) |
| Session vanishes mid-answer | pane killed between render and send | `target_not_running`, chip clears, row drops on next refresh |
| Retry sweep vs a live ATC | both would type into one pane | ATC-owned sessions are excluded from the sweep; ownership checked every tick |
| 80-column terminal | five tab labels plus chip words | tab labels truncate before the pane content; chip words never abbreviate |

## Errors

| Failure mode | User-visible surface | Recovery |
|---|---|---|
| Hangar daemon down | one banner line on the sessions header, not two; chips still render from local producers | tmux answers keep working; ACP rows greyed with reason |
| `channel_list` / `acp_session_create` fails | copilot pane names the failed call | retry key on the pane |
| ACP leg never resolves | PENDING with elapsed time and deadline | cancel offered; deadline converges the turn |
| Verified send fails | chip reverts, reason on the row | `r` retries the same answer under the same request id |
| Retry sweep exhausts budget | ERR chip with `×N` | operator continues manually with `c`, which resets nothing |
| Adapter in config fails to spawn | copilot pane keeps the old engine and says the swap was refused | pick another engine |

## Testing strategy

| Layer | Scope | Coverage gate |
|---|---|---|
| Tripwire (tmux e2e) | ASK appears → answer from `ask` tab → chip clears, with the daemon STOPPED and again with it running; unseeded, per the house rule | must-pass |
| Tripwire | copilot pane reaches a live composer from a cold install; engine swap lands on the same channel | must-pass |
| Tripwire | retry sweep continues a seeded transient ERR and escalates at cap with zero ATC instances provisioned | must-pass |
| Integration | producer precedence: local row, then daemon row wins, then daemon drops and local resumes | must-pass |
| Integration | answer routing by session kind, including ACP + no daemon → not answerable | must-pass |
| Unit | chip precedence and header count; tab enablement; Enter scoping per tab | where non-trivial |
| Insta | chip strip at 80 and 100 columns, both themes | must-pass |
| Parity | every verb the old panel had is either rehomed and exercised, or explicitly listed as deleted | must-pass |

## Out of scope

- The hangar plugin's `F`, `I` and `C` tabs. Untouched.
- ATC full mode. Keeps its LLM heartbeat, its cron and its shared ledger.
- Rebuilding the ACP client or the chat wire protocol. Only the surface and its failure reporting change.
- A web or macOS client equivalent of the tab strip.
- Any new spawn path. `ainb run` and the new-session flow stay as they are.

## Open questions for /plan

- [ ] `f` and `b` are freed. Rebind, or leave unbound for a release?
- [ ] `atc_retry.instance_name` foreign-keys `atc_instance`. Relax the FK, or write sweep rows against a synthetic default instance?
- [ ] Sweep cadence: lite ran every 5s. Keep 5s, or align to the daemon's existing reconcile tick?
- [ ] Retry cap for sweep-owned sessions: reuse `DEFAULT_ERR_RETRY_CAP`, or a separate daemon config key?
- [ ] Which read tools does `help` mode actually get: memory, skills, repo, docs? Each one widens the injection surface the classifier must cover.
- [ ] Does `yolo` persist across restarts, or reset to `guarded` on daemon start?
- [ ] Adapter registry file: new `adapters.toml`, or a section in the existing hangar `config.toml`?
- [ ] Is the `log` tab's per-session scoping enough, or does anything still need the cross-session notification history that `b` provided?
