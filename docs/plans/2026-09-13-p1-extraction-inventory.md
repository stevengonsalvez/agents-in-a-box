# P1 extraction inventory: `ainb-app`

Status: P1a and P1b merged, P1c implemented. Base spec: `2026-09-04-desktop-shared-core-spec.md`, rows P1 to P5 of "Extraction plan". Goal: `goals/2026-09-13-p1-ainb-app.md`.

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

"297 tests under `app/`" counts every test in the directory. 16 of them stay with the renderer because they build a ratatui `Frame`, a crossterm event or `UiState`; the P1c section lists them.

## P1a: service layer (#973)

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

## P1b: component pure halves (#975)

Each screen gets one commit. In each component file, the types and the logic that does not draw move to `ainb-app/src/components/<same path>`. The draw functions stay at the old path, and the core file starts with `pub use ainb_app::components::<path>::*;`.

The leak replacements are the only logic changes, and each one is listed below. Everything else is text moved as-is, plus two kinds of edit: `pub(crate)` and private items widened to `pub` where the core renderer reads them, and test-only imports placed inside test modules. `crates/ainb-app/tests/renderer_free.rs` (from P1a) enforces the boundary on every commit.

| Screen or module | Moved to `ainb-app` (LOC now there) | Leak replaced | Stays in `ainb-core` |
|------------------|-------------------------------------|---------------|----------------------|
| `onboarding` | `state.rs` whole (1,261) | none | wizard renderer |
| `skills` | `SkillsViewState`, provider/tab enums, query filters (260) | none | renderer |
| `setup_menu` | `SetupMenuItem`, `SetupMenuState` (142) | none | renderer |
| `config_popup` | `ConfigPopupState`, value/type enums, editing logic (333) | none | renderer |
| `skill_manager_screen` | `SkillsScreenData`, browse/library/preview/input states, discovery banner, selection reducers (1,460) | none | renderer and its tests |
| `daemons` | `DaemonsState`, collector, action runner (851); `cli::daemon` (834); `cli::fleet::daemons` (296); `fleet::daemon_cta` (277) | none | renderer; the daemon argv test, which needs the clap tree |
| live log pipeline | `log_parser` (393), `LogEntry` half of `live_logs_stream` (240), `log_reader` (739), `log_writer` (314), `widgets` whole (8,272), `docker::log_streaming` (934) | `LogLevel::color` becomes `log_parser::level_color` in core. The widgets' `crossterm::terminal::size` read becomes `ainb_app::viewport::columns()`, which the TUI publishes at startup and on resize. | `LiveLogsStreamComponent`, formatters |
| `session_tabs` (log rows) | `LogRow`, `log_rows`, `log_detail` (57); `fleet::session_log` (575) | none | tab renderers |
| `home_screen_v2` | `HomeScreenV2State`, focus, click outcome (189); `sidebar` state and `item_index_at` (256); `welcome_panel` state (101); `mascot` animation (157) | `last_sidebar_rect: Option<Rect>` becomes `Option<Area>`, converted in `publish_after_draw`. `item_index_at` takes `Area` and moves from `SidebarComponent` to a free fn beside `SidebarState`. | renderers |
| `log_history_viewer` | `LogHistoryViewerState`, focus/filter/selection types, session summaries (650) | `session_list_state: ListState` becomes `selected_session: Option<usize>`; the renderer already kept its own `ListState` in `UiState` and synced only the index. `log_entries_area: Option<Rect>` becomes `Option<Area>`. | renderer |
| `git_view` + `code_review` | `GitViewState` and its file tree/markdown/commit types (1,257); `code_review::model` (97) and `parse` (318) whole; `CodeReviewUi` and its keyboard/mouse helpers (478) | `GitFileStatus::color` becomes `git_view::status_color` in core. `CodeReviewUi.sidebar_rect: Cell<Rect>` becomes `Cell<Area>`. | draw fns, `code_review::highlight` |
| `changelog` | `ChangelogState`, markdown line types, embedded `CHANGELOG.md` (259) | none | renderer |
| `new_session` | `ConfigureState`, branch picker, preset and base selections, `LaunchSpec` (766); `PickRepoState` and rows (347); `TextEditor` out of `app::state` into `ainb_app::text_editor` (315) | none | renderers, dispatcher, key handlers (these take `KeyEvent` until P1c) |
| `cli::statusline_install` | whole (614) | none | nothing |

