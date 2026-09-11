# Spike 4: specta on the real 19 sections, and SolidJS store fan-out at 100 sessions

**Run** 2026-09-11. **Gates** W0-mirror (D15). **Repo** `/home/claude/.ref/agents-in-a-box-desktop-app-part2`, branch `desktop-app-part2`, HEAD `7eede74f`.
**Spec** `docs/plans/2026-09-11-multi-surface-decisions-spec.md` spike row 4.
Part A ran in a throwaway detached worktree; nothing was committed, staged, or stashed. Confirmation at the end.

```
┌──────────────┐   ┌────────────────┐   ┌──────────────────┐   ┌───────────────┐
│ AppState     │──▶│ specta derive  │──▶│ AppState.ts      │──▶│ SolidJS store │
│ 124 fields   │   │ Type closure   │   │ 19 sections      │   │ 406 listeners │
│ 19 sections  │   │ 11 crates deep │   │                  │   │ 100 sessions  │
└──────────────┘   └────────────────┘   └──────────────────┘   └───────┬───────┘
        PART A                                                 PART B  │
                                                    2,000 frames / 2 s ▼
                                              ┌─────────────────────────────┐
                                              │ (i) per send   110 ms 18.6k │
                                              │ (ii) per drain  33 ms  9.6k │ ◀── ceiling
                                              │ (iii) + filter  18 ms  8.1k │
                                              └─────────────────────────────┘
```

## Headline

| question | answer |
|---|---|
| does one transaction per drain hold a ceiling? | **yes.** 32.5 ms apply and 9,644 computation runs per 2 s burst, against 110.4 ms and 18,616 for per-send. 3.40x on time, 1.93x on computation runs |
| **the CI ceiling for W0-mirror** | **12,800 computation runs per 2,000-frame burst, 32.5 ms apply per 1,000 frames, longest apply unit 2.4 ms, apply units exactly 125**, all at 100 sessions with 20% headroom over the measured worst case |
| is section subscription needed locally? | **no, keep it but do not gate on it.** 1.02x when the hot section is subscribed, 1.85x when it is not |
| does specta cost enough compile time to need a feature flag? | **no on compile time (+8.3% cold, +6.8% incremental). Yes on blast radius: it is a 15-crate change with 456 hand-maintained attributes** |
| does `AppState` export? | **yes.** 250 types, 238 declarations, 163.5 KiB (49.7 KiB without doc comments), exported in 0.02 s |
| does anything fail to derive? | **36 of 124 fields need `#[specta(skip)]`**, plus 28 more nested. One field (`attention_local_since`) cannot export at all: tuple map key |
| does the struct split cleanly along the 19 boundaries? | **8 sections clean, 5 near-clean, 5 mix wire and host state, 1 now empty.** Boundaries are fine; five sections need a wire/host split |

## Environment

| | |
|---|---|
| machine | 8 cores, 15 GiB RAM, Linux 6.8.0-137-generic x86_64 |
| disk free at start | 34 GiB on `/` (covers `/tmp` and `/home`); gate was 15 GiB |
| rustc / cargo | 1.96.0 (`ac68faa20` 2026-05-25) |
| specta | 2.0.0-rc.25 (features `derive`, `uuid`, `chrono`) |
| specta-typescript | 0.0.12 (0.0.9 pins `specta =2.0.0-rc.22` and will not co-resolve) |
| node / npm | v22.23.0 / 10.9.8 |
| solid-js | 1.9.15 |
| vite | 6.4.3 |
| playwright / browser | 1.63.0 / Chrome Headless Shell 153.0.8010.12 |
| cargo target dir | `<scratch>/spike4/target`, never the repo's |

## Part B: renderer fan-out (the number that becomes the CI ceiling)

### What was built

A Vite + SolidJS scratch app at `<scratch>/spike4/fanout/`, run in headless Chromium through Playwright over a local static server (`drive.mjs`). State is one `createStore` with 19 top-level sections; `sessions` holds 100 rows (id, name, status, host, updatedAt, ring, lastReply), `agent_status` holds 100 rows (state, provenance, tier, three timestamps), `board` holds 100 cards, and the other 16 sections are small scalar records.

**No JSX on purpose.** Every computation is an explicit `createEffect` or `createMemo` call, so the listener census is an exact count rather than a guess at what the Solid JSX compiler emitted. Fine-grained store subscription is unaffected: the reads still go through the same store proxy that JSX-compiled reads use.

