# Attention surface — progress log

Contract: `docs/plans/attention-surface-spec.md`. Run protocol:
`docs/plans/attention-surface-goal.md`.

## Grounding corrections to the goal's "current state"

The goal doc was grounded against a different tree. Verified 2026-09-04 on
`f/attention-surface`:

| Goal claim | Actual |
|---|---|
| `fleet_panel.rs` is 2177 lines | 2159 lines, 9 `ainb_plugin_hangar` references — claim holds in substance |
| hangar `screen/inbox.rs` is 1652 lines, "the ONE attention surface" | 324 lines, a read-only `hangar/inbox_list` renderer. It is the daemon's issue/comment inbox, not the notification attention queue |
| `SupervisorMode::Lite` / `Controller::LiteScanner` appear 16x in `fleet/atc/supervisor.rs` | **Neither symbol nor that file exists anywhere in the workspace.** ATC lite mode was already removed. Criterion 1's `rg`-zero gate for these two is already satisfied |
| `ainb fleet atc supervise`, `--set lite` | do not exist |
| `heartbeat-state.json` `continue_counts` | exists (`fleet/atc/heartbeat.rs`), still the JSON ledger |
| `screen/fleet.rs:1354` `FleetMode::Start` (`t`) | exists, now at `fleet.rs:1340` |
| host `t` key | `GoToAbtop` on HomeScreen — NOT the Codex start form, and NOT deleted |
| `atc_retry` table | exists since migration `0028`, `instance_name TEXT NOT NULL REFERENCES atc_instance(name)` |

Consequence: phase 9's deletion half is mostly already done; its real work is
the daemon retry sweep. Recorded so the final report does not claim a deletion
it did not perform.

## Open-question decisions

All eight resolved here per the run protocol's autonomy envelope.

### Q1 — `f` and `b` are freed. Rebind, or leave unbound?

**Leave unbound, and delete their HomeScreen tiles.**

Muscle memory is the whole risk. An operator who presses `f` today expects the
Fleet panel; rebinding `f` to a different verb means their reflex fires
something they did not ask for. Unbound plus a tile that no longer advertises
`[f]` is the honest signal that the surface moved. Rebinding waits a release,
by which time the reflex is gone.

### Q2 — `atc_retry.instance_name` FK: relax it, or a synthetic instance?

**Synthetic reserved instance, registered idempotently at daemon boot.**

SQLite cannot drop a foreign key in place; relaxing it means rebuilding
`atc_retry` and copying every real ATC's ledger, which risks live escalation
history for a cosmetic constraint change. A reserved instance keeps the FK,
keeps `retry_get` / `retry_list` / `record_continue` / `mark_escalated` /
`reset_retry` byte-identical (zero repo churn), and gives the sweep its own
ledger namespace so its counts can never collide with a real ATC's counts for
the same session.

### Q3 — Sweep cadence: keep lite's 5s, or align to a daemon tick?

**30s, its own interval, with an env override for tests.**

5s was right for lite because lite was a foreground scanner with nothing else
to do; at 5s the sweep re-reads the whole ERR roster twelve times a minute for
a condition that moves on the scale of an API backoff. The general sweeper's
60s is too slow — a rate-limited agent then idles a full minute before the
`continue` lands. 30s matches the daemon's existing presence pass, is a 6x
reduction in wake rate against lite, and keeps worst-case idle inside the
transient-error window. The env override exists so a tripwire can drive the
sweep in about a second instead of waiting 30.

### Q4 — Retry cap: reuse `DEFAULT_ERR_RETRY_CAP`, or a new config key?

**Reuse `DEFAULT_ERR_RETRY_CAP`. No new key.**

The sweep and the ATC heartbeat must escalate at the same point. Two caps that
can drift is precisely the split-brain this epic exists to delete. The constant
is one line if it ever needs to move.

### Q5 — Which read tools does `help` mode get?

**Exactly `ainb_fleet_tools::guardrail::READ_TOOLS`: `fleet_status`,
`session_needs`, `session_transcript`. Nothing else.**

