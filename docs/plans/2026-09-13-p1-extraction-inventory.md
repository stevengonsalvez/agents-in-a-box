# P1 extraction inventory: `ainb-app`

Status: P1a implemented, P1b and P1c planned. Base spec: `2026-09-04-desktop-shared-core-spec.md`, rows P1 to P5 of "Extraction plan". Goal: `goals/2026-09-13-p1-ainb-app.md`.

This is the checklist reviewers hold each P1 PR against. Every row names a module, its size, what in it touches ratatui or crossterm, where it ends up, and the shim that keeps `ainb::<path>` and `crate::<path>` resolving in `ainb-core`.

## Shape

```
┌──────────────┐ pub use ainb_app::*  ┌──────────────────────────────┐
│ ainb-core    │─────────────────────▶│ ainb-app                     │
│ ratatui      │                      │ no ratatui, no crossterm     │
│ draw fns     │ local shim modules:  │ services (P1a)               │
│ UiState      │ app cli components   │ component pure halves (P1b)  │
│ tripwires    │ docker fleet tmux    │ AppState, Intent (P1c)       │
│ main.rs      │ widgets test_support │                              │
└──────────────┘                      └──────────────────────────────┘
```

A shim is a module in `ainb-core` with the same name as one in `ainb-app`. It starts with `pub use ainb_app::<module>::*;` and then declares only the files that still touch the terminal. A local module shadows the glob import of the same name, so paths do not change for any caller.

## Corrections to the spec

The spec's P1 row and "Hard knots" line were written before a module graph existed. Three figures in them do not hold:

| Spec claim | Measured | Why it was off |
|------------|----------|----------------|
| ~41k LOC move in P1 | ~120k LOC | `AppState` holds handles from config, models, docker, tmux, fleet, interactive and claude, and those modules reach back into `cli` and `components`. Their whole closure has to sit at or below `ainb-app`. P1a alone moved 82.6k lines. |
| crossterm in ~130 handler signatures | 3 in `events.rs` (`handle_key_event`, `handle_key_event_with_keymap`, `handle_new_session_keys`) plus 2 component reducers (`configure::handle_key`, `pick_repo::handle_key`) | Dispatch already resolves through `Chord` since Phase 1 keymap. The other `KeyEvent` uses are test constructors (36 in `events.rs`). |
| 17 component fields on `AppState` can wait for P2 to P5 | They cannot | A type in `ainb-app` cannot name a type in `ainb-core`, which depends on it. A generic seam (`AppState<X>`) would need about 560 trait touchpoints in `events.rs`, and the moved tests could not build without core types. P1b moves the pure half of each component file (types, impls, reducers) and leaves render fns in core, which is the P2 to P5 "cut at the first render fn", done earlier and without moving any render code. |

"297 tests under `app/`" counts every test in the directory. 13 of them are in files that stay in the renderer: `ui_state.rs` (1), `attach_handler.rs` (1) and `screens/builtin.rs` (11).

## P1a: service layer (this PR)

Zero logic change. Files moved with `git mv`; the only text edits are import paths, `pub(crate)` widened to `pub` where `ainb-core` reaches in, and `#[cfg(test)]` helpers that core tests use widened to `#[cfg(any(test, feature = "test-support"))]`.

