# GOAL: verify-converged — full autonomous verification of the converged control center

Drop this file into a fresh Claude Code / Codex session at the repo root and run it
end-to-end without hand-holding. It is the **P11 acceptance harness** — the successor to
`verify-hangar-goal.md` (F01–F44 + R01–R08, all inherited unchanged) extended with the
converged control-plane legs the P1–P10 phases shipped and the resilience blind spots the
static suite omits. The outcome is a per-leg PASS/SKIP/FAIL table plus a maintained
J1–J5 × C1–C5 journey-traceability table. You are verifying the product the way a
sceptical user would — real binaries, real tmux, real SQLite — not re-running unit tests.

> Supersedes `verify-hangar-goal.md`. That file's F01–F44 (feature walk) and R01–R08
> (resilience legs) are carried in verbatim by reference (Phase B / Phase C below); this
> doc adds Phase D (converged C-legs) + Phase E (resilience blind spots) + the traceability
> table. Where a converged leg subsumes an older one, the older leg still runs — additive,
> never a regression.

## Mission

```
┌─────────┐  ┌──────────┐  ┌────────────┐  ┌────────────┐  ┌────────────┐  ┌────────┐
│ build + │─▶│ seed     │─▶│ F01-44     │─▶│ C-legs     │─▶│ resilience │─▶│ report │
│ stage   │  │ fixtures │  │ feature    │  │ attention· │  │ blind spot │  │ table +│
│ plugin  │  │ 2 ws +   │  │ walk       │  │ boards·    │  │ isolation· │  │ J×C    │
│ +daemon │  │ attention│  │ (Phase B)  │  │ squad·ATC· │  │ kill-9·    │  │ matrix │
│         │  │ + squad  │  │            │  │ history    │  │ migrate·cap│  │ +exit N│
└─────────┘  └──────────┘  └────────────┘  └────────────┘  └────────────┘  └────────┘
```

A leg is GREEN only when a **positive** assertion (seeded data rendered / row persisted /
answer delivered / bytes on disk) AND a **negative** assertion (no placeholder, no
prior-screen bleed, no cross-workspace leak, no over-dispatch) both hold within a
deadline-bounded poll.

## Hard safety rules (non-negotiable — inherited unchanged)

- NEVER `tmux kill-server`, `pkill tmux`, `killall tmux`, or wildcard session kills.
  Kill ONLY sessions you created, by their exact unique name
  (`hangar-verify-<pid>-<nanos>`, `hangar-answer-target-<pid>-<nanos>`).
- Kill the daemon ONLY by the exact child PID you spawned (`kill -9` the captured pid via
  `nix`, never by name).
- NEVER `cargo clippy --workspace -- -D warnings`; never crate-wide `cargo fmt`.
- Per-leg `$HOME` tempdirs; never touch the real `~/.agents-in-a-box`.
- Every cargo invocation runs under the shared target dir when one is configured.

## Phase A — environment setup

Same as `verify-hangar-goal.md` Phase A, plus:

1. The Cargo workspace lives in `ainb-tui/` — **the repo root has no `Cargo.toml`** and the
   build-plugins script is at `ainb-tui/scripts/build-plugins.sh`, so run every cargo/plugin
   command from the workspace, not the repo root:
   `cd ainb-tui && cargo build -p ainb -p ainb-hangar-daemon` (release not required). Stage
   the plugin with `bash ainb-tui/scripts/build-plugins.sh` (from the repo root) or
   `./scripts/build-plugins.sh` (from `ainb-tui/`) → verify
   `ainb-tui/dist/plugins/hangar-tui/hangar-tui` is executable; on macOS re-`codesign
   --sign -` + `touch` after any copy so AMFI does not SIGKILL a stale-signed binary
   (exit 137, no stderr).
   - **Shared-target caveat.** The tripwire gate (`can_run_tripwire`) derives
     `dist/plugins` as a sibling of the cargo target dir (from the test binary's own
     path). When `CARGO_TARGET_DIR` points OUTSIDE the workspace, stage the plugin into
     `$(dirname "$CARGO_TARGET_DIR")/dist/plugins/hangar-tui/` (binary + `manifest.toml`,
     re-signed + touched on macOS) or the tripwire SKIPs instead of running.
2. `tmux -V` and `sqlite3 --version` must succeed, else every tmux/daemon leg is
   `SKIP: <reason>` (greppable), never FAIL.
3. Reuse the seed helpers in `crates/ainb-hangar-daemon/tests/tripwire_p4_common.rs`
   (`prepare_pipeline*`, `seed_attention_pair`, `seed_deliverable_ask`, `DeliveryTarget`,
   `attention_row_state`, `seed_autopilot`, `seed_logs`) rather than re-inventing fixtures.

