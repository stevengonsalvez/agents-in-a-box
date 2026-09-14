# P1 extraction inventory: `ainb-app`

Status: P1a, P1b and P1c merged; P2 implemented (section "P2" below). Base spec: `2026-09-04-desktop-shared-core-spec.md`, rows P1 to P5 of "Extraction plan". Goal: `goals/2026-09-13-p1-ainb-app.md`.

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
- **Unbound commands.** A palette must list commands that have no key. `Binding.chord` is `Option<Chord>`, and `Keymap::new` indexes only bound rows. Every built-in row stays bound, so `keyboard-shortcuts.md` does not change. `ainb keymap list` prints `null` for an unbound chord in JSON; since P3 the Markdown lists bound rows only, because the pointer, report and plugin action rows are unbound.

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

## P2: sessions and effects

Goal: `goals/2026-09-13-p2-p5-effects-and-hosts.md`, first of four staged PRs. Two overrides from the orchestrator apply: no `Serialize` on any section or moved type (#983), and P2 closes the P1c seams in the table above.

### Effect

The reducer queues host work on an outbox that is not a section, so queuing bumps no version. `dispatch` and `App::tick` drain it and return `Vec<Effect>`; the TUI host runs each effect after the step that queued it has written state (`ainb-core/src/effect_host.rs`). Every variant's doc comment says which host executes it and what that host does when it cannot.

| Variant | Produced by | Terminal host | On failure |
|---------|-------------|---------------|------------|
| `AttachTerminal(Session(id))` | `a`, a double-click on a session row, the `1`-`9` rows | suspends, attaches the session's tmux, resumes | notice naming the target |
| `AttachTerminal(InPlace)` | `A` | sizes and enters the preview's interactive embed | notice when the row has no tmux or the attach fails |
| `AttachTerminal(Tmux(name))` | Other tmux and SSH rows, the daemons menu | full-screen attach | notice naming the target |
| `AttachTerminal(Tool(Witr / Abtop / AbtopWithSetup))` | `w`, `t`, the sidebar tiles, the abtop setup confirm | runs the tool in its own tmux session and attaches | notice "Failed to open" with the error |
| `AttachTerminal(WorkspaceShell { .. })` | quick shell | creates or reuses the workspace shell, then attaches | notice |
| `AttachTerminal(ClaudeLogin { auth_dir, image })` | the OAuth setup async action, once Docker is ready | leaves its input modes, runs the auth container on the tty, reports the exit to `AppState::finish_oauth_login` | a child that cannot start reports a failed exit; the reducer shows "Authentication failed" |
| `Detach` | Ctrl+Q while interactive | releases the embed to the read-only preview | no live terminal is a no-op |
| `OpenEditor(path)` | `o`, the session context menu | `preferred_editor`, else `code`, else `$EDITOR`, detached | notice saying how to set an editor |
| `PasteClipboard` | Ctrl+V on the repo picker and in a config text popup | reads the clipboard with `arboard`, dispatches `Intent::Text` | notice "Could not read clipboard"; nothing dispatched |

Additions to the spec's enum, each proven by the call site it replaced: the `TerminalTarget` variants beyond a session id (`InPlace`, `Tmux`, `Tool`, `WorkspaceShell`, `ClaudeLogin`) and `PasteClipboard`. `Notify`, `Clipboard` and `OpenUrl` land with their first producers: onboarding's OSC 52 copy and the welcome and log-history copies in P5, the daemons URL in P3. The unreachable `AttachToContainer` path and `docker::exec_interactive_blocking` are deleted, not converted.

### Seams closed

| P1c seam | P2 |
|----------|----|
| `RendererHost::pointer` returned `Option<AppEvent>` from `&mut AppState` | `pointer(&mut self, &AppState, Pos, Btn) -> Option<Intent>`. The TUI hit-test (`mouse::press`) answers with a keymap command (menu bar, filter glyph) or a pointer command naming what was under the pointer by its place in state: `session_list.select_row {row, open}`, `open_row_menu {row}`, `focus_pane {pane}`, `save_pane_layout {width, collapsed}`, `skill_manager.all_sources`, `select_source {index}`, `select_unit {position}`, `focus_pane {pane}`, `save_sources_width {width}`, `home.begin_sidebar_resize`, `home.click_sidebar_item {index}` (`ainb_app::app::pointer`). Drags, releases and hovers are `mouse::gesture`, which returns the command that saves a finished resize. |
| `AppEvent` public | The `events` module, `AppEvent` and `EventHandler` are public only under the `test-support` feature, for integration tests that assert on resolved events. Hosts use `dispatch`, `RendererHost`, `NoRenderer` and three free queries (`is_in_text_input_context`, `skill_manager_overlay_open`, `slash_command_intent`). The run loop matches on no reducer event: keys, pastes and slash commands go through `dispatch`, the deferred pending event through `AppState::apply_pending_event`. `renderer_free.rs` fails if any normal dependency enables `ainb-app/test-support`. |
| `Args` dropped | `KeyAction::with_args` replaces the payload of a row that carries one (a session position, a step, an agent, a character) from `Args` of the matching JSON type. `Null` runs the row as written; anything else is refused and the command does not run. Pointer commands parse their own payloads the same way. |
| `host::TERMINAL` process global | Deleted with `ainb_app::host`. Its one caller, the OAuth login, is `AttachTerminal(ClaudeLogin)`, so only the host that owns a terminal ever releases its modes. |
| `RendererHost::statusline_status` | `AppState::statusline_status` over a `StatuslineProbe`: a TTL cache behind a lock beside the effect outbox, readable through `&AppState` without writing a section. The reducer invalidates it after the `W` install. |
| `RendererHost::columns` and a shared clamped width | Gone. The Skill Manager Sources width and its divider drag live in the TUI's `UiState` (`SkillSourcesPane`), starting from the saved preference. `[` and `]` resolve to `HostAction::ShrinkSkillSources` / `GrowSkillSources`, which the host steps against its own width and then saves. `ainb-core/tests/host_width.rs` draws one `AppState` from two hosts at 80 and 200 columns: a step on either moves only its own divider. |

`RendererHost::queue_scroll(ScrollAction)` became `queue(HostAction)` with `Scroll`, `ToggleSessionsSidebar`, `GrowSkillSources` and `ShrinkSkillSources`, so the sidebar toggle stopped being a reducer event the host intercepted.

### Guards

| Test | Fails when |
|------|------------|
| `ainb-app/tests/host_side_effects.rs` dependency check | A clipboard, browser-open, editor or terminal-attach crate becomes reachable through normal dependencies for any target (the `cargo metadata` resolve walk, so target-gated and renamed dependencies count), or one of the three still listed stops being reachable: `arboard` and its Windows backend `clipboard-win` (welcome and log-history copies, P5) and `portable-pty` (the preview embed's tmux client) |
| `ainb-app/tests/host_side_effects.rs` source walk | A module's count of `Command::new(`, `CommandBuilder::new(`, `arboard`, `copy_osc52(`, `webbrowser` or `open::that` lines, outside `#[cfg(test)]` modules and comments, differs from its allow-list entry. Each entry carries its reason; the list is a ratchet both ways. It is a line fence, not a call-graph check: a helper in an allow-listed module can be called from anywhere, and a spawn spelled through an alias or macro is not seen. |
| `ainb-app/tests/renderer_free.rs` | ratatui or crossterm is reachable through normal dependencies, or a normal dependency enables `test-support` |
| `ainb-app/tests/effects.rs` | One test per effect kind: the intent that asks for host work returns exactly that effect and moves exactly the expected section versions |

### Behaviour changes

| Change | Why |
|--------|-----|
| The immediate tick after a key now follows a key that closes a confirmation dialog with async work queued, instead of after `NewSession`, `SearchWorkspace` or `ConfirmationConfirm` | The host no longer sees the event. `NewSession` and `SearchWorkspace` have queued no async work since the new-session redesign, and the next loop iteration repaints them anyway. |
| After the OAuth login the host clears the terminal before the next frame | The screen is re-entered, and ratatui's diff would otherwise keep a blank buffer. |
| `B` resolves to a renderer-local `UiAction` | The keymap golden (`tests/fixtures/keymap_rows.txt`) records `Ui(ToggleSessionsSidebar)`; `keyboard-shortcuts.md` is unchanged because the Markdown prints no action kind. |
| A Sources resize on one surface saves the preference, which a surface that has not resized starts from; it no longer moves another surface's panel | Width is per host. |
| A press on the home sidebar off its resize edge no longer clears the hover flag | The flag follows pointer moves already; the press path only reads state now. |

### Screens

`fleet_panel` and `inbox` are N/A: v2 deleted both host screens (`f80512864` "delete the host Fleet panel", `62dedf28e` "delete the host notifyd Inbox"), and `ainb-app/src/app/sections.rs` keeps the inbox section empty on purpose. The other states the goal lists were already in `ainb-app` with their reducers after P1c.

### Left for P3 to P5

| What | Where | Step |
|------|-------|------|
| OSC 52 copy of the installer command | `app/events.rs`, `clipboard.rs` | P5 `Effect::Clipboard` |
| `arboard` copies | `components/welcome_panel.rs`, `components/log_history_viewer.rs` | P5 `Effect::Clipboard` |
| Daemon start and stop verbs | `components/daemons.rs` | done in P3 (`Effect::RunDaemonAction`); there is no daemons URL on v2, so `OpenUrl` has no producer yet |
| Detached tmux recreation in recovery | `components/session_recovery.rs` | P5 |
| Home sidebar geometry, hover and resize drag in `AppState` | `components/home_screen_v2.rs`, `ainb-core/src/app/mouse.rs` gestures | P5, the same move the Sources pane made |
| Log-history and code-review click paths, wheel scrolling of home, git view and log history | `ainb-core/src/main.rs` mouse loop | P4 (code review), P5 |
| `Pos` is a terminal cell | `ainb_app::app::intent` | the desktop host names what it hit through pointer commands instead |

## P3: hangar host, daemons, and the P2 design review

Goal criteria 2 and 3a (#1002). `Notify`, `Clipboard` and `OpenUrl` still have no producer; the spec's `Effect` row now says each ships only with one and a test.

### Report path

A host never writes state. What its work changed comes back as a report command it dispatches like any other intent (`ainb_app::app::reports`, unbound `global.*` rows). The TUI executor takes `&AppState` and returns the reports; `main.rs::run_effects` dispatches each effect's reports before the next effect runs. Work that outlives the effect (a daemon verb) queues its report for the top of the run loop.

| Effect | Report | Reducer applies |
|--------|--------|-----------------|
| `AttachTerminal(Session)` | `attach_finished {session, detached / failed / target_missing}` | detached mark; failure notice; a missing target stops the session and reloads rows |
| `AttachTerminal(Tmux)` | `attach_finished {tmux, ..}` | failure notice; reloads the other tmux rows |
| `AttachTerminal(Tool)` | `abtop_setup_finished {ok}`, `attach_finished {witr / abtop, not_installed / ..}` | install or failure notices |
| `AttachTerminal(WorkspaceShell)` | `shell_prepared {workspace, ready {created, cd} / failed}`, `attach_finished` | working dir, created notice, cd warning or error |
| `AttachTerminal(InPlace)` | `in_place_sized {rows, cols}` | enters the interactive pane |
| `AttachTerminal(ClaudeLogin)` | `login_finished {auth_dir, exited_ok}` | `finish_oauth_login` |
| `Detach` | `detached` | releases the pane |
| `OpenEditor` | `editor_finished {opened / none_found / failed}` | notice |
| `PasteClipboard` | `Intent::Text`, or `clipboard_failed {error}` | paste, or notice |
| `RunDaemonAction {daemon, action}` | `daemon_action_finished {report}` | the row's outcome and the Pal start offer |

Writes that preceded an attach (mark attached, release the in-place pane, create the shell record) moved into the producer. Tests: `ainb-app/tests/reports.rs` (one per outcome, with section versions), `ainb-core/tests/effect_host_reports.rs` (the executor source takes `&AppState` and posts no notice).

### Plugin actions and `ui.state`

| Layer | Added |
|-------|-------|
| protocol 0.1.1 | `plugin/handle_action {action_id, payload}`, topic `ui.state`; `wire-surface.lock` regenerated once |
| runtime | `RuntimeHandle::send_action`, on the plugin's inbox; spawns an idle plugin |
| SDK | `Plugin::handle_action`, dispatched inline in receive order |
| CTS | `axis_handle_action_forwarded_and_ui_state_read_back` with the `cts-action-forward` canary |
| host | unbound `plugin.owned.action {plugin, action_id, payload}` row through `dispatch`; `PluginsHostSection.plugin_ui_states` keeps each plugin's newest view by version |
| hangar | `ui_view::view` (screen, issues, selection, focused card) published on change; actions `screen.go`, `issue.open`, `issue.mark_done`, `modal.close` |

The bus keeps one `ui.state` value, so two plugins publishing in one tick keep the later; only hangar publishes it today.

### #1002

| Item | Where | Test |
|------|-------|------|
| 1 report path | above | `reports.rs`, `effect_host_reports.rs` |
| 2 identity pointer intents | `SessionListRowId`, source URIs, `SidebarItem` ids | `pointer_commands.rs::a_click_captured_before_its_workspace_is_removed_selects_nothing` |
| 3 one registry | pointer ids are unbound rows; `event_for` and the resolve fallback deleted; Markdown skips unbound rows | `pointer_commands.rs`, keymap golden |
| 4 no drawn geometry in a section | home sidebar rect, hover and resize in `UiState`; hit-test in core; width saved by intent | `host_width.rs` home case, `home_sidebar_mouse.rs` move bumps nothing |
| 5 statusline for a remote host | probe answer and live window copied into the Config and Fleet sections on the tick, bumped only on change | `statusline_from_sections.rs` |
| 6 unit-free widths | `home_sidebar_fraction`, `skill_manager_sources_fraction`; one-time migration by the startup width report | `host_width.rs` 80 and 200, `reports.rs` migration |

## P4: review screens and the P3 review items

Goal criteria 2 (`git_view`, `code_review`) and 3b (#1017).

### Review screens

| Before | After | Test |
|--------|-------|------|
| The code review sidebar's drawn rect and scroll window were `Cell`s inside `CodeReviewState`, in the shared GitView section | `ReviewSidebarLayout {rect, window}` lives in `UiState.review_sidebar`; the renderer records it while drawing | `ainb-core/tests/review_mouse.rs` |
| `main.rs` hit-tested a Review tab click and wrote the selection | `ainb-core/src/app/mouse.rs` hit-tests and dispatches `git_view.select_review_row {target}`, naming the row by `ReviewRowId` (a directory or file path), resolved where the row is now | `ainb-app/tests/review_commands.rs` (identity after the tree changed, a gone file changes nothing) |
| `main.rs` wheel wrote the git view scroll offsets | `git_view.scroll {lines}`, applied by the reducer to the active tab only | `review_commands.rs::the_wheel_scrolls_the_review_through_the_reducer` |

Both commands are unbound rows. `ainb diff-review` keeps its layout local and calls the same resolver. Home and log-history wheel and click paths stay in `main.rs` for P5.

### Side effects out of the reducer

| Was in the reducer | Effect | Report |
|--------------------|--------|--------|
| `RuntimeHandle::send_action` in the `plugin.owned.action` arm | `RunPluginAction {plugin, action_id, payload}` | `plugin_action_undelivered {plugin, action_id}` when no running plugin took it |
| `enter_interactive_pane` opening the writable embed (a PTY) | `AttachTerminal(InPlace {tmux_session, show_menu_bar})` | `in_place_opened {tmux_session}`, sent only once the host holds its own client; or `in_place_failed {tmux_session, error, unsupported}`, where `unsupported` stops the reducer asking a host that can never attach in place |
| `sync_terminal_observer` opening the read-only preview client | `AttachTerminal(Observe {tmux_session, show_menu_bar})`, returned by `AppState::request_terminal_observer` | `observer_opened {tmux_session}`, or `observer_failed {tmux_session, error, unsupported}` |

`in_place_sized` is gone: the host sizes the client from its own terminal and sidebar. `enter_interactive_pane` is deleted; `tripwire_interactive_pane` drives the key's command, opens the client as the host does and dispatches the report. A report is adopted only while the session list shows the row it names; otherwise the client is closed. Both clients belong to the host (`ainb-core/src/terminal_clients.rs`): state keeps only the session name and focus, and the host closes any client `embed_session` no longer names, before every effect and on every loop, so a released client never outlives a blocking attach. `InPlace` is portable: `tests/terminal_host_contract.rs` implements it with a headless host, including the key routing (every key to the client except the release chord) and a host that answers `unsupported`.

The executor is `execute(effect, terminal, ui, clients, plugins)`, with no `&AppState`. The data it read from state now rides on the effect, through validating constructors: `TmuxSessionName::new` (not empty, no `:` or `.`, no control characters, no leading `$`, `%`, `@` or `=`; both attach sites pass `=name` so tmux matches exactly) on `Session`, `InPlace`, `Tmux` and `WorkspaceShell`, and `EditorPath::new` (absolute) with the preferred editor on `OpenEditor`.

### Per-plugin `ui.state`

| Layer | Change |
|-------|--------|
| protocol 0.1.2 | `topics::ui_state_topic(plugin)`, prefix `ui.state/`; `wire-surface.lock` regenerated a second time (goal amended) |
| runtime | a plugin's bare `ui.state` publish is stored under `ui.state/<publisher>`; a publish to another plugin's slot is dropped; the slot is cleared when the plugin's process goes |
| CTS | `axis_two_plugins_publish_ui_state_without_overwriting_each_other`, `axis_a_crashed_plugins_ui_state_is_gone_before_it_restarts` |
| host | `record_plugin_ui_state(plugin, running, snapshot)` evicts a stopped plugin's view, refuses one over `MAX_PLUGIN_UI_STATE_BYTES` (256 KiB), and never takes a version at or below the last one seen stopped or refused (`plugin_ui_state_spent`); `plugin.owned.watch_screen {screen, watching}` keeps a plugin screen rendering while another host watches it, as a 30 s lease that lapses unless renewed and goes when the plugin is quarantined or unregistered; views and watches are not in the section frame (#983) |

### Command context gate

`Intent::Command` resolves a row only when its context is active, through the same `active_contexts` as `Intent::Key`. Host-authored rows (the report ids and plugin actions) resolve everywhere. `global.wire_statusline` writes Claude Code's settings, so it is in `KEY_ONLY_COMMANDS` and never runs by name (`key_only_commands.rs`). The slash palette's `/recall` now names the unbound `global.open_learnings` instead of a Learnings-screen row. Tests: `pointer_commands.rs::a_command_scoped_to_another_screen_changes_nothing`, `the_slash_palette_opens_learnings_from_any_screen`.

### Tripwires in CI

Job `core-tripwires` runs `cargo nextest run -p ainb -E 'binary(/^tripwire_/)'` minus `ainb-core/tests/tripwire_ci_exclusions.txt`, serially, and fails when nothing passed or the `SKIP:` lines are at least the pass count. Each exclusion carries an issue and a reason, checked by `ci_tripwire_exclusions.rs`. 13 excluded binaries are red on v2 both before and after #1008 on the same box (#1023, #1024, #1025); 3 more are red only on the CI runner (#1027); `tripwire_burndown_keys` is red on v2 and in CI (#1028). The job stages the bundled plugins first, because without them 30 plugin tripwires print `SKIP:` and pass. The first CI run also caught one #1008 regression, fixed here: `tripwire_plugin_runtime_baseline`'s fixture had no `event_bus` grant, so the bus dropped its snapshot.

### Left for P5

| What | Where |
|------|-------|
| `sessions_sidebar_width` is a column count | becomes a fraction of the row with the same one-time migration, because a desktop host has no columns |
| Synchronous disk writes on reducer paths: `app_config.save()` (7 in `events.rs`, 4 in `state.rs`), the favourites store (2 in `events.rs`, 1 in `components/new_session/pick_repo.rs`), the session label store (3 in `state.rs`), `interactive::SessionStore` (1 in `state.rs`), and the onboarding and auth-provider config loads and saves (1 in `events.rs`, 2 in `state.rs`) | one persistence effect covering every store, run by the host after the commit, so `dispatch` never blocks on disk; a source guard in the `CALL_SITES` style keeps the count at zero |
| Done in P5a (#1043): `AppState` owned the terminal clients (`tmux.embed` held the writable `EmbedClient`, handed over as a process-local `LocalEmbed`, and `sync_terminal_observer` opened the read-only one) | the host keeps both clients and state keeps the session name and focus; `TerminalTarget::InPlace` and `Observe` are portable, and `LocalEmbed` is deleted |
| `Intent::Command` carries no origin: report ids and plugin actions are runnable by any surface that can dispatch, and only `KEY_ONLY_COMMANDS` is refused by name | an origin on intents, or report rows a non-host surface cannot send, before any off-process surface dispatches |
| Home and log-history wheel and click writes | `ainb-core/src/main.rs` mouse loop |