| Module | LOC now in `ainb-app` | ratatui / crossterm inside | Target | Shim in `ainb-core` |
|--------|----------------------:|----------------------------|--------|---------------------|
| `agent_parsers` | 953 | none | ainb-app | root glob |
| `agents` | 319 | none | ainb-app | root glob |
| `audit` | 242 | none | ainb-app | root glob |
| `claude` | 907 | none | ainb-app | root glob |
| `clipboard` | 75 | none | ainb-app | root glob |
| `config` | 12,797 | none | ainb-app | root glob |
| `credentials` | 321 | none | ainb-app | root glob |
| `docker` | 3,726 | `container_manager.rs:880` suspended raw mode for `docker exec -it` | ainb-app | `docker/mod.rs`: keeps `log_streaming.rs` (934) and `exec_interactive_blocking` |
| `docs` | 59 | none | ainb-app | root glob |
| `editors` | 94 | none | ainb-app | root glob |
| `fleet` | 24,598 | none | ainb-app | `fleet/mod.rs`: keeps `daemon_cta.rs` (277) and `session_log.rs` (575) |
| `git` | 5,746 | none | ainb-app | root glob |
| `headroom` | 763 | none | ainb-app | root glob |
| `interactive` | 5,627 | none | ainb-app | root glob |
| `mcp_pool` | 2,706 | none | ainb-app | root glob |
| `models` | 7,489 | none | ainb-app | root glob |
| `otel` (+ `assets/otel`) | 799 | none | ainb-app | root glob |
| `perf` | 211 | none | ainb-app | root glob |
| `plugins` | 899 | none | ainb-app | root glob |
| `providers` | 441 | none | ainb-app | root glob |
| `rtk` | 285 | none | ainb-app | root glob |
| `self_exec_guard` | 113 | none | ainb-app | root glob |
| `setup` | 2,202 | none | ainb-app | root glob |
| `tmux` | 2,190 | `embed_input.rs` translates crossterm key and mouse events | ainb-app | `tmux/mod.rs`: keeps `embed_input.rs` (704) |
| `usage_cache` | 1,167 | none | ainb-app | root glob |
| `test_support` | 386 | none | ainb-app, behind `test-support` | `test_support.rs`: keeps `cli_usage_report_json` |
| `cli::{statusline, codex_statusline, deps, update, util}` | 3,199 | none | ainb-app `cli/` | `cli/mod.rs` glob |
| `cli::OutputFormat` | 10 | none | ainb-app `cli/mod.rs` | `cli/mod.rs` glob |
| `cli::hangar` daemon lifecycle (start, stop, restart, prune, pid ownership) | 4,249 incl. its tests | none | ainb-app `cli/hangar.rs` | `cli/hangar/mod.rs` glob |

### Back-edges cut in P1a

| Edge | Resolution |
|------|------------|
| `config` → `app::state::{SessionFilter, ConfigCategory, ConfigSetting, ConfigValue, SecretValue}` | Types moved verbatim to `ainb_app::config::settings_model`; `app/state.rs` re-exports them. |
| `docker::container_manager` → crossterm | `exec_interactive_blocking` (its only caller is `app/state.rs`, and it never read `self`) became a free fn in the core `docker` shim. |
| `docker::log_streaming` → `components::{live_logs_stream, log_parser}`, `widgets::MessageRouter` | File stays in core until P1b moves those pure halves. |
| `fleet::daemon_cta` → `components::daemons`, `cli::daemon` | File stays in core until P1b. |
| `fleet::session_log` → `components::session_tabs::log_rows` | File stays in core until P1b. |
| `fleet::daemons::probe`, `interactive::session_manager` → `cli::hangar` | The daemon lifecycle closure moved to `ainb_app::cli::hangar`, found by a transitive item scan from `ensure_hangar_daemon`, `daemon_runtime_status` and `argv_credits_a_daemon`. Its tests moved with it, except the three that parse argv through the clap tree, which stay in core as `prune_argv_tests`. |
| `models::live_window` → `cli::{statusline, codex_statusline}`; `setup` → `cli::deps`; `fleet::daemons::probe` → `cli::update`; `interactive` → `cli::util` | Leaf files moved whole. |
| `main.rs` re-declared every module with `mod` | Separate commit: the binary imports the `ainb` library. |

### Non-code paths touched

| File | Change |
|------|--------|
| `.github/workflows/ci.yml:762` | `cargo test -p ainb --lib capture_failed_launch_pane_reads_a_dead_pane` becomes `-p ainb-app`. The test moved with `interactive::session_manager`. The main test job already runs `--workspace`. |
| `docs/assets/diagrams/generate-diagrams.py` | Reads `fleet/daemons/probe.rs` from the new path. |
| `plugins/README.md`, `ainb-tui/scripts/chat-bus-smoke.sh` | Path references updated. Historical research and plan docs keep the paths they were written against. |

## P1b: component pure halves (planned)

