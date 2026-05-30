# Sessions Trackpad Scroll Fix Implementation Plan

## Overview

Restore two-finger trackpad and mouse-wheel scrolling on the sessions screen by routing `ScrollUp` and `ScrollDown` events to session selection when the pointer is over the sessions pane.

## Current State Analysis

`MouseEventKind::ScrollDown | ScrollUp` is handled centrally in `ainb-tui/crates/ainb-core/src/main.rs`, but it only branches for Home, Git View, and Log History. The sessions screen falls through to the default live-log scroll path, so trackpad scroll is consumed without changing the selected session.

## Desired End State

On `SESSION_LIST`:

- Wheel/trackpad over the sessions pane moves session selection up or down.
- Wheel/trackpad over the preview pane scrolls logs, preserving current behavior.
- Routing uses cached pane layout and existing selection functions.
- The event hot path performs only in-memory state updates.

### Key Discoveries

- `ainb-tui/crates/ainb-core/src/main.rs:422` owns crossterm mouse-wheel dispatch.
- `ainb-tui/crates/ainb-core/src/app/state.rs:545` can hit-test the sessions pane from cached layout.
- `ainb-tui/crates/ainb-core/src/app/state.rs:555` can hit-test the preview pane from cached layout.
- `ainb-tui/crates/ainb-core/src/app/state.rs:4767` and `:4891` already implement session up/down movement.
- `ainb-tui/crates/ainb-core/tests/sessions_mouse.rs` lacks wheel regression coverage.

## What We're NOT Doing

- Not changing Home, Git View, or Log History scroll behavior.
- Not adding new async work, tmux calls, config writes, or subprocess calls to mouse scroll dispatch.
- Not changing collapse/expand, resize, click, or double-click behavior.

## Implementation Approach

Add a small sessions-specific scroll helper on `AppState`, then call it from the `SESSION_LIST` branch in `main.rs`. The helper is unit-testable and keeps `main.rs` from owning detailed pane hit-testing logic.

## Phase 1: Sessions Wheel Routing
<!-- wave: 1 | depends_on: [] | files: [ainb-tui/crates/ainb-core/src/app/state.rs, ainb-tui/crates/ainb-core/src/main.rs, ainb-tui/crates/ainb-core/tests/sessions_mouse.rs] -->

### Overview

Route session-screen wheel events to either sessions selection or live-log scrolling based on pointer position.

### Changes Required

#### 1. Add Session Scroll Helper

**File**: `ainb-tui/crates/ainb-core/src/app/state.rs`

**Changes**:

- [x] Add `scroll_session_list_by_mouse`.
- [x] Return `true` when the event was handled by session selection.
- [x] Return `false` when caller should keep live-log scrolling.
- [x] Set focus based on pointer-local pane.

#### 2. Route Wheel Events

**File**: `ainb-tui/crates/ainb-core/src/main.rs`

**Changes**:

- [x] Add `SESSION_LIST` branch before default live-log fallback.
- [x] Call `scroll_session_list_by_mouse`.
- [x] If helper returns `false`, use current live-log scroll behavior.

#### 3. Add Regression Tests

**File**: `ainb-tui/crates/ainb-core/tests/sessions_mouse.rs`

**Changes**:

- [x] Test wheel down over sessions pane advances selected session.
- [x] Test wheel up over sessions pane moves selection upward.
- [x] Test wheel over preview pane returns false and leaves session selection unchanged.

### Success Criteria

#### Automated Verification

- [x] `cargo test -p ainb --test sessions_mouse -- --nocapture`
- [x] `cargo build -p ainb`
- [x] `git diff --check`

#### Manual Verification

- [ ] In sessions screen, two-finger scroll over sessions pane changes selected session.
- [ ] In sessions screen, two-finger scroll over preview pane scrolls preview/log area.

## Testing Strategy

Focused regression coverage belongs in `sessions_mouse.rs`, because the bug is specifically sessions mouse behavior. The new helper avoids needing a full terminal event-loop harness for most of the routing logic.

## Performance Considerations

The scroll helper uses cached layout rectangles and existing selection methods. It does not call disk, tmux, Docker, subprocesses, or async functions during the mouse event itself.

## References

- Research: `research/2026-05-27_18-19-19_sessions-trackpad-scroll-regression.md`
