# Research: Desktop app equivalent to the ainb TUI, on a shared core

**Date**: 2026-09-04 14:10:02
**Repository**: agents-in-a-box (worktree `agents-in-a-box--desktop-appc204d0f1--adea5f3b`)
**Branch**: desktop-appc204d0f1
**Commit**: f9a36414
**Research Type**: Comprehensive (codebase + web)
**Brief**: `.agents/research/2026-09-04-desktop-app-brief.md` (interview-locked decisions D1-D9)

## Research Question

How do we build a proper desktop app equivalent to the `ainb` TUI, where both are built on the same core and only the UI differs, with matching dev experience (same operator UX, same build/test DX, same data with no drift, Zed/Warp-class feel)?

## Executive Summary

The extraction is cheaper than it looks: `app/state.rs` and `app/events.rs` (23.6k LOC) are already 99% renderer-agnostic, there is a single reducer over a 410-variant intent enum, and the plugin host already translates crossterm keys into a portable wire shape. The real work is the ~130 hard-coded key match blocks, 17 component state structs owned by `AppState`, and ~15.7k LOC of business logic living above `render()` fns in `components/`. Nobody ships a ratatui TUI plus a Tauri desktop from one core, but Neovim (core owns state + keymap, dumb renderers over an event stream) is the proven shape, and Tauri v2 has the primitives: `Channel<T>` for snapshot streams, a local WebSocket for the terminal hot path, `@wdio/tauri-service` for macOS e2e, SolidJS + xterm.js + CodeMirror 6 + uPlot for the view layer.

## Key Findings