### Listener census

| what | count | how counted |
|---|---|---|
| 100 row components x 3 subscriptions (own `sessions` row, own `agent_status` row, shared header + own status) | 300 effects | every `createEffect` goes through one `mkEffect` wrapper that increments a counter and wraps the body in a second counter for re-runs |
| 100 `board` card effects | 100 effects | same wrapper |
| root header effects | 3 effects | same wrapper |
| root selectors (`needsInput` reads all 100 session statuses, `runningCount` reads all 100 agent states, `boardTotal`) | 3 memos | `mkMemo` wrapper, same scheme |
| **total computations at mount** | **406** (403 effects + 3 memos) | counters read immediately after `createRoot` returns |

Method note: the census counts creations and runs, not dependency edges. Edge count is higher, because `needsInput` alone holds 100 edges. [fact]

### Burst

2,000 frames stamped 1 ms apart across a 2,000 ms window, drawn from a seeded LCG so every mode sees the identical stream: 70% `agent_status` single-row changes, 20% `sessions` row changes, 10% `board`. 5 runs per mode, seeds 1234-1238. The 2 s timeline is simulated from the frame timestamps rather than driven by real timers, so the numbers are apply cost and not idle wall-clock. Drain ticks fire whenever a frame's timestamp crosses a 16 ms boundary. [fact]

### Results

| mode | apply ms med | apply ms max | apply ms /1k frames (med) | apply units | frames applied | frames dropped | computation runs med | computation runs max | longest unit ms med | longest unit ms max | CPU share of 2 s burst |
|---|---|---|---|---|---|---|---|---|---|---|---|
| (i) one write per send | 110.4 | 135.4 | 55.2 | 2000 | 2000 | 0 | 18616 | 20071 | 1.5 | 5.1 | 5.5% |
| (ii) one batch per 16 ms drain | 32.5 | 54.1 | 16.25 | 125 | 2000 | 0 | 9644 | 10628 | 1.5 | 2 | 1.6% |
| (iii-a) (ii) + 5/19 subscribed, hot sections in | 32 | 37.5 | 16 | 125 | 2000 | 0 | 9644 | 10628 | 0.6 | 3.5 | 1.6% |
| (iii-b) (ii) + 5/19 subscribed, agent_status out | 17.6 | 18.3 | 8.8 | 125 | 607 | 1393 | 8107 | 9094 | 0.3 | 2.4 | 0.9% |

"Computation runs" counts effect plus memo invocations during the burst only, mount excluded. "Apply units" is the number of separate store-write transactions: 2,000 in mode (i), 125 drains in modes (ii) and (iii).

### Ratios

| comparison | apply ms | computation runs | apply units |
|---|---|---|---|
| (i) / (ii) | **3.40x** | **1.93x** | 16x |
| (ii) / (iii-a), hot sections still subscribed | 1.02x | 1.00x | 1x |
| (ii) / (iii-b), `agent_status` unsubscribed | **1.85x** | **1.19x** | 1x |

Batching wins 3.4x on apply time but only 1.93x on computation runs. [fact] The reason is that the 70% `agent_status` traffic spreads over 100 distinct rows, so a 16-frame drain rarely hits the same row twice and rarely collapses two effect runs into one. What batching does collapse hard is the shared selector: `needsInput` and `runningCount` ran a median 1,351 times in mode (i) against 242 in mode (ii), a 5.6x drop, because a batch re-evaluates each memo once instead of once per write. [fact]

Solid's memo equality check does a second, larger piece of work for free: `needsInput` returns a count, so a row flipping `idle` to `running` leaves the count unchanged, the memo's value does not change, and none of the 100 dependent row effects re-run. Without that, the 400 `sessions` frames alone would have driven 40,000 effect runs. [fact] A selector that returned a list or an object instead of a scalar would lose this and the fan-out would be roughly 4x worse. [inference]

### Memory

| point | V8 used heap | V8 total heap |
|---|---|---|
| after load | 1.08 MB | 2.06 MB |
| after 1 run of all 4 modes | 1.36 MB | 3.06 MB |
| after 5 runs of all 4 modes | 1.40 MB | 3.81 MB |

Measured through the Chrome DevTools Protocol (`HeapProfiler.collectGarbage` then `Runtime.getHeapUsage`), because `performance.memory` reported a bucketed constant 10,000,000 bytes and is useless here. [fact] Used heap is flat across 20 bursts, so `createRoot` disposal is releasing the 406 computations cleanly and there is no per-burst leak. [fact]