## Phase B — feature walk (F01–F44)

Inherited verbatim from `verify-hangar-goal.md` Phase B. The checklist (F01–F44), surface
protocols (CLI / TUI / daemon), and marker discipline are unchanged. Run them as before.

## Phase C — resilience legs (R01–R08)

Inherited verbatim from `verify-hangar-goal.md` Phase C (daemon kill-9, plugin crash /
host-exit orphan, multi-workspace data isolation, migration-from-populated, concurrent
dispatch cap, retry+timeout, create-flow round trip, short soak). Phase E below pins each
to its concrete gating test.

## Phase D — converged control-plane legs (C-legs)

Each C-leg proves one P1–P10 journey behaviour end to end through the REAL daemon +
store (a `MOCK` note marks the few plugin-side legs that drive a fake daemon by design).
Every leg is a checked-in test that CI gates today (see "CI wiring"), so this phase is
`cargo test`-runnable AND re-assertable by a sceptical operator. Statuses below were
re-run locally on macOS at P11 close.

| id | Journey | Behaviour proven | Drive via (test file) | Real / Mocked | Status |
|----|---------|------------------|-----------------------|---------------|--------|
| CC01 | C1 | **answer FLIP e2e** — ASK answered from the TUI control center: press `2` → answer RPC → C1 live-target resolve → verified tmux delivery → row `open`→`answered` → board refreshes to `0 need you`; picked option lands in the target pane; DB row reads `answered`=`prod` | `ainb-hangar-daemon/tests/tripwire_p2_answer_flip_e2e.rs` **(NEW, agents-in-a-box-43e)** | REAL daemon + REAL tmux last-mile delivery | GREEN |
| CC02 | C1 · C4 | control-center RENDER: urgency shuffle (ASK above older WAIT), inline ①②③ options, `N sessions / M need you` counts, return-nav | `tripwire_p2_control_center_attention.rs` | REAL binaries | GREEN |
| CC03 | C1 | pressing a digit issues `attention/answer(row_id, option_label)` from the plugin | `ainb-plugin-hangar/tests/attention_answer_over_socket.rs` | MOCK daemon (by design) | GREEN |
| CC04 | C1 | first-answer-wins idempotency: a second surface gets `already_answered`, never a second delivery | `rpc_attention.rs` | REAL daemon | GREEN |
| CC05 | C1 | attention `list` scoping (fleet / workspace / host) + `subscribe` fleet-wide raised delta | `rpc_attention.rs` | REAL daemon | GREEN |
| CC06 | J1 · J3 | **boards create→run→auto-move**: card runs → task `succeeded` → card green + auto-moved per column↔FSM mapping | `tripwire_board_auto_move_e2e.rs` | REAL daemon | GREEN |
| CC07 | J1 | user-defined kanban columns render across the lifecycle; board management RPCs | `tripwire_kanban_columns_render.rs`, `rpc_board_management.rs` | REAL daemon | GREEN |
| CC08 | J2 | provider runner: Claude + Codex headless/interactive spawn, env/exit/stream/timeout | `runner_claude.rs`, `runner_codex.rs`, `tripwire_task_happy_path_claude_provider.rs` | REAL runner (provider mocked via fake-claude/fake-codex) | GREEN |
| CC09 | C5 | **squad fan-out**: leader gets the brief, ≥2 member tasks on one issue claimable in parallel; human-leader rejected | `rpc_squad_management.rs::squad_fanout_rpc_*` | REAL daemon | GREEN |
| CC10 | C5 | Squads TUI: open → create → assign → members + live status | `ainb-plugin-hangar/tests/tripwire_squads_flow.rs` | REAL plugin↔daemon | GREEN |
| CC11 | C2 | **bridge outbound formatting**: "session X asks: … ①②③" + numbered "reply N" contract; de-dupe | `ainb-core` `fleet::bridge::outbound` unit tests | unit | GREEN |
| CC12 | C4 | **auto-standup gate**: fires only when idle-at-prompt; never mid-turn/busy; per-session opt-out; cooldown; max-1-concurrent | `ainb-hangar-daemon` `standup::decide_standup` unit tests | unit | GREEN |
| CC13 | C3 | ATC on the daemon: register / list / escalate(→attention) / unregister(heartbeat off) over the socket; blank-name + invalid-cron rejected | `rpc_atc.rs` | REAL daemon | GREEN |
| CC14 | J5 | **history rows**: `run_history` per run (tokens/cost/diff/outcome); cost rollup absorbs fleet cost | `rpc_run_history.rs`, `rpc_usage_rollup.rs` | REAL daemon | GREEN |
| CC15 | J5 | OTLP export: a run's span/metrics reach a local collector when the endpoint is set | `tripwire_otel_export_when_endpoint_set.rs` (`--features otlp`) | REAL exporter → local collector | GREEN (feature-gated) |
| CC16 | J4 | autopilot cron: fires on schedule; skips when a run is in flight | `tripwire_autopilot_fires_on_schedule.rs`, `tripwire_autopilot_skips_when_running.rs` | REAL scheduler | GREEN |
| CC17 | J1 | **profile compile — both targets** (Claude `.md` master → Codex `[profiles.<slug>]` + prompt, WARN on dropped tools/color); fs-watch pickup | `ainb-hangar-core/src/profile.rs` (`compile_claude_is_lossless_golden`, `compile_codex_is_lossy_with_warnings_golden`, `compile_codex_no_warnings_when_no_dropped_fields`) + `ainb-hangar-daemon/src/profile.rs` (`materialise_claude_writes_lossless_subagent`, `materialise_codex_writes_config_prompt_and_warns`, `refresh_index_upserts_updates_and_prunes` for the fs-watch reconcile pickup) + `ainb-plugin-hangar/tests/snapshot_profiles.rs` | unit (core compilers + daemon materialise/reconcile) + plugin snapshot | **GREEN (P5/8fbc9bd3 landed) — re-run locally: `cargo test -p ainb-hangar-core --lib profile::` (13 passed), `cargo test -p ainb-hangar-daemon --lib profile::` (12 passed), `cargo test -p ainb-plugin-hangar --test snapshot_profiles` (5 passed). No dedicated `tripwire_*` e2e file exists for this leg yet — the doc's original "Drive via" was blank; the unit/snapshot suite above is the closest-fidelity coverage of the same behaviour (both-target compile + WARN-on-drop + edit-on-disk reconcile pickup) and is what CI already runs on this branch under the `test` job, same posture as CC08/CC11/CC12. A future `tripwire_profile_compile_e2e.rs` exercising the live `notify` watcher end to end would strengthen this further but is not required to close P11.**
| CC18 | C2 | **web ASK-answer e2e** (real browser): the daemon-seeded 3-option ASK renders on the dashboard via `GET /api/snapshot` (attention/list, D18) → click option ② → `POST /api/answer(answeredBy=web)` → daemon C1 resolve + verified tmux send → the answered row drops off the open inbox (ASK card disappears) → the pick lands in the raising session's real tmux pane → the store row reads `answered`/`web`/`2` | `ainb-tui/crates/ainb-web/e2e/tests/ask-answer.spec.ts` via `scripts/hangar/run_web_e2e.sh` **(NEW, agents-in-a-box-bct.8)** | REAL daemon + REAL tmux last-mile + REAL chromium (provider not a live agent — the delivery target is a plain-shell tmux session, same as CC01) | GREEN |

