# Research: Mouse-first TUI Step 1

**Date**: 2026-05-20
**Repository**: agents-in-a-box
**Branch**: feat/ui
**Commit**: ef70103eb0bbcce2dcf68eed6737a1bfa91c5eba
**Research Type**: Codebase + Documentation + Prior Learnings

## Research Question

Understand enough context from `/tmp/mouse-first-tui-prompt.md` to interview on a narrow Step 1 implementation: drag the HomeScreen sidebar edge in `ainb tui`.

## Executive Summary

The TUI already receives crossterm mouse events, but HomeScreen drag behavior is not wired. Step 1 can stay small: add HomeScreen-specific sidebar width state, use it in render and Home scroll hit testing, handle left-drag on the sidebar boundary, and persist the chosen width through the existing user config.

## Key Findings

- `ainb` launches TUI by default or through `ainb tui`; setup enables raw mode, alternate screen, mouse capture, and bracketed paste in `ainb-tui/crates/ainb-core/src/main.rs:189`.
- Mouse events already reach the main loop in `ainb-tui/crates/ainb-core/src/main.rs:405`; click, wheel, drag, and release branches exist, but drag is only converted to generic `MouseDragging`.
- `EventHandler::handle_mouse_event` only switches focus on `SESSION_LIST` using 40 percent split math in `ainb-tui/crates/ainb-core/src/app/events.rs:407`; `HOME` returns `None`.
- HomeScreen V2 is the active HomeScreen path. `HomeScreen` delegates to `HomeScreenV2Component` with `state.home_screen_v2_state` in `ainb-tui/crates/ainb-core/src/app/screens/builtin.rs:455`.
- Home sidebar width is duplicated and hardcoded: full layout uses `26` at `ainb-tui/crates/ainb-core/src/components/home_screen_v2.rs:204`, compact uses `24` at `ainb-tui/crates/ainb-core/src/components/home_screen_v2.rs:275`, and Home wheel routing uses `26` at `ainb-tui/crates/ainb-core/src/main.rs:431`.

## Prior Learnings

Relevant memory matched earlier AINB TUI work:

| Learning | Key Insight | Confidence |
| --- | --- | --- |
| AINB TUI architecture and dashboard work | Extend existing TUI screen/state surfaces instead of creating parallel subsystems. | Medium |
| Plugin/integration verification | Done means real tmux capture or terminal-visible behavior, not only green unit tests. | High |

## Codebase Analysis

### TUI Entry And Event Loop

- Entry point: `ainb-tui/crates/ainb-core/src/main.rs:85`.
- TUI launch: `ainb-tui/crates/ainb-core/src/main.rs:114`.
- Mouse capture enabled: `ainb-tui/crates/ainb-core/src/main.rs:189`.
- Main loop draw/input cadence: `ainb-tui/crates/ainb-core/src/main.rs:213`.
- Mouse branch: `ainb-tui/crates/ainb-core/src/main.rs:405`.

### Mouse Dispatch

- `AppEvent` has `MouseClick`, `MouseDragStart`, `MouseDragEnd`, and `MouseDragging` at `ainb-tui/crates/ainb-core/src/app/events.rs:65`.
- `MouseDragStart` is not emitted by the main loop today; `Down(Left)` emits `MouseClick`, `Drag(Left)` emits `MouseDragging`, and `Up(Left)` emits `MouseDragEnd`.
- `process_event` no-ops mouse variants because the main event loop is expected to process them directly at `ainb-tui/crates/ainb-core/src/app/events.rs:4815`.

### HomeScreen Layout

- Active HomeScreen render path: `ainb-tui/crates/ainb-core/src/app/screens/builtin.rs:455`.
- Home V2 state owns focus, sidebar state, welcome state, mascot at `ainb-tui/crates/ainb-core/src/components/home_screen_v2.rs:39`.
- Full layout splits sidebar/welcome with fixed `Constraint::Length(26)` at `ainb-tui/crates/ainb-core/src/components/home_screen_v2.rs:204`.
- Compact layout uses fixed `Constraint::Length(24)` at `ainb-tui/crates/ainb-core/src/components/home_screen_v2.rs:275`.
- Sidebar component has state for selection/focus/labels/count at `ainb-tui/crates/ainb-core/src/components/sidebar.rs:130`.

### Sessions Screen

- Sessions screen is not a registry full-screen component; it falls through to `LayoutComponent` split-pane fallback.
- Split-pane layout is 40 percent sessions, 60 percent logs/preview at `ainb-tui/crates/ainb-core/src/components/layout.rs:91`.
- Mouse focus logic mirrors that 40 percent split in `ainb-tui/crates/ainb-core/src/app/events.rs:411`.

### Config Persistence

- `AppConfig` includes `ui_preferences` at `ainb-tui/crates/ainb-core/src/config/mod.rs:234`.
- `UiPreferences` starts at `ainb-tui/crates/ainb-core/src/config/mod.rs:430`.
- Config loads merged project/user/system files in `ainb-tui/crates/ainb-core/src/config/mod.rs:535`.
- Config saves to user config in `ainb-tui/crates/ainb-core/src/config/mod.rs:569`.
- Step 1 should use this config path for sidebar width only; collapsed state and broader layout state remain deferred.

### Test And Validation Contract

- `just check` runs fmt-check, clippy, and tests in `ainb-tui/justfile:43`.
- Tmux tripwire skill requires detached tmux, isolated `HOME`, seeded onboarding, polling capture, and exact-session cleanup in `ainb-tui/.claude/skills/tmux-ui-tripwire/SKILL.md:19`.
- Existing non-plugin tripwire `tripwire_sessions_screen` shows the launch/poll pattern at `ainb-tui/crates/ainb-core/tests/tripwire_sessions_screen.rs:79`.
- No `.vhs` files were found in this worktree; docs mention `.tape` screenshots under `docs/assets/screenshots`.

## Recommendations

1. Keep Step 1 scoped to HomeScreen only.
2. Add sidebar width and drag lifecycle state to `HomeScreenV2State` or nested `SidebarState`.
3. Replace Home layout constants with state-derived width for full/standard/compact layouts.
4. Handle `HOME` mouse `Down/Drag/Up` either in the main mouse branch or `EventHandler::handle_mouse_event`.
5. Reuse actual rendered layout math for hit testing; do not revive BSP/tree/prefix-mode code.
6. Validate with a real tmux/terminal path before Step 2.

## Open Questions For Interview

- Should Step 1 include only drag-resize, or also clickable sidebar item selection?
- Should drag work only on full/standard HomeScreen layouts, or also compact layout?
- Where should the first state live: nested `SidebarState`, `HomeScreenV2State`, or top-level `AppState`?
- What exact min/max widths should Step 1 enforce before Step 2 collapse exists?
- Is tmux tripwire enough for automated acceptance, or do we also need manual `cargo run` proof before handoff?
