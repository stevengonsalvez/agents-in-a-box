# Research: Sessions Screen Trackpad Scroll Regression

**Date**: 2026-05-27 18:19:19 BST
**Repository**: agents-in-a-box
**Branch**: feat/ui
**Commit**: 9d3867c
**Research Type**: Codebase

## Research Question

Double-finger trackpad scroll used to scroll sessions in the sessions screen, but no longer works. Find why.

## Executive Summary

`SESSION_LIST` has no mouse-wheel branch. Crossterm `MouseEventKind::ScrollDown` and `ScrollUp` are handled centrally in `main.rs`, but the branch only special-cases Home, Git View, and Log History. Every other screen, including Sessions, falls through to the default live-log scrolling path, so trackpad scroll events do not move session selection or list offset.

The likely user-visible regression is caused by adding richer mouse capture/click handling without routing wheel events to the sessions pane. Tests cover click, double-click, resize, collapse, and expand, but no wheel/trackpad scroll behavior.

## Key Findings

- `main.rs` handles wheel events directly at `ainb-tui/crates/ainb-core/src/main.rs:422`.
- There is no `screen_ids::SESSION_LIST` branch in the wheel handler.
- The fallback at `ainb-tui/crates/ainb-core/src/main.rs:490` scrolls live logs, not sessions.
- Keyboard `Up`/`Down` already maps to `PreviousSession` and `NextSession` when the sessions pane has focus.
- Session mouse tests do not cover `ScrollUp` or `ScrollDown`.

## Detailed Findings

### Mouse Scroll Dispatch

Current dispatch:

- Home screen: scroll welcome panel only on right side.
- Git View: scroll diff or markdown.
- Log History: vertical or horizontal scroll.
- Everything else: scroll live logs.

Code reference:

- `ainb-tui/crates/ainb-core/src/main.rs:422` starts the wheel handler.
- `ainb-tui/crates/ainb-core/src/main.rs:428` handles Home.
- `ainb-tui/crates/ainb-core/src/main.rs:448` handles Git View.
- `ainb-tui/crates/ainb-core/src/main.rs:469` handles Log History.
- `ainb-tui/crates/ainb-core/src/main.rs:490` fallback scrolls live logs.

There is no equivalent branch for `screen_ids::SESSION_LIST`.

### Existing Sessions Navigation

Keyboard navigation already has the behavior needed for a low-risk mouse-wheel implementation:

- `ainb-tui/crates/ainb-core/src/app/events.rs:1147` maps Down to `NextSession` when `FocusedPane::Sessions`.
- `ainb-tui/crates/ainb-core/src/app/events.rs:1160` maps Up to `PreviousSession` when `FocusedPane::Sessions`.
- `ainb-tui/crates/ainb-core/src/app/events.rs:2326` processes `NextSession`.
- `ainb-tui/crates/ainb-core/src/app/events.rs:2330` processes `PreviousSession`.
- `ainb-tui/crates/ainb-core/src/app/state.rs:4767` implements `next_session`.
- `ainb-tui/crates/ainb-core/src/app/state.rs:4891` implements `previous_session`.

### Mouse Hit-Testing State

Recent sessions mouse work stores rendered pane geometry:

- `ainb-tui/crates/ainb-core/src/app/state.rs:465` records sessions and preview rects.
- `ainb-tui/crates/ainb-core/src/app/state.rs:545` can detect whether a coordinate is inside the sessions pane.
- `ainb-tui/crates/ainb-core/src/app/state.rs:555` can detect whether a coordinate is inside the preview pane.
- `ainb-tui/crates/ainb-core/src/app/state.rs:565` maps mouse coordinates to visible session-list rows.

This means wheel routing can use cached layout and stay in-memory.

### Test Gap

Current session mouse tests cover:

- click row selection
- double-click attach
- double-click guard
- drag resize
- collapse and expand

No test exercises wheel events or the `main.rs` crossterm scroll dispatch path.

## Recommendation

Add a `screen_ids::SESSION_LIST` branch inside `MouseEventKind::ScrollDown | MouseEventKind::ScrollUp` before the default live-log fallback.

Behavior:

1. If pointer is over sessions pane, set focus to `FocusedPane::Sessions` and process `NextSession` or `PreviousSession` `SCROLL_LINES` times.
2. If pointer is over preview pane, preserve current behavior by scrolling live logs.
3. If coordinates are unavailable or outside cached layout, fall back to focused pane: sessions focus scrolls session selection, live-log focus scrolls logs.

This keeps the hot path in-memory. It reuses existing selection methods and does not introduce async work, disk writes, tmux calls, or subprocess calls.

## Open Questions

- Should one trackpad tick move one session or three sessions? Existing wheel constant is `SCROLL_LINES = 3`; using it preserves app-wide scroll speed, but session lists may feel better at one row per tick.
- Should wheel over the preview always scroll logs even when sessions pane has focus? That matches IDE-style pointer-local behavior.