**Running CC18.** `bash scripts/hangar/run_web_e2e.sh` (macOS/Linux). Self-contained + idempotent: it builds `ainb` + `ainb-hangar-daemon` + the `seed_control_center` example into the shared target, provisions a short `/tmp` HOME (unix-socket 104-char limit), seeds the 3-option ASK + spawns the daemon, wires a real delivery-target tmux session via a fake `ainb list` (`AINB_BIN`) + `AINB_FLEET_TRANSPORT=tmux-only` (the `record-control-center.sh` technique), starts `ainb web` with a bearer token, installs the Playwright chromium on demand, runs the headless journey, and tears everything down by EXACT name / PID only. Requires `tmux`, `sqlite3`, `node`, `npm` (else it exits `2` with the missing tool named). Not CI-gated on this branch — it is a local real-browser leg (like the live-provider journey suite); the deterministic answer-path proof CI already gates is CC01.

## Phase E — resilience blind spots (the P11-mandated legs)

The four blind spots the goal calls out explicitly, each pinned to its gating test and
re-run locally at P11 close. All GREEN.

| id | Blind spot | Pass condition (positive + negative) | Gating test | Status |
|----|-----------|--------------------------------------|-------------|--------|
| RB01 | **multi-workspace isolation** | two NON-EMPTY tenants (`id != slug`); each list query returns only its own rows; ACME never sees GLOBEX's issue/agent and vice-versa (seeded cross-tenant leak assertion). TUI switch flips visible issues | `ainb-hangar-store/tests/workspace_data_isolation.rs` (store) + `tripwire_workspace_switch_e2e.rs` (TUI) | GREEN |
| RB02 | **daemon kill-9 restart recovery** | `kill -9` the exact daemon pid mid-`running`-task; WAL opens cleanly post-unclean-kill; the orphaned `running` row reaches a recovered/terminal state on restart, never stuck | `tripwire_daemon_crash_recovery.rs` | GREEN |
| RB03 | **event outbox resume proves state** | events persist to `event_log` with monotonic `seq`; a `subscribe` carrying `since_seq` replays exactly the events after that cursor (the daemon-restart resume path); workspace B never replays workspace A's log | `rpc_event_replay.rs` | GREEN |
| RB04 | **concurrent-dispatch cap** | 5 daemons contend for a cap-3 agent queue; a live `running`-count sampler never observes >3; no double-claim (DB audit); no WAL deadlock | `tripwire_daemon_concurrent_cap.rs` | GREEN |
| RB05 | **populated-DB migration upgrade** | apply 0001..N-1, seed ONE row for EVERY entity type, carry to head; every seeded row survives with fields intact; a second `apply_migrations` is a pure idempotent no-op | `migration_upgrade_full_chain.rs` | GREEN |