These three are the entire read surface that exists today, they are already in
the classifier's table, and they are already covered by the injection audit
(`model_supplied_justification_cannot_move_a_verdict`). Inventing memory /
skills / repo / docs read tools to answer this question would widen the
injection surface AND add a tool surface the spec never asked for. `guarded`
adds the write tools behind confirm cards; `yolo` promotes the confirm-class
tools via `with_auto_overrides`, which already drops `kill` because `kill` is
`NEVER_OVERRIDABLE` — so `kill` stays off the automatic path in every mode,
including yolo.

### Q6 — Does `yolo` persist across restarts?

**No. Every channel's stored mode is normalised yolo to guarded at daemon
start.** `help` and `guarded` persist untouched.

A dangerous mode that survives a restart is a mode the operator forgets is on.
The restart is the natural revocation point, and the pane's red banner makes
re-arming it a one-keystroke, visible act.

### Q7 — Adapter registry: new `adapters.toml`, or a config section?

**Neither — the registry already exists.** `PoolConfig::from_config()` reads
`[acp.adapters.*]` out of the host `config.toml` today
(`crates/ainb-hangar-daemon/src/acp_pool.rs`). Phase 7 exposes what is there
over RPC and validates the copilot's `provider` string against it. A second
file would be a second source of truth for a registry that already has one.

### Q8 — Is the `log` tab's per-session scoping enough?

**Yes.** The cross-session notification history the host `b` Inbox provided is
already carried by the hangar plugin's `I` Inbox tab, which is explicitly out
of scope and stays. Adding a second cross-session view to the `log` tab would
recreate the duplication this epic is deleting.

## Phases

| # | Phase | State |
|---|---|---|
| 0 | Baseline, log, decisions | done |
| 1 | Chips on session rows | done |
| 2 | Attention merge (`AttentionRow` view model) | done |
| 3 | Right-pane tab strip | done |
| 4 | Answering from the `ask` tab | done |
| 5 | Delete the host Fleet panel | done |
| 6 | Delete the host Inbox | done |
| 7 | Copilot registry + mode dial | done |
| 8 | Broadcast + rehoming, delete `t` start form | done |
| 9 | ATC lite audit + daemon retry sweep | pending |
| 10 | ACP chat repair | pending |

## Deviations from the spec, with reasons

Each is a place the spec asked for something the tree cannot currently
express. None is a shortcut around work.