### Proposed CI ceiling for W0-mirror

Derived from the **maximum** observed in mode (ii) across 5 runs, plus 20%:

| gate | ceiling | max measured (mode ii) | headroom |
|---|---|---|---|
| computation runs per 2,000-frame burst at 100 sessions | **12,800** | 10,628 | 20% |
| apply ms per 1,000 frames | **32.5 ms** | 27.05 ms | 20% |
| longest single apply unit | **2.4 ms** | 2.0 ms | 20% |
| apply units per 2 s burst | **125** (exactly `2000 / 16`) | 125 | exact, it is a function of the drain interval |

The last row is the real invariant and the cheapest thing to assert: if the census reports more than `ceil(burst_ms / drain_ms)` apply units, something is writing outside the drain, which is D15's per-drain invariant broken. Assert it as equality, not a ceiling. [inference]

Two of these are worth stating as a single headline for the mirror bench: **12,800 computation runs and 32.5 ms of apply time per 1,000 frames, at 100 sessions.** Mode (i) violates both (18,616 runs, 55.2 ms/1k), so the gate actually fails if per-drain batching regresses to per-send. [fact]

## Part A: specta on the real `AppState`

### Verifying the handover's numbers first

| claim in the handover / P0 spec | measured today |
|---|---|
| `AppState` at `state.rs:3162-3521` | actually `state.rs:3357-3832` [fact] |
| 106 fields | **124 fields** [fact] |
| fields group into 19 sections | they do, and the grouping still holds, but the doc's field table lists 101 of them |

Drift against the 19-section table in `docs/plans/2026-09-05-desktop-p0-surface-safety.md`: **25 fields exist in the struct and are absent from the doc's table**, and **2 fields in the table no longer exist in the struct**. [fact]

Gone from the struct (both moved out to plugins): `fleet_panel_state`, `inbox_state`. The P0 spec's note that "two hold SQLite connections (`InboxState`, `FleetPanelState`)" and therefore block `Serialize` is now stale, and section 13 (`inbox`) has zero fields.

New since the table was written, mapped to their sections here: `ask_state`, `pal_chat`, `pal_dial`, `daemon_start_cta`, `broadcast`, `session_chat`, `session_tab`, `daemon_attention`, `fleet_snapshot`, `fleet_metadata`, `attention_poll_running`, `session_log`, `session_log_running`, `attention_elsewhere`, `attention_error_since`, `attention_local_since` (all `fleet`); `observer_pending`, `observer_failed_target`, `observer_started_at`, `embed_pane_area` (`tmux`); `plugin_render_areas`, `plugin_render_origins`, `plugin_last_render_viewport` (`plugins_host`); `statusline_status_cache` (`config`); `codex_degrade_announced` (`shell`).

The `fleet` section is now the largest at 20 fields and carries 11 of the 36 skips. Regenerate the table at implementation time, as Phase 2 already instructs. [fact]

### Field census across the 19 sections