- **Seam exists.** `AppEvent` (events.rs:25-626, 410 variants) and `process_event` (events.rs:3922, single reducer) already separate key-to-intent from intent-to-state. Extraction is a crate move plus a de-crossterm pass, not a rewrite.
- **Coupling is thin skin.** state.rs has 8 `Rect` refs; events.rs has one `Rect` hit-test island (883-921) and crossterm types in signatures. Estimated ~41.5k LOC moves to `ainb-app`, ~25.6k render code stays in `ainb-core`.
- **No data-driven keymap.** 456 `KeyCode` match arms across ~130 blocks in events.rs. Docs drift because nothing reads them. Keymap-as-data is a prerequisite for the desktop command palette and for parity checks.
- **Reducer leak.** main.rs:676-760 intercepts eight scroll events and mutates the ratatui `layout` directly. Desktop would silently lose scroll bindings unless that state moves into the core.
- **Terminal hot path must not go through Tauri IPC.** Open issues on raw binary payloads plus a 200ms/10MB Windows anecdote. Production Tauri terminals (OxideTerm) run a local WebSocket with binary frames and keep IPC for control. `ainb-web` already has the WS terminal.
- **Rust-WASM frontends are dead on arrival for this app.** The only xterm.js binding (`xterm-js-rs`) last pushed 2023-01-07. SolidJS recommended, Svelte 5 runner-up.
- **macOS e2e has a trap.** Apple ships no WKWebView WebDriver; stock `tauri-driver` covers Windows/Linux only. `@wdio/tauri-service` embeds a WebDriver in-app and covers macOS free.
- **Tauri does not supervise sidecars.** Spawn/kill only; no restart-on-crash plugin exists (plugins-workspace#3062). Hand-roll it, same as the TUI does for the daemon today.
- **Category converges on our shape.** Sidebar of worktree-scoped sessions + attention state per row + diff panel is what Conductor, Emdash, Superset, cmux, agent-deck all do. Conductor is Tauri + Rust core, explicitly chosen over Electron for cold start.

## Prior Learnings

None returned from the knowledge base for this topic. The one adjacent prior doc, `research/2026-06-02_11-07-19_tmux-pane-in-tui.md`, established that `tui-term` (vt100 + portable-pty) is the in-TUI embed path and that the current attach is a full-screen tty handoff. That handoff has no desktop analogue (see knot 5 below).

## Detailed Findings

### Codebase Analysis

#### A. `app/` extraction surface (ainb-tui/crates/ainb-core/src/app/)

Production LOC excludes inline `#[cfg(test)]` blocks (brace-matched).

| file | total | prod | agnostic est | coupled est | coupling kind |
|---|---|---|---|---|---|
| `state.rs` | 14,045 | 13,121 | ~13,050 | ~70 | 8 `Rect` refs (`state.rs:26,601,602,605,632,641,3227,3230`). Zero `Frame`, `KeyEvent`, `Style` |
| `events.rs` | 10,570 | 8,616 | ~8,560 | ~55 | crossterm `KeyCode/KeyEvent/KeyModifiers` in signatures (`events.rs:17`); `Rect` hit-test island `events.rs:883-921` |
| `screens/builtin.rs` | 1,470 | 1,103 | ~120 | ~980 | real renderer glue; `Frame`/`Rect` at `:3`, key translation `:91,188,274` |
| `screens/mod.rs` | 143 | 108 | ~70 | ~38 | `Screen` trait is draw-coupled |
| `attach_handler.rs` | 198 | 198 | 0 | 198 | owns `Terminal<CrosstermBackend<Stdout>>`, raw-mode toggling |
| `session_loader.rs` | 276 | 276 | 276 | 0 | pure domain (docker, worktrees, tmux) |
| `snapshot.rs` | 193 | 193 | 193 | 0 | pure domain (tmux/git subprocess) |
| `event_bus.rs` | 130 | 92 | 92 | 0 | pure `HashMap<&str, Vec<Box<dyn FnMut(&[u8])>>>` |
| `registry.rs` | 140 | 66 | 66 | 0 | only the test stub imports ratatui (`registry.rs:73`) |
| `state_tests.rs` | 2,987 | test | ~2,987 | 0 | asserts on `AppState` |

**Event loop today** (`main.rs:418-760`, `run_tui_loop`; tick 16ms per `config/tunables.rs:394`, app tick 250ms, dirty-gated `needs_redraw`):

```
 ┌──────────┐ 16ms  ┌───────────────┐ needs_redraw ┌──────────────────┐
 │ crossterm│──────▶│  run_tui_loop │─────────────▶│ layout.render()  │
 │  poll    │       │  main.rs:475  │              │ ratatui Frame    │
 └──────────┘       └───────┬───────┘              └──────────────────┘
                            │ KeyEvent
                            ▼
              ┌─────────────────────────────┐
              │ handle_key_event            │ events.rs:1473
              │ KeyEvent ─▶ Option<AppEvent>│ ~130 match blocks
              └─────────────┬───────────────┘
                            ▼
              ┌─────────────────────────────┐
              │ process_event               │ events.rs:3922
              │ AppEvent ─▶ &mut AppState   │ SINGLE REDUCER ~4480 LOC
              └─────────────┬───────────────┘
                            ▼
              ┌─────────────────────────────┐    ┌────────────────────┐
              │ AppState                    │◀───│ 9 mpsc receivers   │
              │ pending_async_action        │    │ try_recv in tick() │
              └─────────────┬───────────────┘    └────────────────────┘
                            ▼
              process_async_action  state.rs:10345  (tokio)
```

- Async in: nine `mpsc::UnboundedReceiver` fields on `AppState` (`state.rs:1013,3455,3463,3472,3484,3491,3513`), drained by `try_recv` in `App::tick` (`state.rs:12877`).
- Async out: `pending_async_action` (`state.rs:3177`) holds an `AsyncAction` (`state.rs:3761`), executed by `process_async_action` (`state.rs:10345`).
- Reducer leak: `main.rs:704-760` handles `ScrollLogsUp/Down/ToTop/ToBottom`, `ToggleAutoScroll`, `ScrollPreviewUp/Down`, `ExitScrollMode` by mutating `layout`, and `main.rs:676-700` swallows arrows and `j`/`k`/PageUp/PageDown when `tmux_preview.is_scroll_mode()`.

**Keymap:** none data-driven. `KeyCode::` appears 529 times in events.rs, 456 as match-arm patterns, ~130 `match key_event.code` blocks from `:1481` to `:3459`. Twenty per-screen handlers (`handle_git_view_keys :2796`, `handle_fleet_panel_keys :3162`, `handle_onboarding_keys :2599`). The word "keymap" in code means the legend bar (`state.rs:6806`, `events.rs:207`, `config/registry.rs:576`, `components/help.rs:80`). `docs/tui/keyboard-shortcuts.md` is prose that nothing reads.

**Screen trait** (`screens/mod.rs:80-106`):

```rust
pub trait Screen: Send {
    fn id(&self) -> &str;
    fn render(&mut self, frame: &mut Frame, area: Rect, state: &mut AppState);
    fn handle_event(&mut self, _state: &mut AppState) -> EventOutcome { EventOutcome::NotHandled }
    fn handle_key(&mut self, _state: &mut AppState, _key: &crossterm::event::KeyEvent) -> EventOutcome { EventOutcome::NotHandled }
}
```

Two of three methods are renderer-coupled. `ScreenRegistry` (`registry.rs:13`) is renderer-free in production. All `impl Screen` live in `screens/builtin.rs`; plugin screens share one generic `PluginScreen` impl that translates the crossterm key into a portable wire shape before `plugin/handle_key` (`screens/mod.rs:96-98`). That translation is the model for de-crossterming everything else.

#### B. `components/` logic-in-view (ainb-tui/crates/ainb-core/src/components/)

Every component follows `impl XState { logic }` then free `pub fn render(frame, area, &mut XState)`. Cutting at the first render fn gives a seam. Across 50 files: ~15,700 LOC logic above the seam, ~23,900 render below (estimate).

| # | file | total | logic above render | evidence |
|---|---|---|---|---|
| 1 | `session_recovery.rs` | 2,113 | 1,381 | 12 `Command::new`, 25 fs calls; `:351` reads `.agents`, `:371` shells to tmux |
| 2 | `git_view.rs` | 2,203 | 1,299 | `git2::Repository` `:7`, `Repository::open` `:292`, delta classification `:307-309` |
| 3 | `fleet_panel.rs` | 2,178 | 1,086 | `impl FleetPanelState` `:169`; chat polling, tokio channels |
| 4 | `daemons.rs` | 2,654 | 831 | `impl DaemonsState` `:302`; `Command` `:690`, mpsc `:478,536` |
| 5 | `new_session/configure.rs` | 3,668 | 804 | `LaunchSpec`, referenced from `state.rs:3773` |
| 6 | `skill_manager_screen.rs` | 3,474 | 786 | fs writes `:2046,2223,2224`, marker delete `:2144` |
| 7 | `log_history_viewer.rs` | 1,093 | 686 | 4 fs reads; log parse + index |
| 8 | `code_review/render.rs` | 1,273 | 508 | diff-block folding and hunk math above render |

#### C. Tests

- 101 `tripwire_*.rs` files, 23,574 LOC, PTY-driven, stay with ratatui.
- 45 other integration files, ~15,200 LOC.
- Movable unchanged into the new crate: `app/state_tests.rs` (2,987) plus `test_app_state.rs`, `test_events.rs`, `test_manual_refresh.rs`, `test_session_creation_refresh.rs`, `tripwire_core_skill_manager_drift_background_poll.rs`, ~3,500 LOC total.

#### D. Proposed crate split

| crate | contents | est LOC |
|---|---|---|
| `ainb-app` | `state.rs` less `Rect` (13,050); `events.rs` less crossterm layer (8,560); `session_loader`/`snapshot`/`event_bus`/`registry` (630); `screens::ids` + `EventOutcome` (70) | 22,310 |
| `ainb-app` | component `*State` structs and impl blocks, cut at first render fn | 15,700 est |
| `ainb-app` | renderer-free tests | 3,500 |
| `ainb-core` | render fns (23,900 est), `screens/builtin.rs` (1,470), `attach_handler.rs` (198), `main.rs` loop, 101 tripwires | ~25,600 + tests |

Total moved: ~41,500 LOC.

**Top 5 hard knots**

1. `AppState` owns 17 component-typed state fields (`state.rs:3261,3319,3322,3325,3331,3334,3337,3341,3344,3348,3452,3458,3705,3710`) plus `AsyncAction::CreateSessionFromConfigure(LaunchSpec)` (`state.rs:3773`); 44 `crate::components::` refs. Those structs move first.
2. `crossterm::KeyEvent` threads through ~130 handler signatures from `events.rs:17`.
3. `Rect` in `AppState` for mouse hit-testing (`state.rs:601,602,605,3227,3230`), consumed by `handle_mouse_event` (`events.rs:952`) and geometry recompute (`events.rs:883-921`).
4. `main.rs:676-700` and `:704-760` mutate `layout` not state.
5. `attach_handler.rs` hands the tty to `tmux attach`; no desktop analogue. `ainb-app` carries only the intent; each renderer implements attach its own way (tty handoff vs xterm.js WS).

#### E. Plugin runtime, ainb-web, ACP, daemon transport

**E1. Plugin runtime render + snapshot contract**

- 28 wire methods in `ALL_METHODS` (`ainb-plugin-protocol/src/methods.rs:195`). Host→plugin: `plugin/init`, `shutdown`, `render`, `handle_event`, `handle_key`, `handle_mouse`, `cli_dispatch`. Plugin→host: `host/snapshot/{get,publish,subscribe}`, `host/action/invoke`, `host/log`, `host/fs/*`, `host/network/fetch`, `host/event_stream_*`, `host/spawn_managed_subprocess`, `host/unix_socket_*`, `host/secret_store_get`, six `host/workspace_*`.
- Render is pull, cells only: `RenderParams { viewport, generation }` → `RenderResult { buffer: WireBuffer, redraw, captures_text }` (`params.rs:94-135`). `WireBuffer` is a sparse `Vec<(Coord, Cell)>`, RGB fg/bg, `u16` modifier bitfield (`wire_buffer.rs:100-109`).
- Host paint is a direct cell loop into the ratatui buffer (`app/screens/builtin.rs:647-670`; `rgb_to_color :681`, five modifier bits `:689`). Render tick at `state.rs:12477` (`tick_plugin_renders`); a static screen→plugin table lives at `state.rs:12494` and is duplicated at `builtin.rs:64` (`PLUGIN_SCREENS`).
- Snapshot bus (`ainb-plugin-runtime/src/snapshot.rs`): `RwLock<HashMap<Topic, (Bytes, Version, PluginId)>>`, one global monotonic version (`:80-86`), latest-wins, no history. Publisher id stamped by the host (`:32-37`). Two consumers: plugins subscribe and get `plugin/handle_event` pushes; the host polls by version via `snapshot_get_versioned` (`handle.rs:297`), as `tick_panel_close_requests` does (`state.rs:12102-12126`).
- Reserved topics: exactly one, `ui.close_request` (`topics.rs:23`).
- Input: keys and mouse forwarded as notifications on priority inboxes to the focused plugin only (`runtime.rs:26-32`); crossterm→wire at `builtin.rs:85`; mouse coords pre-translated to plugin viewport (`builtin.rs:645`).
- Watchdog: 2s render timeout (`types.rs:193`), `expire_render` sets `render_wedged` (`plugin_task.rs:921`), quarantine after 3 failures in 60s, `redraw_governor` (`plugin_task.rs:944`).
- SDK trait (`ainb-plugin-sdk-rust/src/plugin.rs:90`): `manifest()` and `render() -> WireBuffer` required; the rest defaulted. Manifest (`manifest.rs:36`): 13 capability grants (`:152-226`), `[provides] screens/commands/cli_namespaces/snapshots`, `[subscribes]`.
- **`ui.view_model` slot: zero protocol churn.** Topics are free-form strings on `host/snapshot/publish`, and plugins already publish from inside `render` (`ainb-plugin-hangar/src/plugin.rs:6138-6143` publishes `ui.close_request`). A desktop host reads it with `snapshot_get_versioned`. A named constant in `topics.rs` is optional and costs a protocol version bump plus lock regen.

**E2. What `hangar-tui` needs, and the CTS constraint**

- `ScreenStates` (`ainb-plugin-hangar/src/screen/app_screens.rs:593`) is not `Serialize` and mixes data with cursor/mode/wizard state. Its inputs are already serde proto rows (`IssueRow`, `TaskCardRow`, `AttentionRow`), so the view model is a projection of those rows, not the cache.
- CTS constrains only protocol-crate edits: `tests/wire_surface_gate.rs` byte-compares a `syn`-rendered surface against `wire-surface.lock`; `topics` and `params` are in `SURFACE_MODULES` (`ainb-plugin-cts-v2/src/wire_surface.rs:29`). A new const or field needs a version bump plus `UPDATE_WIRE_SURFACE=1` regen.

**E3. `ainb-web` (ainb-tui/crates/ainb-web)**

- Rust deps: `ainb-hangar-proto` and `ainb-hangar-core` only. No `ainb-fleet-core`, `ainb-usage`, or `ainb-hangar-client`.
- Sessions and cost come from shelling `ainb --format json list | fleet cost` (`src/data.rs:185-235`); needs come from the daemon (`data.rs:266`).
- Reimplements the daemon client: dial, `auth/hello`, Content-Length JSON-RPC, fresh connection per call, 5s timeout (`src/daemon.rs:38`), duplicating `ainb-hangar-client` and its `DaemonError`.
- Terminal is a real `tmux attach-session -t <name>` under `portable-pty` (`terminal.rs:288-303`). WS framing: server sends JSON status text frames plus raw binary PTY bytes; client sends `{type: input|resize|ping}` (`terminal.rs:22-38`); resize floor 10x3 (`:53-58`).
- Auth: one bearer token, constant-time via `subtle`; `?token=` whitelisted to `/api/events` and `/ws/session/` (`auth.rs:17-35`); `read_only_gate` 403s the terminal and `/api/answer` (`terminal.rs:112`).
- JS reimplements session cards, needs taxonomy and ordering, cost formatting, SSE reconnect, terminal wiring (`frontend/app.js:105-254, 372-520`), 676 lines by hand.
- Reusable in Tauri: `PtyBridge` and the frame protocol port directly with xterm.js. Web-only: `push.rs` VAPID, `auth.rs`, `assets.rs`, the SSE poller. Replace `data.rs` rather than reuse it.

**E4. ACP (ainb-tui/crates/ainb-acp)**

- Reducer emits `TranscriptChunk { kind, session_id, event_id, text, payload }` (`reducer.rs:75`); `ChunkKind` has seven variants mapping to `acp.message|user_message|thought|tool_call|plan|permission|usage` (`:33-71`). Neither derives `Serialize`.
- Store target is `fleet_provider_event` rows, `source = 'acp'`, cursored by `ingest_order`; no transcript table (`store_writer.rs:1-7`).
- A live stream exists and is enforced: `TranscriptSink` publishes live before the durable commit and owns the writer privately (`ainb-hangar-daemon/src/acp_transcript.rs`).
- A UI subscribes via `fleet/transcript_subscribe` plus `fleet/transcript_list` (`ainb-hangar-proto/src/methods.rs:466-474`); chunks arrive as serde `FleetTranscriptChunk` (`fleet.rs:1587`); client seam `open_transcript_subscription` / `next_chunk` (`ainb-hangar-client/src/lib.rs:439, 179`).
- The TUI has no ACP transcript screen. Hangar surfaces ACP only as `acp_permission` cards (`screen/control_center.rs:346`) plus inbox rows. The desktop chat card would be the first transcript consumer.

**E5. Daemon transport and auth**

- Unix socket only, `{hangar_home}/hangar.sock`, stale file unlinked then chmod `0600` (`ainb-hangar-daemon/src/rpc/mod.rs:225-242`). No `TcpListener` in the daemon.
- Converged-control-center D2 is fully implemented: `0600` (`rpc/mod.rs:241`), `SO_PEERCRED` same-uid via `peer_cred()` (`rpc/auth.rs:218`), mandatory first-frame `auth/hello` token verify (`rpc/auth.rs:229`).

- Two caller identities at `rpc/auth.rs:53`: `Caller::Operator` (the `0600` daemon token) and `Caller::Copilot { scope }` (per-channel token so a model-steered tool server cannot answer its own cards).
- Reconnect: none, in any client. `FleetStreamEvent::ResyncRequired` (`ainb-hangar-client/src/lib.rs:105`) only tells the caller to resnapshot after lag. Message and transcript streams have no resync frame; the forwarder pages from its own cursor (`:149, :178`). Subscriptions retain the write half so the server never sees EOF (`:110-116`).
- `workspace/subscribe`: params `{workspace_id}`, result is the current snapshot, then events push on the same connection (`ainb-hangar-proto/src/methods.rs:14-19`).
- `ALL_METHODS`: 153 entries (`methods.rs:1654`). Notifications: 5 fleet (`:553-561`) plus `hangar/event` carrying a 13-variant `HangarEvent` (`events.rs:29-41`).
- TCP / remote / tailnet / SSH: none in code or docs. Only tailnet mention is `docs/tui/web.md:13,31`, about exposing `ainb-web` with a bearer token. The `SO_PEERCRED` same-uid check blocks a shared-box model outright.

Implications for the desktop:

- Link `ainb-hangar-client` and dial the daemon directly; do not route hangar data through the plugin ABI.
- Write reconnect once, in `ainb-hangar-client`, and let TUI, web, and desktop share it. Nothing in the tree has any today.
- Remote (D7) means an SSH or tailnet socket forward outside the daemon, or a new listener plus new auth. The same-uid peer check must be bypassed or replaced for either.
- Three daemon clients already exist (`ainb-hangar-client`, `ainb-web/src/daemon.rs`, the Swift `FleetRPC`). The Tauri app is the fourth unless it reuses the first.

### External Research

#### R2. Shared-core prior art (fetched 2026-09-04)

| project | boundary | snapshot vs RPC | keymap | core tests | pain |
|---|---|---|---|---|---|
| Helix | `helix-core` → `helix-view` (meant to be frontend-agnostic, still crossterm-coupled) → `helix-term` | neither cleanly; commands live in `helix-term` | not shared | none | maintainers: commands instantiate TUI components directly; no agreement on grid API vs custom components ([architecture.md](https://github.com/helix-editor/helix/blob/master/docs/architecture.md), [discussion #11783](https://github.com/helix-editor/helix/discussions/11783)) |
| Neovim / Neovide | full process split; nvim is a server | msgpack-RPC semantic `ui_events`, `ext_linegrid`/`ext_multigrid` opt-ins | entirely in core; UIs forward raw input | core suite independent of UI; protocol versioned | IPC overhead, but cleanest separation ([api-ui-events](https://neovim.io/doc/user/api-ui-events/), [neovide](https://github.com/neovide/neovide)) |
| Zed | one renderer; `foo` / `foo_ui` / `foo_settings` crate naming | n/a | n/a | logic crates testable without GPUI | convention, not compiler-enforced ([deepwiki](https://deepwiki.com/zed-industries/zed/4.1-editor-core)) |
| Ghostty | `libghostty` C-ABI core, native shells (Swift, GTK) | embedding API + callbacks | per-platform | n/a | validates "zero UI deps in core", not a snapshot precedent ([libghostty](https://mitchellh.com/writing/libghostty-is-coming)) |
| Ratatui 0.30 | `ratatui-core` / `-widgets` / backends; `Backend` trait | push buffer diff | app-owned | `TestBackend` (medium confidence) | non-terminal backends exist: [egui_ratatui](https://github.com/gold-silver-copper/egui_ratatui), [ratzilla](https://github.com/ratatui/ratzilla), [ratatui-wgpu](https://github.com/Jesterhearts/ratatui-wgpu); docs push TEA ([ARCHITECTURE.md](https://github.com/ratatui/ratatui/blob/main/ARCHITECTURE.md), [TEA](https://ratatui.rs/concepts/application-patterns/the-elm-architecture/)) |
| Warp | custom `warpui`, no public boundary write-up | unknown | n/a | n/a | gap |

Transferable patterns:

1. Adopt the Neovim shape, not the Helix shape. Core owns state and keymap, emits snapshots/events, renderers are dumb.
2. TEA: core = Model + Update, both renderers are pure `view(snapshot)`.
3. Keymap dispatch lives inside core only; renderers forward raw input.
4. Tauri side: `tauri::State` + `Channel<T>` (ordered, high-throughput) over `emit`/`listen`; `tauri-specta` generates TS types from the same Rust model ([calling-frontend](https://v2.tauri.app/develop/calling-frontend/), [tauri-specta](https://github.com/specta-rs/tauri-specta)).
5. Ratatui's `Backend` seam is a fallback for widgets not worth re-implementing: a cell-grid canvas in the webview (what D4 rejected for plugins, but still a valid escape hatch for one-off screens).

No public project ships a ratatui TUI and a Tauri desktop from one core. We would be first.

#### R3. Agent desktop apps (fetched 2026-09-04)

| app | stack | session model | terminal | attention | chat vs terminal | remote | plugins | OSS |
|---|---|---|---|---|---|---|---|---|
| Conductor | Tauri, Rust core | worktree per task, sidebar | integrated + diff panel | not public | chat-first | Mac only | none | closed, free |
| Codex desktop | native | threads per project | in-app terminal + browser | not detailed | chat threads | unclear | 90+ Agent Plugins | closed |
| Claude Code desktop | native | parallel sessions | integrated | not detailed | side chat + diff + browser | unclear | none | closed |
| Vibe Kanban | Rust + React, local server | card → worktree | per workspace | status per card | kanban | SSH hosts | none | Apache-2.0, vendor shut April 2026 |
| cmux | Swift + libghostty | tabs/splits, branch-aware | real PTY | ring on pane + jump-to-unread | raw terminal | `cmux ssh`, remote tmux | Skills + Unix socket API | GPL-3.0 |
| Superset | source-available IDE | worktree per task | built-in | not detailed | IDE-style | macOS only | none | ELv2 |
| agent-deck | Go + Bubble Tea over tmux | agent + worktree + Docker | tmux, TUI + read-only web | polling states, escalates to Telegram/Slack | TUI transcript | SSH, drain | MCP per session, Skills | MIT |
| Multica | Go daemon + Next.js + Postgres | task as teammate | not terminal-centric | in-thread posts | board/chat hybrid | self-hosted | skills library | OSS |
| Emdash | Electron | worktree per task | embedded | not detailed | chat + diff + ticket import | SSH | none | Apache-2.0 |
| Sculptor | native + Docker | worktree + container | terminal + IDE pairing | not detailed | diff review | WSL on Windows | none | closed beta |

Zed's Agent Panel: threads sidebar grouped by project, per-thread folder access, tab bar for parallel threads. Warp: each agent is a terminal tab/pane, no board layer.

Reported pain at five or more sessions: cannot map terminal to task, idle sessions waste parallelism, loops go unnoticed, "air-traffic controller who's lost the radar". Status fields reflecting last-polled instant let a session sit "waiting" 20+ minutes unseen ([clearly.sh](https://www.clearly.sh/blog/run-multiple-claude-code-sessions)).

Worth stealing: per-pane attention ring + jump-to-unread hotkey ([cmux](https://github.com/manaflow-ai/cmux)); escalation chaining ([agent-deck](https://github.com/asheshgoplani/agent-deck)); ticket import as session creation ([emdash](https://github.com/generalaction/emdash)); pairing back into the operator's IDE ([sculptor](https://github.com/imbue-ai/sculptor)); Tauri over Electron for cold start ([Conductor rewrite](https://performance.dev/the-conductor-rewrite)).

Avoid: hosted tier without a revenue plan ([Vibe Kanban shutdown](https://www.vibekanban.com/blog/shutdown)); last-polled status badges; being a "primitive, not a solution" (cmux maintainer); vendor-locked plugins (Terragon shut 2026).

#### R4. Tauri v2 mechanics (fetched 2026-09-04)

1. **Terminal bytes.** No official IPC benchmark. `Channel<T>` still rides the webview fetch machinery; raw binary support is an open request ([tauri#7127](https://github.com/tauri-apps/tauri/issues/7127), [#13405](https://github.com/tauri-apps/tauri/issues/13405)). [OxideTerm](https://analysedecircuit.github.io/oxideterm/) (Tauri 2, xterm.js 6 WebGL) runs a local WebSocket with binary frames `[Type:1B][Len:4B][Payload]`, zero JSON on the hot path, IPC for control only. [tauri-plugin-pty](https://github.com/Tnze/tauri-plugin-pty) is v0.1.1, 22 stars, Windows-only examples. Recommendation: local axum WS from `portable-pty`, IPC for control.
2. **Sidecar.** `bundle.externalBin` with target-triple-suffixed names; `shell().sidecar().spawn()` needs `shell:allow-execute` with `sidecar: true`. Not killed on exit: hold `Mutex<Option<CommandChild>>`, kill on `RunEvent::ExitRequested`. No supervision plugin ([plugins-workspace#3062](https://github.com/tauri-apps/plugins-workspace/issues/3062)). ([sidecar docs](https://v2.tauri.app/develop/sidecar/))
3. **Linux.** webkit2gtk 4.1; build on Ubuntu 22.04 baseline. AppImage ~70-76MB embeds WebKit; .deb 4-6MB depends on it. ARM AppImage cannot cross-compile; native ARM runners since Aug 2025. `x11` cargo feature default since 2.6.0. WSL needs WSLg; known gdk-pixbuf startup panic, workaround `"icon": []` ([discussion #8648](https://github.com/tauri-apps/tauri/discussions/8648)). ([appimage docs](https://v2.tauri.app/distribute/appimage/))
4. **macOS.** `tauri-action` signs, notarizes, uploads `latest.json`; universal via `--target universal-apple-darwin`; `tauri-plugin-updater` 2.0 with `tauri signer generate`. Gaps: minimum macOS version not found; sidecar under universal build unconfirmed.
5. **E2E.** Apple ships no WKWebView WebDriver; `tauri-driver` is Windows/Linux only ([tauri#7068](https://github.com/tauri-apps/tauri/issues/7068), [webdriver docs](https://v2.tauri.app/develop/tests/webdriver/)). `@wdio/tauri-service` embeds a WebDriver in-app and covers macOS free ([platform support](https://webdriver.io/docs/desktop-testing/tauri/platform-support/)); CrabNebula's fork needs a paid key on macOS. Playwright has no first-class driver. Linux CI via `xvfb-run`.
6. **Frontend build.** Leptos has an official starter, but xterm.js from Rust-WASM is unverified. vite + `beforeBuildCommand` is standard; `tauri-specta` emits `.ts` either way.
7. **Security.** Capabilities deny-by-default per window label; `remote.urls` block for tailnet daemons; Linux cannot distinguish iframe requests. CSP `connect-src` with the literal `ws://localhost:<port>` (inference from CSP semantics, not a quoted example); separate capability for the tailnet `wss://` origin so a compromised frontend cannot pivot.

#### R5. Frontend framework pick (fetched 2026-09-04)

| candidate | maturity | Tauri fit | cargo-only | xterm/diff/chart interop | typed IPC | dev-tool feel |
|---|---|---|---|---|---|---|
| Vanilla TS | 5 | 5 | no | 5 | 4 | 2 |
| SolidJS | 4 | 5 | no | 5 | 4 | 4 |
| Svelte 5 | 5 | 5 | no | 5 | 4 | 4 |
| React 19 | 5 | 5 | no | 5 | 4 | 3 |
| Leptos | 3 | 3 | yes | 2 | 3 | 3 |
| Dioxus 0.7 | 4 | 2 | yes | 2 | 3 | 3 |
| Yew | 3 | 1 | yes | 2 | 3 | 2 |

Decisive constraint: [xterm-js-rs](https://github.com/segeljakt/xterm-js-rs) last pushed 2023-01-07. Any Rust-WASM frontend means hand-rolled `js-sys` glue for the most load-bearing widget. Dioxus desktop also uses `wry`, so it does not avoid the webview while losing Tauri's plugin ecosystem ([pistack comparison](https://www.pistack.xyz/posts/2026-08-29-dioxus-vs-yew-vs-leptos-rust-wasm-frameworks-comparison/)).

Libraries: CodeMirror 6 + [Shiki](https://shiki.style/) ([Inkwell](https://github.com/Amoner/inkwell), a 5MB Tauri app); [uPlot](https://apexcharts.com/blog/javascript-chart-library-benchmark/) 9ms/100K points, 20-50KB; [tauri-specta](https://github.com/specta-rs/tauri-specta) active (pushed 2026-07-26). Starters: [tauri-start-solid](https://github.com/riipandi/tauri-start-solid), [tauri2-svelte5-shadcn](https://github.com/alysonhower/tauri2-svelte5-shadcn), [awesome-tauri](https://github.com/tauri-apps/awesome-tauri).

Caveat: Zed and Warp are custom GPU UIs ([Warp](https://thenewstack.io/warp-open-source-client/)). "Zed/Warp feel" is a polish target on top of Tauri + web, not an architectural match.

## Target Architecture (synthesis)

```
┌─────────────────────┐            ┌──────────────────────────────┐
│ ainb (TUI binary)   │            │ ainb-desktop (Tauri v2)      │
│ ratatui renderer    │            │ SolidJS + xterm.js + CM6     │
│ tty attach handoff  │            │ WS terminal · Channel<T>     │
└──────────┬──────────┘            └──────────────┬───────────────┘
           │ view(snapshot) / KeyEvent→Intent      │ tauri cmd · Channel · WS
           ▼                                       ▼
┌──────────────────────────────────────────────────────────────────┐
│ ainb-app (new crate, ~41k LOC moved)                             │
│ AppState · AppEvent (410 intents) · process_event (reducer)      │
│ Keymap-as-data · Snapshot<ViewModel> · component *State          │
└────────┬──────────────────┬──────────────────┬───────────────────┘
         ▼                  ▼                  ▼
  hangar-client       fleet-core        plugin-runtime (+ ui.view_model)
         ▼
  hangar daemon: local sidecar │ remote tailnet │ N hosts
```

Renderer contract: `Intent` in (portable key/mouse/command), `Snapshot` out. The TUI renders Snapshot to a ratatui Frame; the desktop serializes Snapshot over `Channel<T>` with `tauri-specta` types. Terminal bytes bypass both: xterm.js talks to the local WS that `ainb-web` already ships.

## Code References

- `ainb-tui/crates/ainb-core/src/app/events.rs:25-626`: `AppEvent`, 410-variant intent enum
- `ainb-tui/crates/ainb-core/src/app/events.rs:1473`: `handle_key_event`, ~130 match blocks
- `ainb-tui/crates/ainb-core/src/app/events.rs:3922`: `process_event`, single reducer
- `ainb-tui/crates/ainb-core/src/app/events.rs:883-921`: `Rect` hit-test island
- `ainb-tui/crates/ainb-core/src/app/state.rs:3261-3710`: 17 component-typed fields on `AppState`
- `ainb-tui/crates/ainb-core/src/app/state.rs:10345`: `process_async_action`
- `ainb-tui/crates/ainb-core/src/app/state.rs:12877`: `App::tick`, drains 9 mpsc receivers
- `ainb-tui/crates/ainb-core/src/app/screens/mod.rs:80-106`: `Screen` trait
- `ainb-tui/crates/ainb-core/src/app/screens/mod.rs:96-98`: `PluginScreen` key translation
- `ainb-tui/crates/ainb-core/src/main.rs:676-760`: reducer bypass for scroll events
- `ainb-tui/crates/ainb-core/src/app/attach_handler.rs:1-198`: tty handoff
- `ainb-tui/crates/ainb-web/frontend/{app.js,xterm.js}`: vanilla JS dashboard + terminal
- `ainb-tui/crates/ainb-plugin-protocol/src/wire_buffer.rs`: cell-grid render contract
- `ainb-tui/crates/ainb-plugin-protocol/src/topics.rs`: reserved snapshot topics

## Recommendations

1. **Phase 0, keymap-as-data + reducer sealing.** Replace the ~130 match blocks with a `Keymap` table mapping `(ScreenId, Chord) → AppEvent`, and move the eight scroll events from `main.rs` into the reducer. Generate `docs/tui/keyboard-shortcuts.md` from the table. This is TUI-only, ships on its own, and is the prerequisite for both the command palette and parity checks.
2. **Phase 1, `ainb-app` crate.** Move `state.rs`, `events.rs` (post de-crossterm), `session_loader`, `snapshot`, `event_bus`, `registry`, then the 17 component `*State` structs cut at the render seam. Define `Intent` and `Snapshot` types. Move the ~3,500 LOC of renderer-free tests. Tripwires stay green throughout because `ainb-core` still renders the same state.
3. **Phase 2, plugin `ui.view_model` topic.** Add a structured JSON topic beside `WireBuffer` in `ainb-plugin-protocol`, SDK helper, CTS axis. Port `hangar-tui` first (it is the control center), then burndown.
4. **Phase 3, `ainb-desktop` crate.** Tauri v2 + SolidJS + `tauri-specta`. Terminal over the existing `ainb-web` WS. Sidecar daemon with hand-rolled supervision. Screens in order: Hangar board + attention, Sessions + terminal, Code review + inbox, plugin screens.
5. **Phase 4, proof.** `@wdio/tauri-service` e2e on macOS + Linux (xvfb), snapshot parity test rendering one fixture `Snapshot` through both renderers, screenshot review.
6. **Spike before committing CI:** minimum macOS version, sidecar in a universal build, and terminal throughput through the WS under a `cat` of a large file.

## Open Questions

- Which subset of the 410 `AppEvent` variants is renderer-only (scroll, focus, layout) versus domain, and should those get a separate `UiIntent` enum?
- Multi-host state model: one `AppState` per host, or one `AppState` with a host dimension? Affects the reducer signature.
- Remote daemon transport does not exist (Unix socket only, no `TcpListener`). D7 (tailnet, multi-host) needs a transport decision: SSH port-forward of the socket, a TCP listener with the existing `auth/hello` token, or tunnelling through `ainb-web`'s axum. Which one, and does it live in the daemon or in `ainb-hangar-client`?
- `TranscriptChunk` and `ChunkKind` do not derive `Serialize`; the proto `FleetTranscriptChunk` does. Does the desktop chat card consume the proto type over `fleet/transcript_subscribe`, or does `ainb-app` own a transcript reducer state that both renderers read?
- `ainb-web` duplicates the daemon client. Should the Tauri app depend on `ainb-hangar-client` directly, and should `ainb-web` be migrated onto it in the same move?