| Spec line | What shipped | Why |
|---|---|---|
| Insta: chip strip "at 80 and 100 columns, both themes" | 80, 100 and 42 columns; one palette | `ui_preferences.theme` is persisted config that **no component reads** — the palette is hardcoded consts in every renderer. There is one rendered theme to snapshot. 42 is added because it is the real default sidebar width and the only one that exercises the name-truncation path |
| Left-pane mock titled `Sessions · 2 need you` | `Workspaces (4) · 2 need you` | `(4)` is the workspace count and the filter's feedback loop; calling the panel "Sessions" while showing a workspace count is worse than keeping the noun. The badge — the part the spec is actually specifying — is verbatim |
| Header carries the elsewhere count | Own trailing row, `N waiting elsewhere` | At the default 38-column sidebar a fourth title item truncated mid-word to `1 el`. Proven in a real tmux capture, then moved |
| "Hangar daemon down → one banner line on the sessions header, not two" | Deferred to phase 5 | "Not two" is only meaningful once the host Fleet panel (the second banner) is deleted. Until then the daemon-down state is carried by the dimmed chip and its reason, which is the part that is not deferrable |
| `Tab` cycles the strip | `Tab` cycles it, `Shift+Tab` walks back, and `SwitchPaneFocus` is gone | `Tab` was pane focus. Focus now FOLLOWS the tab (composer tabs take input, `preview`/`log` do not), so the one thing that key bought is a consequence of this one. Two keys for one concept is the ambiguity the strip removes |
| Footer: "1-9 attach from every tab" | True except inside a live composer, where the footer says so | A `3` typed into a message has to be a `3`. Advertising attach there advertises a key that types a character |
| ERR chip from a local producer | Wired and unit-tested; reachable from the daemon (`error` / `escalation`) and from `SessionStatus::Error` | No local hook event classifies as an error — `classify_attention` has three outcomes and none of them is one. Inventing a fourth would be a producer the spec did not ask for |
| Copilot header: `◀ e cycle`, `◀ o`, `◀ g` on bare letters | `⌥e` / `⌥o` / `⌥g`, rendered that way | The copilot composer takes focus the moment the conversation opens, so a bare `e` is an `e` in a half-typed message. The tripwire proved the dials were unreachable in the state an operator is usually in. Alt never types, so one binding works in both halves of the pane; a bare key advertised on the header that silently does nothing most of the time is worse than a modified one |
| `mode help` → "no write tools in the table" | Refused at the classifier; the advertised MCP tool table is unchanged | The tool table is announced once at `initialize`, and the dial can move mid-session. A table pinned at spawn goes stale the instant an operator turns the dial, and a stale PERMISSIVE table is worse than an accurate refusal. `CopilotMode::tools()` exists and is tested as the projection; the enforcement point is the live daemon-side classifier, which reads the channel row on every call |
| Broadcast as its own strip tab | `thread` becomes `broadcast (N)` while rows are checked | The spec's strip is five tabs and a sixth would not be one. The codebase already has the rule this needs — "checked rows win over the cursor", which `Enter` and `r` follow on this screen — so the checkbox means one thing everywhere rather than one thing per verb. The label and the Enter hint both carry the count, because "send message" is a dangerous thing for a footer to say when the message is going to four sessions |
| Broadcast through the chat surface | Its own small pane, not a `ChatHost` | A thread and the copilot are CONVERSATIONS: durable scope, timeline, a page that reloads it. `fleet/broadcast` fans one text out and answers with N receipts and there is nothing to page afterwards. Modelling it as a conversation means inventing a scope for it, which is exactly the named channel the copilot tab already offers |
| Channels as a picker inside the copilot tab | A `channels` row on the header | What the spec's own header mock draws. A picker implies switching the pane's conversation, which is the `Channel` topic the chat surface already has and the F tab already drives; the name is the way back to one, and listing them is what the copilot pane owes |
| Model picker from the adapter's `config_options` | From a declared `[acp.adapters.<name>].models` list | ACP has no model-discovery call, so an adapter cannot be ASKED what it runs. `config_options` are values already set, not choices. A declared list is the only honest source; empty means the header says "adapter default" rather than offering a guess that would fail at `session/set_config_option` |

## Pre-existing breakage fixed on the way

`cargo test -p ainb --lib` failed **9 tests on `main` (296065a3)** whenever run
from a pane `ainb` itself had spawned; green on a clean CI runner. Verified by
running the baseline in a throwaway worktree: identical 9 failures.

One root, eight cascades. `export_env_bridge` clears any variable still holding
the parent's exact planted value before publishing its own, and
`AINB_BRIDGED_VARS` is how it recognises one. A pane spawned by the TUI inherits
`AINB_FLEET_STATE_STALE_MS=0` as a parent plant, so the "user override" the test
sets is stripped and re-planted as the bridge's own — the legacy rung it asserts
on becomes unreachable. It then panicked before its cleanup, leaving the
process-global snapshot and the `BRIDGED` set installed; the next test read a
self-planted `AINB_HEADROOM_PORT` as unset, panicked holding `ENV_LOCK`, and
poisoned it for six more.

Fixed with an RAII guard covering `AINB_BRIDGED_VARS` and releasing on unwind
(`43561551`). 2150/2150 lib tests now pass, parallel and single-threaded.

## Bugs found by the tripwires, not by the tests