New renderer-agnostic seams introduced by P1b:

| Seam | Where | Why |
|------|-------|-----|
| `viewport::{set_columns, columns}` | `ainb-app/src/viewport.rs` | Width-dependent text layout without asking a terminal library |
| `geometry::Area` | `ainb-app/src/geometry.rs` | State that hit-tests clicks against where the renderer last drew |

Deferred to P1c, because these reach `AppState`, which does not move until then:

- `session_recovery`: `SessionRecoveryState`'s impl calls `AppState::find_latest_transcript`.
- `session_tabs`: `SessionTab`, `cycle`, `resolve` and `selected_blocking` take `&AppState`. `SessionTab` has inherent methods that do so, and an inherent impl must live in its type's crate.
- `configure::handle_key` and `pick_repo::handle_key`: they move once they take `Chord`.

## P1c: state machine (this PR)

The commits, in order:

| Commit | What it does |
|--------|--------------|
| terminal handoff | `ainb_app::host::TerminalHandoff` with `release_terminal` and `reclaim_terminal`; the TUI installs `CrosstermHandoff` at startup. `run_oauth_setup` and `docker::exec_interactive_blocking` (now in `ainb-app`) hand the terminal over through it instead of calling crossterm. The core `docker` shim is gone. One behaviour change: the docker path used to leave only raw mode and the alternate screen. It now also releases mouse capture and bracketed paste, as OAuth setup always did, so a `docker exec -it` child no longer receives the TUI's mouse and paste escape sequences. |
| state machine move | The files in the table below, plus the seams the move forces (next table). |
| pure tests follow their code | `screen_ids_are_unique` and `plugin_id_for_screen_resolves_analytics` move beside the ids and routing fns they test. |
| width from the host | `RendererHost::columns()` replaces the process-global `viewport` from P1b. |
| Intent | `Intent`, `CommandId`, `Pos`, `Btn`, `dispatch`, `EventHandler::resolve_intent`, the command registry, and the root re-exports. |
| unbound commands | `Binding.chord` becomes `Option<Chord>`. An unbound row is a command that a palette or click runs by name, and no key reaches it until a `keymap.toml` override binds one. |

| Moved to `ainb-app/src/app/` (LOC now) | Stays in `ainb-core/src/app/` |
|----------------------------------------|-------------------------------|
| `state.rs` (14,691), `events.rs` (8,622), `state_tests.rs` (4,492), `keymap.rs` (1,236), `keymap_defaults.rs` (1,221), `sections.rs` (784), `session_loader.rs` (276), `versioned.rs` (219), `snapshot.rs` (193), `event_bus.rs` (130), `keymap_toml.rs` (75); `screens::{ids, ScreenId, EventOutcome}` and the plugin routing reads from `screens/builtin.rs` | `ui_state.rs`, `attach_handler.rs`, the `Screen` trait, `registry.rs`, the plugin screen renderer and key/mouse forwarders in `screens/builtin.rs`; new: `mouse.rs` (359), `terminal_keys.rs` (85) |
| Component halves deferred from P1b: `session_recovery` state (1,411), `session_tabs` (305), `configure::handle_key` and helpers (672), `pick_repo::handle_key` and helpers (294) | their renderers |