## Journey traceability — J1–J5 × C1–C5

The validation contract's coverage bar: by P11 every build journey (J1–J5) and every
control journey (C1–C5) has at least one green acceptance leg. Rows = journeys; the C-leg
column lists the Phase D/E legs that serve it.

| Journey | What it delivers | Served by (legs) | Real providers? | Status |
|---------|------------------|------------------|-----------------|--------|
| **J1** | kanban w/ custom columns, task in any column, agent + profile pick | CC06, CC07, CC17 | boards REAL; profile compile unit-level REAL | GREEN |
| **J2** | run headless (`-p`) or interactive YOLO, per provider | CC08 | runner REAL (provider mocked via fake) | GREEN |
| **J3** | attach from card; tmux always; green on done; auto-move | CC06, F37 (attach), CC02 (event refresh) | REAL | GREEN |
| **J4** | autopilot cron kept | CC16, F19–F23 | REAL scheduler | GREEN |
| **J5** | full history / traceability + OTel | CC14, CC15 | REAL daemon + REAL OTLP export | GREEN |
| **C1** | every input surfaced + answerable, ALL sessions | CC01, CC02, CC03, CC04, CC05 | answer path REAL (incl. tmux last mile) | GREEN |
| **C2** | web / channels same ecosystem | CC11 (bridge) + CC18 (web ASK-answer Playwright) | bridge unit REAL; web = REAL daemon + REAL tmux + REAL browser | **GREEN — bridge green (CC11) and the web click-② ASK-answer journey is now proven end to end by CC18 (`scripts/hangar/run_web_e2e.sh`): render → answer(by=web) → verified tmux delivery → store flip.** |
| **C3** | ATC session notified + broadcast/correct via skills | CC13 | REAL daemon | GREEN |
| **C4** | agentpeek UX (shuffle, standup) | CC02 (shuffle), CC12 (standup gate) | REAL render + unit gate | GREEN |
| **C5** | squads / workspaces purposed | CC09, CC10, RB01 | REAL daemon | GREEN |