| # | section | fields | derives as-is | needs cascade | `#[specta(skip)]` | skipped fields |
|---|---|---|---|---|---|---|
| 1 | `sessions` | 9 | 7 | 2 | 0 | none |
| 2 | `session_labels` | 6 | 2 | 4 | 0 | none |
| 3 | `tmux` | 14 | 6 | 2 | 6 | `embed` (live tmux PTY client); `observer_pending` (std Instant (no wire repr)); `observer_failed_target` (std Instant (no wire repr)); `observer_started_at` (std Instant (no wire repr)); `embed_pane_area` (ratatui Rect); `preview_update_task` (task handle) |
| 4 | `ssh` | 5 | 4 | 1 | 0 | none |
| 5 | `git_view` | 4 | 3 | 1 | 0 | none |
| 6 | `workspace_load` | 7 | 2 | 0 | 5 | `last_snapshot_time` (std Instant (no wire repr)); `last_preview_update` (std Instant (no wire repr)); `last_status_check` (std Instant (no wire repr)); `workspace_load_started` (std Instant (no wire repr)); `workspace_load_receiver` (channel) |
| 7 | `new_session` | 7 | 3 | 1 | 3 | `branch_refresh_receiver` (channel); `repo_check_receiver` (channel); `repo_init_receiver` (channel) |
| 8 | `logs` | 8 | 2 | 2 | 4 | `log_last_updated` (std Instant (no wire repr)); `last_log_check` (std Instant (no wire repr)); `log_streaming_coordinator` (docker log coordinator); `log_sender` (channel) |
| 9 | `claude_chat` | 3 | 1 | 1 | 1 | `claude_manager` (HTTP client manager) |
| 10 | `fleet` | 20 | 3 | 6 | 11 | `last_token_refresh_check` (std Instant (no wire repr)); `last_headroom_watchdog` (std Instant (no wire repr)); `live_window_watcher` (background poller (RwLock)); `pal_chat` (daemon chat host (socket)); `daemon_start_cta` (holds a spawn handle); `session_chat` (daemon chat host (socket)); `daemon_attention` (Arc<RwLock<..>> shared cell); `fleet_snapshot` (Arc<RwLock<..>> shared cell); `attention_poll_running` (Arc<Atomic*>); `session_log` (Arc<RwLock<..>> shared cell); `session_log_running` (Arc<Atomic*>) |
| 11 | `hangar` | 3 | 2 | 1 | 0 | none |
| 12 | `mcp_pool` | 1 | 0 | 1 | 0 | none |
| 13 | `inbox` | 0 | 0 | 0 | 0 | none |
| 14 | `plugins_host` | 7 | 5 | 0 | 2 | `pending_plugin_renders` (plugin wire buffer); `plugin_runtime` (plugin runtime handle) |
| 15 | `config` | 5 | 0 | 4 | 1 | `statusline_status_cache` (std Instant (no wire repr)) |
| 16 | `skills` | 4 | 0 | 2 | 2 | `skills_load_receiver` (channel); `drift_load_receiver` (channel) |
| 17 | `recovery` | 1 | 0 | 1 | 0 | none |
| 18 | `onboarding` | 4 | 0 | 4 | 0 | none |
| 19 | `shell` | 16 | 5 | 10 | 1 | `menu_bar_area` (ratatui Rect) |
| | **total** | **124** | **45** | **43** | **36** | |

| skip category | count |
|---|---|
| std Instant (no wire repr) | 12 |
| channel | 7 |
| Arc<RwLock<..>> shared cell | 3 |
| ratatui Rect | 2 |
| daemon chat host (socket) | 2 |
| Arc<Atomic*> | 2 |
| live tmux PTY client | 1 |
| HTTP client manager | 1 |
| docker log coordinator | 1 |
| task handle | 1 |
| plugin wire buffer | 1 |
| plugin runtime handle | 1 |
| background poller (RwLock) | 1 |
| holds a spawn handle | 1 |

Verdicts are per field, decided statically and then confirmed against `rustc`: `derives as-is` means the type is a primitive or a std container of primitives and specta already has the impl; `needs cascade` means a crate-local type whose own closure must derive `Type`; `#[specta(skip)]` means there is no wire representation and the field was skipped.

45 of the 124 fields derive with no work at all. 36 need a skip. 43 pull a cascade. [fact]

### Does the struct split cleanly along the 19 boundaries?

**Yes for 8 sections, no for 11.** [fact]

| | sections |
|---|---|
| zero skips, derives whole once the cascade is done | `sessions`, `session_labels`, `ssh`, `git_view`, `hangar`, `mcp_pool`, `recovery`, `onboarding` (8) |
| 1-2 skips, essentially clean | `claude_chat`, `config`, `plugins_host`, `skills`, `shell` (5) |
| 3+ skips, needs a real split between wire state and host state | `tmux` (6 of 14), `workspace_load` (5 of 7), `new_session` (3 of 7), `logs` (4 of 8), `fleet` (11 of 20) (5) |
| empty | `inbox` (0 fields) (1) |

The boundaries themselves are fine. What is not clean is that five sections mix serialisable view state with host-only machinery in the same struct, and `workspace_load` is 5/7 host-only, so it barely has a wire form at all. The fix is not to move the boundaries; it is to split those five sections into a `XSection` (wire) and an `XHost` (not wire) pair, which is a smaller change than re-drawing the section map. [inference]

### The cascade: how deep, and how wide

Deriving `specta::Type` on `AppState` with the 36 top-level skips in place produced **40 immediate missing impls**. Auto-deriving those produced 85 more. The closure did not settle until:

