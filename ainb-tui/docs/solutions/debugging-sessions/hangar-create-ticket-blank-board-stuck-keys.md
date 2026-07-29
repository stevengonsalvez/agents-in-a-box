---
id: lrn-hangar-create-ticket-blank-board-stuck-keys-a70c77
scope: project
confidence: 0.9
learning_type: bug-root-cause
source_episodes:
  - agent-a70c7709ecc04c235
superseded_by: null
provenance:
  source_tool: reflect
  project: ainb-tui
problem: "Creating a ticket in the Hangar TUI board leaves the board blank and q/Esc stop working, forcing a hard quit."
root_cause: "Card creation is a 4-stage overlay wizard (title→repo→agent→profile); Enter after typing only the title just advances to stage 2, so nothing is ever persisted, and while any overlay stage is open a global-key bypass at plugin.rs:2647-2655 routes every key (including q) into the overlay reducer and returns before the quit/routing layer is reached."
fix: "Partially resolved. Esc now fully closes the create overlay from every stage (boards.rs BoardsKey::Esc => close_overlay at 1410/1503/1557/1593/1616/1706/1816/1880), so the stuck-keys/force-quit symptom is gone. Still open: the overlay guard at plugin.rs:3988-3990 swallows q while an overlay is open (Esc is the escape), and a single Enter only advances wizard stages rather than submitting."
rule: "In ainb-tui's Hangar plugin, any UI wizard/overlay screen MUST NOT gate the global quit key (q) behind `overlay().is_some()` without an explicit escape hatch — check plugin.rs's overlay guard pattern before adding new multi-stage overlays."
category: debugging-sessions
entities:
  - ainb-plugin-hangar
  - boards.rs
  - app_screens.rs
  - plugin.rs
  - BoardsIntent::CreateCard
  - overlay guard
  - unix-socket JSON-RPC
causal_relations:
  - {source: "4-stage overlay wizard (single Enter only advances stage)", target: "blank board after ticket creation", type: caused_by}
  - {source: "global-key bypass at plugin.rs:2647-2655 while overlay open", target: "q/Esc stop working (stuck keys)", type: caused_by}
  - {source: "overlay guard returns before routing_event/quit layer", target: "q swallowed as typed char instead of quit", type: causes}
forget_after: null
---

# Hangar TUI: create-ticket leaves board blank, q/Esc stop working

## Problem
User creates a ticket in the Hangar Kanban board via the TUI (types title
"test", presses Enter). Board stays blank — no ticket appears — and the
screen becomes unresponsive: `q` and `Esc` do nothing, requiring a
force-quit.

## Root cause
Ticket creation is implemented as a **4-stage overlay wizard**
(title → repo → agent → profile) in
`crates/ainb-plugin-hangar/src/screen/boards.rs`. A single Enter after
typing the title (`card_title_key`, `boards.rs:1532`) does **not** submit
the ticket — it advances to the repo picker (`boards.rs:1560-1581`).
`BoardsIntent::CreateCard` is only raised at the *final* stage
(`card_profile_key`, Enter at `boards.rs:1817-1835`). Since the wizard
never reaches stage 4 from a single Enter, no DB insert happens
(`IssueRepo::insert` / `BoardRepo::card_add` in
`crates/ainb-hangar-daemon/src/rpc/mod.rs:2288-2361` is never called), so
the board correctly stays empty.

The "stuck keys" symptom is a separate, more serious bug: while *any*
overlay stage is open, a global-key bypass in
`crates/ainb-plugin-hangar/src/plugin.rs:2647-2655` intercepts every key
before it reaches the quit/routing layer:

```rust
if matches!(app.screen, Screen::Boards) && self.screens.boards.overlay().is_some() {
    let _ = route_key(&app, &mut self.screens, key);   // Result discarded
    return;                                            // never reaches routing layer
}
```

This means `q` is typed into the overlay input (or ignored) instead of
triggering quit (`routing_event`, `plugin.rs:3546-3558`). `Esc` only steps
back one wizard stage at a time (`boards.rs:1619-1625`, `1734-1742`,
`1806-1814`) — only Esc at stage 1 actually closes the overlay
(`boards.rs:1539`) — so users need several correctly-aimed Esc presses to
escape, and in practice give up and force-quit.

No blocking async call or hung task is involved — `apply_boards_action`
only awaits a non-blocking `unix_socket_send` (`plugin.rs:1948`).

## Resolution (partial, as of 2026-07-29)
The unrecoverable-UI symptom is fixed: `Esc` now fully closes the create
overlay from every stage. `boards.rs` handles `BoardsKey::Esc =>
close_overlay(state)` at each stage (1410, 1503, 1557, 1593, 1616, 1706,
1816, 1880), so a single Esc always escapes instead of stepping back one
stage at a time.

Still open:
1. The overlay guard (now `plugin.rs:3988-3990`) still routes every key into
   the overlay reducer and returns while an overlay is open, so `q` is
   swallowed rather than quitting; `Esc` is the working escape hatch.
2. Enter still only advances wizard stages (`card_title_key`), so a single
   Enter after typing the title does not submit the ticket; the board staying
   blank on one Enter is by design, not a bug.

## Anti-pattern
Gating the global quit key behind "is any overlay open" with no escape
hatch, combined with a multi-stage wizard where only the final Enter
persists — the two combine into an unrecoverable-feeling UI state.

## Error-swallowing found on the create path (secondary finding)
- `plugin.rs:1945-1947` — `let Ok(body) = encode_request(...) else { return; };` silently drops encode failures.
- `plugin.rs:1948-1950` — socket send failure is only `log_info`'d, never surfaced to the user.
- `plugin.rs:2653` — `let _ = route_key(...)` discards the routing result inside the overlay guard.
- `apply_boards` does surface daemon errors via `set_boards_error`, which preserves the existing board (`app_screens.rs:626-628`) — so the blank screen here is because create was never *sent*, not because an error was swallowed. But if a send ever does fail, `1945-1950` means it fails silently.

## Context
- Repo: `ainb-tui` (crate `ainb-plugin-hangar`, daemon `ainb-hangar-daemon`)
- Investigated via Explore agent, file:line-verified, no fix applied yet (investigation-only task).