Seams the move forces (a type in `ainb-app` cannot name one in `ainb-core`, and an inherent impl must live in its type's crate):

| Seam | Where | Replaces |
|------|-------|----------|
| `Chord` as a structured key: `Key`, `Mods`, `Chord::new`, `code()`, `modifiers()` | `ainb_app::app::keymap` | `Chord::from_key_event(&KeyEvent)`. Handlers take `Chord`: `handle_key_event`, `handle_key_event_with_keymap`, `handle_new_session_keys`, `configure::handle_key`, `pick_repo::handle_key`. |
| `terminal_keys::chord_from_key_event` | `ainb-core/src/app/terminal_keys.rs` | The only crossterm key conversion left. It returns `None` for keys the keymap cannot spell (`Null`, media keys, lone modifiers). The old converter panicked on those, for example Ctrl+Space, which crossterm reports as `KeyCode::Null`. |
| `RendererHost` (`queue_scroll`, `statusline_status`, `columns`, `pointer`), `NoRenderer` | `ainb_app::app::events` | `&mut UiState` in key dispatch. `UiState` implements it. |
| `SessionsPaneHitTest` | `ainb_app::app::state` | `&SessionsPaneState` in the session-list mouse reads |
| `PluginViewports` | `ainb_app::app::screens` | the two plugin geometry maps `tick_plugin_renders` read from `UiState` |
| Mouse reducers | `ainb-core/src/app/mouse.rs` | `EventHandler::handle_mouse_event` and its hit-test helpers, which read the rects `UiState` records |

`crates/ainb-core/build.rs` scans `tick_plugin_renders` for `.await` at the file's new path. A missing scan file now fails the build; before, it passed without checking anything.

Tests from the 297:

| Where | Count | Which |
|-------|------:|-------|
| `ainb-app` | 281 | `state_tests.rs` 148, `events.rs` 77, `state.rs` 42, `versioned.rs` 5, `event_bus.rs` 4, `keymap.rs` 2, `session_loader.rs` 1, `screens/mod.rs` 1, `screens/builtin.rs` 1 |
| `ainb-core` | 16 | `screens/builtin.rs` 10 (the plugin screen renderer, crossterm key translation, `register_builtins`), `registry.rs` 3 (they implement `Screen` with a ratatui `Frame`), `ui_state.rs` 1, `attach_handler.rs` 1, `mouse.rs` 1 (the menu-bar click reads `UiState`, moved out of `events.rs`) |

New tests:

| Test | Proves |
|------|--------|
| `ainb-core/tests/intent_key_parity.rs` | For all 525 default rows, the crossterm key a terminal sends converts to the row's chord, matching the pre-split converter (kept in the test as the oracle). `Intent::Key` resolves to the same event and leaves the same section versions as that key path. 155 rows reach their own event from a fresh state; the rest need an overlay open and match on both paths. |
| `ainb-app/tests/intent_dispatch.rs` | A `Command` intent (`global.help`) bumps only `Shell`. A `Text` intent into the config popup bumps only `Config`. A pointer with no renderer and an unknown command bump nothing. Every row is a registered command. Intents round-trip through JSON. |
| `ainb-app/tests/host_width.rs` | Two hosts at 80 and 200 columns clamp the same resize command to their own widths. |
| `intent_dispatch.rs` `an_unbound_row_is_a_command_no_key_reaches_until_an_override_binds_it` | An unbound row is listed by `commands()`, runs through `Intent::Command` and bumps only its section. No key resolves to it until an override binds it. |

Decisions:

- **`dispatch` and `resolve_intent`.** `dispatch(state, keymap, host, intent)` applies an intent. The TUI calls `resolve_intent` instead, because it handles a few resolved events against its own layout first (embed sizing, sidebar collapse, the immediate tick after `NewSession`). `AppEvent` stays public so that host can match on it. Renderers that have nothing of their own go through `dispatch`.
- **`CommandId`.** Spelled `<context>.<row id>`, the same pair a `keymap.toml` override names. One pre-existing clash, `session_list.restart` (`r` resume and `e` restart), resolves to the first row, as an override does. Renaming it would change `docs/tui/keyboard-shortcuts.md`, so it is tracked in #976 and pinned by the registry test.
- **Pointer intents.** `Mouse(Pos, Btn)` goes to `RendererHost::pointer`, because only the renderer knows what it drew where. The wheel is not an intent: scroll position belongs to the renderer. Drags, hovers and the log-history and code-review click paths stay in the TUI's mouse loop.
- **Surface width.** The only width reads left in `ainb-app` were the Skill Manager `[`/`]` clamps. They are now `UiAction` rows resolved on the host path, and the generated shortcut docs are unchanged. On screen open, only the minimum width is applied; the renderer already clamps the maximum at draw, and a step clamps the current width before moving. The log separator is baked into log entries that every attached surface shares, so it is laid out to a fixed 80 columns, the fallback it used before a host published a width.
- **No `Serialize` on state in P1c.** A first cut derived `Serialize` on `AppState` and its sections, with `typescript-bindings` on the contract types. A security review found 47 credential-bearing or private fields reachable from that derive. Examples: bot tokens in `FleetConfig.bridge`, `env` maps, tmux scrollback in `Session.preview_content`, and typed key buffers. Both commits were dropped. The mirror phase adds serialisation behind a redaction layer, tracked in #983. Only the intent wire types (`Intent`, `Chord`, `CommandId`, `Pos`, `Btn`) derive serde.
- **Unbound commands.** A palette must list commands that have no key. `Binding.chord` is `Option<Chord>`, and `Keymap::new` indexes only bound rows. Every built-in row stays bound, so `keyboard-shortcuts.md` does not change. `ainb keymap list` prints `unbound` in Markdown and `null` in JSON.

### Left for P2 to P5

The reducer in `ainb-app` still performs these side effects directly. P2 to P5 turn each one into an `Effect` the host executes:

| Side effect | Where | Count |
|-------------|-------|------:|
| `std::process::Command` spawns: `tmux` 14, `docker` 8, `git`, `gh`, `sh` 1 each (a `ps` in a test module is not counted) | `app/state.rs` 23, `app/events.rs` 2 | 25 |
| Terminal handoff for OAuth setup and `docker exec -it` | `run_oauth_setup`, `attach_to_container` (through `host::release_terminal`) | 3 |
| OSC 52 clipboard write (copy a run command) | `app/events.rs` | 1 |
| Live embed attach and detach (tmux client) | `enter_interactive_pane`, `release_interactive_pane`, `poll_embed_exit` | 3 fns |

Seams P1c added that P2 reshapes (from the #982 design review):

| Seam | Gap | P2 direction |
|------|-----|--------------|
| `RendererHost::pointer` | Returns `Option<AppEvent>` and takes `&mut AppState`, so a host has to name the internal event enum and mutates state outside the reducer. `Pos` is a terminal cell. | Return `Option<Intent>`, or a `CommandId` from the hit-test. `AppEvent` then stops being public. |
| `Intent::Command(_, Args)` | `resolve_intent` drops `Args`. Payload rows bake the payload into the row, so `AttachSessionByPosition(3)` is one command per index. | Honour `Args` for payload actions, or reject anything but `Null`. |
| `host::TERMINAL` | A process-global `OnceLock` where the first writer wins. A desktop-triggered OAuth setup would release the terminal host's modes. | Move the handoff onto `RendererHost`, or key it per host. |
| `RendererHost::statusline_status` | A cached filesystem probe sitting in a renderer trait. | Split it into a host service beside `TerminalHandoff`. |
| `RendererHost::columns` | The one consumer clamps a width stored in shared `AppState` and persisted to `ui_preferences`. `host_width.rs` uses two separate states, so it does not cover two hosts sharing one. | Keep per-host width host-side, and test one state with two hosts. |

These state types still live in `ainb-core`, because only the terminal renderer uses them:

| File | Types |
|------|-------|
| `tmux_preview.rs` | `TmuxPreviewPane`, `PreviewMode` |
| `slash.rs` | `SlashPalette`, `SlashCommandRegistry`, `SlashAction` and the built-in commands |
| `fuzzy_file_finder.rs` | `FuzzyFileFinderState`, `FileMatch` |
| `action_card.rs` | `ActionCardGridState`, `ActionCard`, `ActionCardId` |
| `claude_chat.rs` | `ConnectionStatus`, `ClaudeConnectionStatus` |
| `app/ui_state.rs` | `UiState`: scroll, hover, pane rects, plugin geometry |

The session list, fleet panel, new-session, daemons, git view, code review, recovery, Skill Manager, log history and config states already live in `ainb-app`, from P1b and P1c. Their renderers and the renderer-local fields in `UiState` stay in core.