| | |
|---|---|
| workspace types given `#[derive(specta::Type)]` | **238** [fact] |
| workspace crates that ended up needing `specta` as a dependency | **15** [fact] |
| crates that ended up carrying at least one derive | 7: `ainb-core` (219), `ainb-cli` (6), `ainb-hangar-proto` (4), `ainb-plugin-notifyd` (4), `ainb-skill-core` (3), `ainb-adapters-source` (1), `ainb-plugin-hangar` (1) |
| rounds of compiler-driven fixpoint iteration | 11 across three passes |

This is the single biggest finding in Part A. **`specta` on `AppState` is not a change to `ainb-core`. It is a change to 15 crates.** [fact] The closure reaches `ainb-hangar-proto`, `ainb-hangar-store`, `ainb-hangar-core`, `ainb-hangar-sandbox`, `ainb-hangar-daemon`, `ainb-fleet-core`, `ainb-plugin-runtime`, `ainb-plugin-hangar`, `ainb-plugin-burndown`, `ainb-plugin-session-reader`, `ainb-plugin-notifyd`, `ainb-skill-core`, `ainb-adapters-source` and `ainb-cli`, because `AppState` holds plugin, daemon and skill types directly. Every one of those crates then compiles the `specta` proc macro in its graph. [fact]

The same skip categories recur *inside* the closure, not just at the top level. After the 238 derives the compiler was still naming `std::time::Instant`, `ratatui::prelude::Rect`, `ratatui::widgets::ListState`, `tokio::sync::mpsc::Sender`/`UnboundedReceiver`, `std::sync::mpsc::Sender`, `serde_json::Value`, `toml::Value`, and three `portable_pty` trait objects (`dyn MasterPty`, `dyn ChildKiller`, `dyn Child`). [fact] So the skip budget is the 36 top-level fields **plus** a second, larger set of nested fields inside the 238 types.

Notable errors that are not simply "missing impl":

| type | problem |
|---|---|
| `HashMap<(Uuid, AttentionKind, Option<String>), i64>` (`attention_local_since`) | TypeScript has no tuple-keyed record. specta will either refuse it or flatten it to `Record<string, number>` and lose the key structure. Needs a named key struct or a `Vec<(Key, i64)>` before it can cross the wire. [fact] |
| `serde_json::Value`, `toml::Value` | these have `Serialize` but no `Type`, so "it already derives `Serialize`" is not a reliable proxy for specta-derivability. [fact] |
| `Instant` (12 top-level fields, more nested) | monotonic, not wall-clock, and has no serialisable representation. Every one of these is a "last checked / started at" throttle. They are host-only by nature and should not be in a section that crosses the wire at all. [inference] |

### Compile cost

All timings are `cargo build -p ainb --lib` (the package is named `ainb`, not `ainb-core`; `--lib` builds the library only), with `CARGO_TARGET_DIR` pointed at scratch. Registry was already warm for every timed run, so none of these include crate downloads.

| measurement | wall time | max RSS |
|---|---|---|
| **cold full build, baseline (no specta)** | **183.85 s** | 3.01 GB |
| **cold full build, 15 crates with specta and 239 derives** | **199.11 s** | 3.09 GB |
| **cold delta** | **+15.26 s (+8.3%)** | +2.6% |
| warm no-op, baseline | 0.49 s (1.90 s on the first no-op) | - |
| warm no-op, with specta | 1.35 s | - |
| `touch state.rs` rebuild, baseline | **12.66 s** | 2.08 GB |
| `touch state.rs` rebuild, with specta | **13.52 s** | 2.13 GB |
| **touch-rebuild delta** | **+0.86 s (+6.8%)** | +2.4% |
| add `specta` + `specta-typescript` to `ainb-core` only, no derives, rebuild | 60.71 s | 3.01 GB |
| TypeScript export itself, warm binary | 0.01-0.02 s (3 runs) | - |

Both cold builds compiled 541 crates and the registry was warm for both, so the two are directly comparable. [fact]

**The compile cost is real but modest: +15.3 s on a 184 s cold build, +0.9 s on a 12.7 s edit cycle.** [fact] Part of the 15.3 s is `specta`, `specta-macros`, `specta-typescript` and `specta-util` compiling; the rest is 239 derive expansions spread over 15 crates.

The dep-add figure is separately informative. A `Cargo.toml` edit invalidates the crate, so 12.66 s of the 60.71 s is the `ainb-core` recompile a `state.rs` touch would cost anyway, leaving roughly 48 s for the specta crates themselves in that one graph. [inference] The cold-build delta came out far below that because cargo compiles those crates once and shares the artifacts across all 15 dependents, and it parallelises across 8 cores. [inference]