One commit per screen. In each file, the types, their impls and the reducers move to `ainb-app/src/components/<same path>`. The render fns stay at the old path and start with `pub use ainb_app::components::<path>::*;`. Where a pure half leaks a ratatui type, it is replaced as follows: `Rect` becomes a crate-owned `Area { x, y, width, height }`, `ListState` becomes `Option<usize>`, and fns that return a `Color` stay in core as free fns.

| File | Leak in the pure half | First render fn (line) | Pure LOC (approx) |
|------|-----------------------|------------------------|------------------:|
| `session_recovery` | none | 1438 | 1,420 |
| `git_view` + `code_review::{model, parse}` | `GitFileStatus::color`, `Cell<Rect>` sidebar rect | 1300 | 2,180 |
| `daemons` | none (14 private fields read by render and tests) | 892 | 880 |
| `skills` | none | 283 | 270 |
| `skill_manager_screen` | none | 787 | 1,500 |
| `onboarding/state` | none, already a separate file | n/a | 1,261 |
| `setup_menu`, `config_popup`, `changelog` | none | 170, 359, 281 | 765 |
| `log_history_viewer` + `log_reader`, `log_writer` | `ListState`, `Rect` | 690 | 1,240 |
| `home_screen_v2` + `sidebar`, `welcome_panel`, `mascot` state | `Rect` in `last_sidebar_rect` | 256 | 775 |
| `session_tabs` | none (`strip`, `footer` stay) | 354 | 410 |
| `live_logs_stream`, `log_parser` | `LogLevel::color` | 78, none | 565 |
| `new_session::{pick_repo, configure}` | `handle_key(KeyEvent)` becomes `handle_key(&Chord)` | 423, 821 | 2,185 |
| `widgets` (`MessageRouter`) | `crossterm::terminal::size` at `widgets/mod.rs:250` becomes a width argument | n/a | 8,273 |
| core-held `docker::log_streaming`, `fleet::{daemon_cta, session_log}` | none once the halves above land | n/a | 1,786 |

## P1c: state machine (planned)

| Moves to `ainb-app/src/app/` | Stays in `ainb-core/src/app/` |
|------------------------------|-------------------------------|
| `state.rs`, `sections.rs`, `versioned.rs`, `events.rs`, `keymap.rs`, `keymap_defaults.rs`, `keymap_toml.rs`, `session_loader.rs`, `snapshot.rs`, `event_bus.rs`, `state_tests.rs`, `screens::ids`, `EventOutcome` | `ui_state.rs`, `attach_handler.rs`, `screens/builtin.rs` (minus `plugin_id_for_screen`, `focused_plugin_captures_text`, `plugin_owns_help_keys`, which are state reads and move), the `Screen` trait and `registry.rs` (they hold `Frame` and `Rect`) |

P1c also covers:

- **Keys.** `Chord::from_key_event` becomes a free fn in core, at the host edge. Handlers take `Chord`.
- **Terminal size.** The eight `crossterm::terminal::size` reads in `events.rs` become a viewport the host passes in.
- **Mouse and terminal suspend.** Mouse fns (`events.rs` 805 to 1210) become a core extension trait. `run_oauth_setup` suspends the terminal through a host hook.
- **Serde.** `AppState` and all 19 sections get `Serialize`, with `#[serde(skip)]` on receivers, handles and join handles. specta sits behind `typescript-bindings`, off by default.
- **Intent.** `Intent` (`Key(Chord) | Command(CommandId, Args) | Mouse(Pos, Btn) | Text(String)`), `CommandId`, `dispatch`, and the command registry derived from the keymap table.
- **Guard tests.** A manifest test fails if ratatui or crossterm is ever added to `ainb-app`.
- **Keymap parity test.** Every row in `keymap_defaults.rs` resolves from `Intent::Key` to the same `AppEvent` the old `KeyEvent` path produced.
- **Section-version tests.** A `Text` intent and a `Command` intent each bump exactly one section version.
- **Programme doc.** The P1 row flips in `2026-09-12-desktop-programme.md`.
- **Build lint.** `build.rs` scans `src/app/state.rs` for `.await` in render-path fns. That scan follows the file or is dropped with a stated reason.
