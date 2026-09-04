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
| 1 | Chips on session rows | pending |
| 2 | Attention merge (`AttentionRow` view model) | pending |
| 3 | Right-pane tab strip | pending |
| 4 | Answering from the `ask` tab | pending |
| 5 | Delete the host Fleet panel | pending |
| 6 | Delete the host Inbox | pending |
| 7 | Copilot registry + mode dial | pending |
| 8 | Broadcast + rehoming, delete `t` start form | pending |
| 9 | ATC lite audit + daemon retry sweep | pending |
| 10 | ACP chat repair | pending |

## Published pages

_(none yet)_

## Blockers

_(none)_
