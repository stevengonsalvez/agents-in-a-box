---
id: lrn-ainb-tui-mouse-perf-ba7c57
title: AINB TUI mouse event hot paths
category: performance-issues
key_insight: Keep mouse move and drag handlers in-memory only; diagnose redraw scheduling separately when terminal mouse events still cause jitter.
scope: project
confidence: high
learning_type: performance-review
source_episodes:
  - mouse-first-tui-session-2026-05-24
superseded_by: null
provenance:
  branch: feat/ui
  merge_commit: ba7c57c
---

## Problem

Mouse-first TUI behavior can make the UI feel jittery if move or drag paths do heavy work, schedule async actions, persist configuration, or trigger subprocess/Docker/tmux calls.

## Solution

Keep `MouseMove` and drag handlers strictly in-memory. They should update hover state, drag state, cached layout rectangles, and selection indexes only. Persist pane width only on mouse-up after an active resize. Persist collapse or expand state only on click.

Use cached render layout for hit-testing and resize bounds. Terminal size calls are acceptable as a fallback, but should not be the primary resize data source for high-rate drag paths.

## Anti-Pattern

Do not write config, start async fetches, call Docker/tmux, or allocate large lists from move/drag handlers. Single-click selection may schedule existing preview work, but hover and resizing must stay cheap.

## Context

The mouse-first sessions work was performance-reviewed after adding sidebar resize, collapse/expand, session row click, and double-click attach. The mouse handlers themselves were cheap; the remaining jitter risk was the broader event loop pattern where high-rate terminal mouse events can still drive redraw pressure.