### What the export actually produced

It works. `AppState` exports to TypeScript, and the generated file is readable and correct-looking. [fact]

| | |
|---|---|
| exported types in the collection | **250** |
| `export type` declarations in the file | **238** |
| file size | **167,515 bytes (163.5 KiB)** |
| lines | **4,549** |
| gzipped | 56,333 bytes (55 KiB) |
| with Rust doc comments stripped | 49,725 bytes, 1,582 lines |
| export run time, warm binary | 0.01-0.02 s (3 runs) |

**70% of the file is Rust doc comments carried across as JSDoc.** 1,749 of the 4,549 lines are comment continuation lines. [fact] That is a feature, not bloat: the Tauri renderer gets the same reasoning the Rust code has, in editor tooltips. But it means the file size figure is dominated by prose, and "163 KiB of generated TypeScript" overstates the type surface. 49.7 KiB is the real one.

Five declarations are 32% of the code-only file:

| type | bytes | lines |
|---|---|---|
| `AppEvent` | 9,159 | 65 |
| `AppState` | 3,363 | 89 |
| `OnboardingState` | 982 | 32 |
| `ConfigureState` | 827 | 28 |
| `SkillsScreenData` | 706 | 23 |

`AppEvent` alone, with its 410 variants, is a fifth of the code-only output. It reaches the TypeScript because `AppState.pending_event: Option<AppEvent>` holds one. A renderer that consumes sections has no use for the reducer's entire event vocabulary, so skipping `pending_event` would cut the generated surface by roughly 20% and remove the largest single source of churn. [inference]

### Things that went wrong, and will go wrong again in W0-mirror

Every item here is a real error from this run, not a prediction.

| what | detail | cost to fix |
|---|---|---|
| **specta-typescript ships no `Format` impl** | `export_to` needs `impl Format`, and neither `specta` 2.0.0-rc.25 nor `specta-typescript` 0.0.12 provides one; `specta-serde` does. An identity `Format` was written here (8 lines). | trivial, but add `specta-serde` in real use or serde attributes are silently ignored |
| **64-bit integers are forbidden outright** | `usize`, `isize`, `u64`, `i64`, `u128`, `i128`, `f128` all refuse to export, to avoid `JSON.parse` precision loss. **169 field occurrences across 71 of the 239 derived types**, 39 of them in `state.rs` alone. | 157 were fixed with per-field `#[specta(type = ..f64..)]`; the rest needed a `specta_util::Remapper` on the collection |
| **`#[specta(type = ..)]` is rejected on enum-variant fields** | specta 2.0.0-rc.25 errors with "Found unsupported field attribute 'type'" on struct-variant and tuple-variant payloads, so there is no per-field escape hatch inside an enum. Hit 5 sites. | forces the `Remapper` route for any enum carrying a `usize`, which `AppEvent` does heavily |
| **`#[specta(rename = ..)]` is gone from containers** | "no longer supported on containers, use `#[serde(rename = "...")]` instead" | the Rust type has to be renamed, or the type must derive serde |
| **type names must be globally unique** | `MarkdownLine`, `MarkdownStyle` (`changelog.rs` vs `git_view.rs`) and `RowKind` (`code_review/model.rs` vs `new_session/pick_repo.rs`): **3 collisions, 6 types**. TypeScript has one flat namespace per module, so module paths do not disambiguate. | 3 Rust renames here; will recur every time two components name a type the same |
| **tuple map keys are refused** | `attention_local_since: HashMap<(Uuid, AttentionKind, Option<String>), i64>` fails with "tuple keys are not supported by serde_json map key serialization". Predicted statically, then confirmed by the exporter. | needs a named key struct or `Vec<(Key, i64)>`; skipped for this run |
| **`Serialize` is not a proxy for `Type`** | `serde_json::Value` and `toml::Value` both have `Serialize` and neither has `Type`. The P0 spec's reasoning that the blocker is the two SQLite-holding types is too narrow. | each needs a skip or a remap |

### Total size of the change

Getting `AppState` to export took, in the throwaway worktree:

| | |
|---|---|
| files changed | **80** |
| lines added / removed | 607 / 55 |
| `#[derive(specta::Type)]` added | **239** |
| `#[specta(skip)]` added | **64** (36 on `AppState` fields, 28 nested inside the closure) |
| `#[specta(type = ..)]` bigint remaps added | **153** |
| Rust types renamed for name collisions | 3 |
| crates given a `specta` dependency | 15 |