| Bug | How it surfaced | Fix |
|---|---|---|
| A failed **option** send wrote the option's label into the free-text composer and moved the cursor there, so a retry would have sent typed text instead of the option | Daemon-up tripwire picked the second option, watched the send fail, and found `api/src/db.sqlite` in the composer | `355d582e` — only a typed answer is a restorable draft |
| The `ask` pane opened with no question on a locally-produced row, and with the composer unfocused when the request had no options | Daemon-down tripwire typed into it and `d` opened the delete-session dialog | Local chips now carry the hook's own `message`; `retarget` focuses the composer when there is nothing to pick |
| A failed `attention/list` poll emptied the row map, so one socket timeout made every live ASK vanish for five seconds | Tab-strip tripwire failed one run in seven, the `ask` tab dimmed because its chip had briefly gone | `f40e09f7` — rows carry across a failed poll and grey out instead, which is the shape the spec asked for all along |
| `tmux_sessions_at` interpolated a cwd into a tmux FORMAT string, where `#(...)` runs a shell command | Background security review of `c577cbb0` | `391f0607` — deleted; it had no callers and the `N waiting elsewhere` row already serves the case |
| The elsewhere count truncated to `1 el` in the panel title | Phase-2 tripwire assertion on a real 38-column capture | Moved to its own full-width row |
| The copilot dials were unreachable: bare `e` / `o` / `g` went into the chat composer, which holds focus from the moment the conversation opens | Phase-7 tripwire pressed `e` and watched the engine not move, with no failure line either | Moved to `⌥`, which never types, and the header renders the modifier |
| `fleet/adapter_list` read the config registry while `copilot_configure` and `acp_session_create` fell back to the hardcoded two-name floor: the picker OFFERED an operator's configured adapter and the write then refused it as `unknown adapter` | Phase-7 tripwire cycled to an adapter that exists only in `[acp.adapters]` and got the refusal rendered on the pane | One `adapter_registry()` resolution now serves the list and both writes |
| The help overlay and the panel legend still documented `b Inbox` and `f Fleet control panel`, both deleted in phases 5 and 6 | Full `--tests` sweep; earlier passes had filtered tripwires out the way CI's Test job does | `065d5a2b` — entries replaced by the pane that took over from both, and both tripwires now assert their ABSENCE |
| CI invoked two deleted tripwires by NAME and had been failing on a missing test binary ever since phases 5 and 6 | The first CI run that actually dispatched after the merge | `cafc4378` — replaced by the sessions tab strip and the copilot picker, the surfaces that took the work over |
| `acp_session::ensure` validated a provider against the two-name built-in floor while `fleet/adapter_list` read the config registry: the picker OFFERED an operator's configured adapter and the mint behind it refused the same name | The copilot tripwire on CI, not locally — the local green run predated the merge that put `ensure` in this path, and only the lib suite was re-run after the rewire | `908b5500` — one resolution beside the pool that owns the registry, used by the list and by every write |

## Phase 8: where each rehomed verb went

| Verb | Old home | New home | Exercised by |
|---|---|---|---|
| broadcast compose + send | panel `b`, with its own recipient picker | `thread` tab while rows are checked; recipients ARE the checkboxes | `tripwire_sessions_broadcast` (two checked rows, both legs rendered) |
| broadcast recipient picker | panel modal, second roster | DELETED. The sessions list's checkboxes are the only selection model | same |
| named channels list | panel `N` create form | `channels` row on the copilot header | `an_unanswered_channel_list_is_not_an_empty_one` |
| new-ATC name prompt | panel `FleetPanelNewAtc*` events | Daemons screen `Provision`, which already provisioned an instance and brought it up without one existing | pre-existing Daemons menu tests; `rg FleetPanelNewAtc` is ZERO |
| managed-Codex start (`t`) | hangar plugin `FleetMode::Start` | DELETED outright. `ainb run` and the new-session flow are the spawn paths, and they know about worktrees, hooks and the session registry, which this form did not | `the_retired_start_key_opens_nothing` |
| Codex remote-control | — | already the default (`[codex] app_server = desktop`), no UI | n/a |
| roster, fleet-wide operator views | — | hangar plugin `F` tab, untouched | its own reducer tests |

## Parity: every verb the deleted Fleet panel had

The spec makes this a must-pass gate — each verb is **rehomed and exercised**,
or **explicitly deleted**. Nothing is left implicit.

