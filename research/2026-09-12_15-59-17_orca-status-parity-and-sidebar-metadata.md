# Research: Orca status parity and sidebar metadata mode

**Date**: 2026-09-12 15:59:17
**Repository**: agents-in-a-box--f-improv43e2f972--cd2a9e8e
**Branch**: f/improv43e2f972
**Commit**: ed558a71
**Research Type**: Comprehensive

## Research Question

Compare AINB session lifecycle status with Orca, diagnose stagnant `RUN`, and
choose a compact model/effort display interaction.

## Executive Summary

AINB correctly receives provider lifecycle facts, but sidebar rendering trusts
retained `RUNNING` and `TURN_COMPLETE` facts forever. Orca expires old working
evidence, retaining only a non-committal live/idle state; AINB should reuse its
existing five-minute healthy-state freshness policy. A literal held key is not
reliable in macOS terminals, so `v` toggles the one-line model/effort view.

## Key Findings

- `skill-hub` has an attachable tmux pane, but its `RUNNING` lifecycle is over
  26 hours old and has no active tracked work.
- Orca treats only explicit provider evidence as agent status and decays stale
  work to an unverifiable live state, never continued working.
- AINB already stores `lifecycle_updated_at` and already has a five-minute
  freshness tunable; the sidebar simply does not consume either.

## Detailed Findings

### Codebase Analysis

#### Lifecycle authority

- Current implementation: [`app/state.rs:12671`](https://github.com/stevengonsalvez/agents-in-a-box/blob/ed558a71/ainb-tui/crates/ainb-core/src/app/state.rs#L12671)
- `Starting`/`Running` project to local `Running`; `TurnComplete`/`Idle` project
  to local `Idle`.
- Refresh accepts any reachable Fleet lifecycle without checking its timestamp:
  [`app/state.rs:12758`](https://github.com/stevengonsalvez/agents-in-a-box/blob/ed558a71/ainb-tui/crates/ainb-core/src/app/state.rs#L12758).
- Sidebar then calls any reachable `Starting`/`Running` state `RUN`, regardless
  of `lifecycle_updated_at`: [`session_list.rs:1246`](https://github.com/stevengonsalvez/agents-in-a-box/blob/ed558a71/ainb-tui/crates/ainb-core/src/components/session_list.rs#L1246).
- Existing `fleet.healthy_state_stale_ms` defaults to five minutes and is the
  project policy for stale health state:
  [`config/mod.rs:560`](https://github.com/stevengonsalvez/agents-in-a-box/blob/ed558a71/ainb-tui/crates/ainb-core/src/config/mod.rs#L560).

#### `skill-hub` diagnosis

| Field | Observed value |
|---|---|
| Lifecycle | `RUNNING` |
| Lifecycle timestamp | Sep 11 13:12 BST |
| Age at investigation | More than 26 hours |
| Tracked active work | `0` |
| Later lifecycle fact | None |

`Stop` with background work intentionally retains `Running` in the Hangar
reducer, but its synthetic work did not later produce a terminal event:
[`fleet.rs:2482`](https://github.com/stevengonsalvez/agents-in-a-box/blob/ed558a71/ainb-tui/crates/ainb-hangar-daemon/src/fleet.rs#L2482).
This is a stale-evidence rendering bug, not an attachability or tmux bug.

#### Sidebar interaction

- Session rows currently emit two physical lines, with model/effort on the
  second: [`session_list.rs:643`](https://github.com/stevengonsalvez/agents-in-a-box/blob/ed558a71/ainb-tui/crates/ainb-core/src/components/session_list.rs#L643).
- `Shift+M` is already the persistent bottom-keymap toggle:
  [`events.rs:2531`](https://github.com/stevengonsalvez/agents-in-a-box/blob/ed558a71/ainb-tui/crates/ainb-core/src/app/events.rs#L2531).
- Default macOS/Linux input deliberately drops key releases because those
  terminals emit only presses: [`main.rs:581`](https://github.com/stevengonsalvez/agents-in-a-box/blob/ed558a71/ainb-tui/crates/ainb-core/src/main.rs#L581).
  A literal hold action would therefore fail in the user’s terminal. `v` is
  unbound in Session List and is the safe toggle for model/effort mode.

### External Research

- Orca owns status from explicit provider events only, with
  `working`, `blocked`, `waiting`, and `done` as the shared vocabulary:
  [agent status types](https://github.com/stablyai/orca/blob/403b62a8d8fa6e896a93acc4c15405be0f0b7dc7/src/shared/agent-status-types.ts#L1-L27).
- Orca preserves original evidence time across replay and does not make replay
  receipt time fresh:
  [status application](https://github.com/stablyai/orca/blob/403b62a8d8fa6e896a93acc4c15405be0f0b7dc7/src/main/agent-hooks/server/server-status-application.ts#L19-L79).
- Stale non-done evidence decays; a live PTY becomes `unverifiable`, otherwise
  `idle`:
  [row decay reducer](https://github.com/stablyai/orca/blob/403b62a8d8fa6e896a93acc4c15405be0f0b7dc7/src/renderer/src/lib/agent-row-decay-state.ts#L7-L31).
- Claude and Codex hooks map prompts/tools to working, user/permission requests
  to waiting, and stop to done:
  [Claude mapping](https://github.com/stablyai/orca/blob/403b62a8d8fa6e896a93acc4c15405be0f0b7dc7/src/shared/agent-hook-listener/providers/claude-events.ts#L43-L116),
  [Codex mapping](https://github.com/stablyai/orca/blob/403b62a8d8fa6e896a93acc4c15405be0f0b7dc7/src/shared/agent-hook-listener/providers/codex-events.ts#L102-L178).

## Code References

- `ainb-tui/crates/ainb-core/src/app/state.rs:12671` — Fleet lifecycle projection.
- `ainb-tui/crates/ainb-core/src/components/session_list.rs:1246` — sidebar lifecycle labels.
- `ainb-tui/crates/ainb-hangar-daemon/src/fleet.rs:2469` — Claude hook lifecycle reducer.
- `ainb-tui/crates/ainb-hangar-daemon/src/fleet.rs:1216` — Codex lifecycle reducer.

## Recommendations

1. Treat Fleet lifecycle as authoritative only while daemon is reachable and
   `lifecycle_updated_at` is within `fleet.healthy_state_stale_ms`.
2. Render stale attachable work as `LIVE`, never `RUN`; do not turn stale work
   into `DONE`.
3. Keep explicit `ASK`, `WAIT`, `APPROVE`, and `ERR` ahead of lifecycle state.
4. Make every session one physical line; use `v` to toggle titles between
   prefix/session name and `model / effort`.
5. Use distinct semantic colours: `RUN` green, `DONE` cyan, `WAIT` amber,
   `ASK`/`APPROVE`/`ERR` red, `IDLE` white, `LIVE`/`STOP` grey.

## Open Questions

- [ ] None. Implement recommended options.