That is the honest size of "derive specta on the real `AppState`". It is not a one-file change. [fact]

## Decision input for D15 and W0-mirror

### 1. Does one-transaction-per-drain hold a ceiling?

**Yes, comfortably, and it should be the gate.** [fact]

At 100 sessions and 2,000 frames in 2 s, one batch per 16 ms drain costs 32.5 ms of apply time, 1.6% of the burst window, with the longest single apply unit at 2.0 ms. That is 12.5% of one 16 ms frame budget, so no drain risks dropping a frame. [fact]

```
budget per drain tick                16.0 ms
├─ measured apply, median             1.5 ms  ▓
├─ measured apply, worst              2.0 ms  ▓▓
└─ headroom                          14.0 ms  ░░░░░░░░░░░░░░░░░░
```

Set the gate at **12,800 computation runs and 32.5 ms apply per 1,000 frames**, and assert **apply units == ceil(burst_ms / drain_ms)** exactly. Per-send mode fails both numeric gates by 45% and 70%, so the gate has real teeth against a regression. [fact]

One caveat on extrapolating: batching bought only 1.93x on computation runs, not the order of magnitude the D15 wording implies. The win is concentrated in root selectors (5.6x on memo runs), not in per-row effects, because the hot traffic fans across 100 distinct rows and rarely collides inside one drain. **Adding sessions makes batching relatively less effective per frame, not more**, because collisions get rarer. Re-measure the ceiling if the target moves past 100 sessions. [inference]

### 2. Is section subscription needed locally?

**Not for a correctness or performance reason at 100 sessions. Keep it, but demote it from "required" to "available".** [inference]

The measurement splits cleanly:

| case | result |
|---|---|
| 5 of 19 sections subscribed, the three hot sections among them | 1.02x. Nothing dropped, filter overhead unmeasurable. |
| 5 of 19 sections subscribed, `agent_status` not among them | 1.85x apply time, 1.19x computation runs, 1,393 of 2,000 frames dropped before apply. |

Section subscription only pays when the renderer is genuinely not looking at the hot section, which for the desktop shell mostly means "the operator is on the git view, not the fleet panel". It is a real 1.85x in that case, and it is essentially free when it does not apply. [fact]

The spike's flip condition was "if per-drain batching cannot hold a ceiling, subscription becomes mandatory locally". **Batching holds the ceiling, so subscription does not become mandatory.** [fact] Keep it in W0-mirror anyway, because the phone and the web surface will need the same filter and it costs one `Set` lookup per frame, but do not gate on it and do not block the mirror on getting subscription semantics perfect.

The higher-leverage local optimisation is not subscription at all: it is keeping root selectors scalar. `needsInput` returning a count instead of a list is what stops one `sessions` write from invalidating 100 row effects. Write that down as a D15 invariant next to the other three, because it is worth more than the section filter and nothing currently records it. [inference]

### 3. Does specta on the real struct cost enough compile time to move it behind a feature flag?

**No, not on compile time. Yes on dependency blast radius.** Put it behind a feature flag, but for the right reason. [inference]

The compile-time answer is a clean no. The spike's implied worry does not survive measurement:

| argument | evidence | verdict |
|---|---|---|
| cold CI build cost | 183.85 s to 199.11 s, **+15.26 s (+8.3%)**, both compiling 541 crates [fact] | not enough to hide behind a flag |
| edit-cycle cost | `touch state.rs` rebuild 12.66 s to 13.52 s, **+0.86 s (+6.8%)** [fact] | negligible |
| the export step itself | 0.01-0.02 s to emit 250 types [fact] | free |
| **dependency blast radius** | the `Type` closure reaches **15 workspace crates**, `ainb-hangar-sandbox` and `ainb-plugin-notifyd` among them [fact] | **this is the problem** |
| maintenance surface | **456 attributes** (239 derives, 64 skips, 153 bigint remaps) across **80 files** [fact] | second problem |

So the flag is not a performance measure. It is worth having because **without it, `ainb-hangar-sandbox` and `ainb-plugin-notifyd` grow a TypeScript-generation dependency they have no business having**, and because 456 hand-maintained attributes with no gate will drift the moment someone adds a `u64` field. A flag plus a CI freshness check keeps both problems in one place.

Concretely for W0-mirror and D1:

1. `specta` as an **optional** dependency in each of the 15 crates, behind one workspace feature (`typescript-bindings`), and `specta-typescript` + `specta-util` only in `ainb-core`.
2. A CI job that builds with the feature on, regenerates `AppState.ts`, and fails on a diff. The same shape as the existing `docs/tui/cli.md` freshness gate at `.github/workflows/ci.yml:138-168`. Reuse that pattern rather than inventing one.
3. **Do not derive `Type` on `AppEvent`.** Skip `AppState.pending_event`. It costs 20% of the generated type surface for a reducer vocabulary no renderer consumes, and it is the field that forces the `Remapper` workaround. [inference]
4. Fix `attention_local_since` before it crosses the wire: the tuple map key cannot export at all. A named key struct is the smaller change. [fact]
5. Budget the split of `tmux`, `workspace_load`, `new_session`, `logs` and `fleet` into wire and host halves. Those five hold 29 of the 36 top-level skips; the other 14 sections are essentially clean. [fact]

### Not established by this spike

- Whether the 250 exported types are *correct*, not merely well-formed. The `Format` impl here is an identity, so serde attributes were **not** applied, and the generated file proves it: `ActionReceiptStatus` carries `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]` in Rust but exports as `"Pending" | "Delivered" | "Failed" | "Unknown" | "Rejected"`, not the screaming form the wire actually uses. [fact] Real use needs `specta-serde`; every `rename_all`, `skip_serializing_if` and `flatten` in the closure will change the type surface.
- Release-profile compile cost. Every timing here is `dev`.
- Fan-out above 100 sessions, and fan-out with a non-scalar root selector, which the analysis above flags as the bigger risk.
- Whether the 153 bigint remaps to `f64` are sound at runtime. They are not: remapping `u64` to `number` without a matching serde transform is exactly the precision loss specta refuses by default. This spike remapped to get a measurement; production must pick the string route or narrower integers per field.

## Artifacts and how to reproduce

All under `/tmp/claude-1000/-home-claude--ref-agents-in-a-box-desktop-app-part2/7ec9b5c8-5403-4a49-b0ee-f6b71be9354a/scratchpad/spike4/`:

| path | what |
|---|---|
| `AppState.ts` | the generated TypeScript, 167,515 bytes |
| `AppState.nodoc.ts` | the same with doc comments stripped, 49,725 bytes |
| `classified.tsv` | all 124 `AppState` fields, section, type, verdict, reason |
| `fields.tsv` | the raw field extraction |
| `cascade.json`, `cascade2.json`, `closer.json` | the compiler-driven derive fixpoint, round by round |
| `bigint-remaps.json`, `bigint-enum-remaps.json` | every 64-bit-int field remapped, with before and after |
| `dedupe.json` | the type-name collisions |
| `fanout-raw.json` | Part B, all 5 runs of all 4 modes, every metric |
| `fanout/bench.js` | the bench: store shape, listener census, burst generator, the three modes |
| `fanout/drive.mjs`, `fanout/heap.mjs` | the Playwright harnesses |
| `logs/` | every cargo invocation and its timing |

Part B reproduces in about a minute: `cd fanout && npx vite build && PLAYWRIGHT_BROWSERS_PATH=../pw-browsers node drive.mjs`.

Part A does not reproduce cheaply, because the 456 attributes lived only in the throwaway worktree, which has been removed. `classified.tsv` plus the three cascade JSON files are the durable record of what was derived, skipped and remapped.

## Worktree hygiene

| check | result |
|---|---|
| throwaway worktree created | `git worktree add --detach <scratch>/spike4/wt HEAD` at `7eede74f` |
| all 80 file edits confined to it | yes; captured as `spike4/specta-experiment.diff` (154,952 bytes) before teardown |
| `git add`, `git commit`, `git stash` used | never |
| worktree removed | `git worktree remove --force` succeeded; directory gone |
| `git worktree list` | shows only `/home/claude/.ref/agents-in-a-box` (main) and `/home/claude/.ref/agents-in-a-box-desktop-app-part2` (desktop-app-part2) |
| `git status --short` in the main worktree | empty |
| stash stack | 0 entries, same as at start |
| `HEAD` | `7eede74f`, unchanged |
| cargo target dir | `<scratch>/spike4/target`, never the repo's; deleted after the run |
| system-wide installs | none; chromium went to `<scratch>/spike4/pw-browsers`, node_modules to `<scratch>/spike4/fanout` |