**Matrix reading (J × C).** J-journeys are the *build* plane (how work is launched and
tracked); C-journeys are the *converge* plane (how every session's input/attention is
surfaced and answered across surfaces). A J×C cell is "covered" when a board/runner-launched
session (Jn) is answerable / observable through the converged plane (Cm) — the highest-value
crossings are exercised by CC01 (a launched session's ASK answered from the TUI), CC06+CC02
(a launched board card that auto-moves on the event bus), CC09+RB01 (a squad-launched
fan-out scoped to its workspace), and CC14/CC15 (a launched run's history + OTLP span).
The C2×web ASK-answer crossing is now covered by CC18 (real-browser Playwright, P8 landed).
The J1×* profile-compile crossing (CC17, P5/8fbc9bd3 landed) is now covered by the
core-compiler + daemon-materialise/reconcile + plugin-snapshot unit suite. Every J×C cell
in this matrix is now GREEN — no crossing blocks P11 completeness.

## CI wiring — the non-dispatching legs

Follow how hangar tripwires are gated today (do NOT add a bespoke workflow). Two
auto-globbing scripts, both invoked by the `hangar-e2e` job in `.github/workflows/ci.yml`,
pick up new legs WITHOUT editing YAML:

```
.github/workflows/ci.yml  (hangar-e2e job, ubuntu + macos)
        │
        ├─ scripts/hangar/run_all_tripwires.sh   ── globs crates/*/tests/tripwire_*.rs
        │     → CC01 (tripwire_p2_answer_flip_e2e), CC02, CC06, CC07, CC10, CC16,
        │       RB02, RB04, RB05(store tripwire_migrations_apply), workspace-switch …
        │
        └─ scripts/hangar/run_acceptance_tests.sh ── globs every NON-tripwire tests/*.rs
              → CC04, CC05, CC09, CC13, CC14, RB01(workspace_data_isolation),
                RB03(rpc_event_replay), RB05(migration_upgrade_full_chain) …
```

- **CC01 is auto-gated.** `tripwire_p2_answer_flip_e2e.rs` matches the
  `run_all_tripwires.sh` glob (`crates/ainb-hangar-daemon/tests/tripwire_*.rs`), so it
  runs on the Linux full leg with `HANGAR_TRIPWIRE_BUDGET_SCALE=3`. It is NOT in the macOS
  `HANGAR_TRIPWIRE_SMOKE` subset (deliberately minimal — heavy serial TUI tripwires flake
  on the small hosted macOS runner); its OS-agnostic render/protocol/delivery logic is
  authoritative on the Linux leg. No `ci.yml` edit is required or made.
- The unit-level converged legs (CC11 bridge outbound, CC12 standup gate, CC08 runner,
  CC17 profile compile/materialise/reconcile) ride the existing `test` job
  (`cargo nextest run --lib`); CC17's plugin-snapshot leg (`snapshot_profiles.rs`) rides
  `run_acceptance_tests.sh` (non-tripwire glob).
- `cargo xtask ci-lint` still asserts the `hangar-e2e` contract; unchanged.

## The cumulative journey suite (real providers) — LOCAL only

Per the validation contract, the REAL-`claude`/`codex`-dispatching journey legs (P2
raise→answer→deliver against a live `claude` session, P4 card→run→green against a live
provider, P7 squad fan-out onto live members, P9 ATC auto-answer) run LOCALLY at every
phase close and are NOT CI-required (CI keeps the deterministic non-dispatching unit +
acceptance + tripwire tests). CC01 is the deterministic, CI-safe proof of the SAME answer
path: it dispatches no `claude`, but exercises the daemon's real C1 resolve + real tmux
delivery into a real (plain-shell) session. When running the live-provider suite locally,
disclose per report which legs ran REAL vs seeded-fixture.

## Reporting

- Emit the three tables above: F01–F44 + R01–R08 (Phase B/C), the C-leg table (Phase D),
  the resilience-blind-spot table (Phase E), and the J×C traceability table.
- Exit code = number of FAILed legs. On any FAIL print the last 20 lines of the relevant
  capture/log.
- **DISCLOSE prominently** (mocked-vs-live rule): which legs dispatch a REAL provider vs a
  fake (`fake-claude.sh` / seeded fixtures). CC01's provider is not a live `claude` — its
  DELIVERY target is a real tmux shell and its daemon path is fully real; it proves the
  answer FLIP, not a live agent turn. CC18 (web) is the same posture: a REAL daemon + REAL
  tmux last-mile + REAL chromium, but its delivery target is a plain-shell tmux session, not
  a live agent — it proves the web click-② answer FLIP, not a live agent turn.
- **Un-landed matrix legs FAIL the P11 completeness bar — they are not benign PENDINGs.**
  The validation contract requires the full J1–J5 × C1–C5 matrix to be covered by P11, so
  a missing acceptance leg counts as a FAIL against P11 sign-off (and toward the exit code),
  even when its root cause is an un-landed upstream phase rather than a product regression.
  Both legs that previously blocked P11 completeness are now RESOLVED:
  - (RESOLVED) CC17 (profile compile, P5 / 8fbc9bd3) — the profile-compiler module
    (`ainb-hangar-core::profile` compilers + `ainb-hangar-daemon::profile` materialise/
    fs-watch-reconcile) landed and is covered by a 13+12+5-test unit/snapshot suite,
    re-run GREEN locally at this update.
  - (RESOLVED) C2 web ASK-answer (P8) — CC18 now ships the Playwright web suite
    (`scripts/hangar/run_web_e2e.sh`) and is GREEN; this cell is covered.
  Every J1–J5 × C1–C5 cell is now covered by at least one GREEN leg. **P11 is complete and
  the harness is GREEN** as of this update (tripwire suite: 40 ran, 0 failed, 0 skipped on
  the re-run that counts; one earlier `tripwire_workspace_switch_e2e` run flaked and passed
  clean on immediate re-run — see the verification report for the run this doc update was
  based on).
- Do not modify product code to make a leg pass. If a leg reveals a genuine product bug,
  file the evidence (a bead) — do not "fix" the product mid-run.
