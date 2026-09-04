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
| 4 | Answering from the `ask` tab | pending |
| 5 | Delete the host Fleet panel | pending |
| 6 | Delete the host Inbox | pending |
| 7 | Copilot registry + mode dial | pending |
| 8 | Broadcast + rehoming, delete `t` start form | pending |
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
| A failed `attention/list` poll emptied the row map, so one socket timeout made every live ASK vanish for five seconds | Tab-strip tripwire failed one run in seven, the `ask` tab dimmed because its chip had briefly gone | `f40e09f7` — rows carry across a failed poll and grey out instead, which is the shape the spec asked for all along |
| `tmux_sessions_at` interpolated a cwd into a tmux FORMAT string, where `#(...)` runs a shell command | Background security review of `c577cbb0` | `391f0607` — deleted; it had no callers and the `N waiting elsewhere` row already serves the case |
| The elsewhere count truncated to `1 el` in the panel title | Phase-2 tripwire assertion on a real 38-column capture | Moved to its own full-width row |

## Published pages

_(none yet)_

## Blockers

_(none)_