| Panel verb | Key | Outcome | Exercised by |
|---|---|---|---|
| move row selection | `↑ ↓ k j` | rehomed: the sessions list's own selection | every sessions tripwire |
| move the ASK option cursor | `Tab` `⇧Tab` | rehomed: `↑ ↓` in the `ask` pane | `tripwire_sessions_answer_daemon_up` (picks the SECOND option) |
| answer the selected ASK | `Enter` `a` | rehomed: `Enter` on the `ask` pane | `..._answer_daemon_up`, `..._answer_daemon_down` |
| approve / deny a permission request | `y` `n` | rehomed: `approve`/`deny` options in the `ask` pane, delivered through notifyd's broker | `tripwire_sessions_approve_unblocks_hook` |
| open the copilot conversation | `m` | rehomed: the `copilot` tab | `tripwire_fleet_chat_screen` |
| open the selected session's thread | `M` | rehomed: the `thread` tab | `tripwire_fleet_session_thread` |
| answer a guardrail confirm card | `⇥` then `y` | rehomed: `⇧⇥` then `y` in the `copilot` tab | `tripwire_fleet_chat_screen` |
| force-refresh from the daemon | `F5` | rehomed as automatic: the attention poller's 5s cadence, with rows carried across a failed poll | `tripwire_sessions_daemon_attention` |
| back to the previous screen | `q` `Esc` | unchanged | `tripwire_fleet_chat_screen` (Esc leaves the conversation) |
| restart the selected session | `R` | already on the sessions screen (`E` restart) | pre-existing |
| roster lenses 1-5 | `1`-`5` | rehomed in spirit: the sessions list's own `F` filter. The fleet-wide lens view itself is the hangar plugin's `F` tab | pre-existing |
| reconcile a Claude interview | `r` | **not rehomed** — hangar plugin `F` tab, untouched | plugin's own tests |
| the multi-question vertical-card interview | `Enter` on a multi-ASK | **not rehomed** — hangar plugin `F` tab, untouched. The sessions screen answers one question at a time from the daemon's payload | plugin's own tests |
| ACP session visible in a roster | — | **not rehomed** — an ACP session has no host session row; it is counted by the `N waiting elsewhere` row, and the roster is the plugin's `F` tab | `tripwire_sessions_daemon_attention` (the elsewhere count) |
| attach to an EXACT `session:window.pane` | `→` | **deleted.** The sessions screen attaches by session, which is the same thing for an ainb session (one pane) | — |
| broadcast to the current lens | `B` | **rehomed in phase 8** — the sessions list's existing checkboxes | phase 8 restores `tripwire_fleet_broadcast_channel` against the copilot tab |
| create a named channel | `N` | **rehomed in phase 8** — the channels list inside the `copilot` tab | as above |
| new-ATC name prompt | `n` | **rehomed in phase 8** — the Daemons screen | as above |
| managed-Codex start form | `t` | **deleted in phase 8.** Codex remote control is already the default (`[codex] app_server = desktop`) | — |

## Parity: the deleted host Inbox

| Inbox verb | Key | Outcome | Exercised by |
|---|---|---|---|
| this session's notification history | — | rehomed: the `log` tab | `tripwire_sessions_tab_strip` reads a real hook row back |
| the fleet-wide notification list | `b` | **not rehomed** — the hangar plugin's `I` tab, untouched. Rebuilding it here recreates the duplication the epic deletes | plugin's own tests |
| unread badge on the menu bar | — | **deleted.** The needs-you badge on the sessions header is the count that matters: what is BLOCKING an agent, not what is unread | `tripwire_sessions_attention_chips` |
| dismiss / archive / agent filter | `d` `C` `a` `p` | **deleted** with the screen | — |
| jump to the row's tmux session | `Enter` | rehomed: the attach digits, which work from every tab | `tripwire_sessions_tab_strip` footer assertion |

## Suite status

`cargo test --workspace`: **465 test binaries, 464 pass.**

The one failure is `ainb-plugin-cts-v2::real_plugin_axes`, and it is not a
code failure — it is that test's own safety guard:

```
scratch hangar home .../ainb-tui/target/tmp/real-plugin-axes-hangar-home
must not be inside the live hangar home /Users/stevengonsalvez/.agents-in-a-box
```

This worktree was created by `ainb run`, so it lives under
`~/.agents-in-a-box`, and so does its `target/`. Verified by re-running with
`CARGO_TARGET_DIR=/tmp/ainb-cts-target`: **passes, 74.66s**. A CI checkout is
not under that path, so it is green there. Nothing in this branch touches that
crate.

## Published pages

_(none yet)_

## Blockers

_(none)_
