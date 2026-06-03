# Specification: Interactive In-Place tmux Pane (ainb-tui)

**Generated from:** research/2026-06-02_11-07-19_tmux-pane-in-tui.md + DE critique
**Interview date:** 2026-06-02
**Branch:** feat/tmux-in-pane-2 (fresh off origin/main; stale feat/tmux-in-pane retired)
**Re-grounded:** 2026-06-02 on v1.3.1, then rebased + re-verified 2026-06-03 on v1.3.3.
Code under `ainb-tui/crates/ainb-core/src/`. Deps unchanged across both; preview poll
(5s capture) unchanged; the 2 `inner(&Margin)` + ~8 `frame.size()` P0 sites unchanged;
events.rs/state.rs churned (line refs are approximate — re-grep at implementation time).
**Version:** 1.1

## Executive Summary

Add an interactive, in-place embedded `tmux attach` to ainb-tui's existing 60%
preview pane, so the user can type into a Claude Code agent without the current
full-screen suspend→exec→detach round-trip. The feature is **purely additive**: tmux
session lifecycle, session creation, and the 5-second `tmux capture-pane` read-only
preview polling are all unchanged. tmux remains the session owner; the embed is an
ephemeral tmux *client* in a PTY, rendered as a terminal-cell widget while focused.

## Re-grounding deltas (origin/main v1.3.1, 2026-06-02)

The original research/critique analyzed a stale snapshot 1023 commits behind. Re-checked
against current origin/main — **all core findings hold; two items REDUCE scope:**

```
 STILL TRUE on main (design intact)        ALREADY DONE on main (scope shrinks)
 ──────────────────────────────────        ────────────────────────────────────
 preview = read-only Paragraph             event loop ALREADY decoupled:
   (capture-poll, 5s)                         tick_rate 33ms (poll/render) +
 full-screen attach (suspend/exec)            app_tick_rate 250ms (heavy work)
 PtyWrapper drops _child (dead)             bracketed paste ALREADY on +
 deps: ratatui0.26/vt1000.15/pty0.8           Event::Paste handled (main.rs:609)
```

| Re-grounded fact | Impact on plan |
|---|---|
| Code moved to `ainb-tui/crates/ainb-core/src/` (workspace, v1.3.1) | all paths re-pointed in Code References |
| Poll loop already 33ms, heavy work 250ms (`main.rs:250,260`) | DE perf-decouple concern mostly FREE — live PTY render rides the 33ms poll + a dirty flag; no new loop split needed |
| Bracketed paste on + `Event::Paste` handled (`main.rs:19,609`) | paste capture exists; embed only needs to FORWARD paste to the PTY + push keyboard-enhancement flags |
| `'i'` CONFIRMED free in the session-list handler `handle_key_event` (events.rs ~824-1324; taken keys: q c f n s a r e d x g p o) | bind `'i'`=interactive there; the other `'i'` bindings (onboarding ~1636, home-screen→GoToStats ~1989) are separate contexts. Ctrl+Q also free. Phase-3 tripwire guards regression. |
| `attached_terminal.rs` exists = Docker full-screen info+logs view (NOT a terminal emulator) | unrelated; avoid naming/UX collision with the new embed |
| `DetachTmuxSession` event still present (`events.rs:156`) | repurpose as the focus-release handler as planned |
| Deps unchanged on main (ratatui 0.26 etc.) | the 0.30 upgrade gap + Phase 0 plan still valid |

## Objectives

```
┌──────────────┐  'i'   ┌────────────────────┐  Ctrl+Q  ┌──────────────┐
│ read-only    │───────▶│ interactive embed  │─────────▶│ read-only    │
│ preview      │        │ tmux attach in PTY │          │ preview      │
│ (unchanged)  │◀───────│ pane expands wide  │          │ (unchanged)  │
└──────────────┘ auto-  └────────────────────┘  revert  └──────────────┘
         release on session-switch / quit / panic
```

### Primary Goals
- In-place interactive tmux attach in the preview pane (no full-screen takeover).
- Nothing about the current run model changes — additive only.
- tmux still owns sessions; embed client death never kills a session.

### Success Metrics
- User can press `i`, type into the agent, hit Ctrl+Q, and be back in the TUI with the
  session list still visible — no full-screen flash, no manual `Ctrl+B D`.
- Zero leaked `tmux attach` clients after focus cycles / session switches / quit /
  panic (verify via `tmux list-clients`).
- A subsequent full-screen `a` attach to the same agent shows the correct window size.

## Scope

### In Scope
- Phase 0: ratatui 0.26→0.30 + vt100 0.15→0.16 + portable-pty 0.8→0.9 dependency
  upgrade migration (prerequisite — chosen over vendoring).
- Focus mode `FocusedPane::Preview` with input precedence interactive > scroll > normal.
- Embed PTY running `tmux attach-session -t <name>`; vt100 parse; `tui-term`
  `PseudoTerminal` render in the focused pane.
- Keyboard + mouse + paste forwarding to the inner PTY while focused.
- Pane expansion to near-full-width while interactive; revert on release.
- `PtyWrapper` rewrite (owns `Child`, real `kill()` + `Drop`); process-global registry
  drained by the panic hook.
- One-key detach freebie: no-prefix `Ctrl+Q` → `detach-client` added to session config.

### Out of Scope
- tmux control mode `-CC` (rejected — single-pane-per-agent makes it unjustified).
- Changing the capture-pane preview cadence/mechanism.
- Multi-pane tmux layout mirroring.
- Retrofitting the Ctrl+Q detach binding onto already-running sessions (only new
  sessions get it via `configure_session`).

### Future Considerations
- Live PTY for non-focused visible previews (kept on cheap capture-poll for now).
- Revisit if ainb ever wants multi-pane sessions (would reopen `-CC`).

## Technical Requirements

### Architecture

```
 main loop (ratatui 0.30)                    decoupled reader task
 ┌─────────────────────────┐                 ┌──────────────────────┐
 │ KeyEvent / MouseEvent    │                 │ master.read() 8KB     │
 │  focused? ─yes─▶ encode ──┼──bytes──▶ PTY ──┤ batch ─▶ vt100 parse  │
 │           └─no─▶ TUI nav  │   master       │ ─▶ set dirty flag      │
 │ render: PseudoTerminal    │◀───Screen───────┤ (rides 33ms poll loop │
 │  on dirty flag            │                 │  — already decoupled) │
 └─────────────────────────┘
         tmux attach-session -t <name>  ◀── ephemeral client (PTY)
         capture-pane poll (5s, UNCHANGED) ◀── still runs in parallel
```

### Components (paths relative to `ainb-tui/crates/ainb-core/src/`)
| Component | Purpose | Change |
|-----------|---------|--------|
| `PtyWrapper` (tmux/pty_wrapper.rs) | own embed child + kill/Drop | **rewrite** (still drops `_child` @:38) |
| `TmuxPreviewPane` (components/tmux_preview.rs) | render embed vs read-only | add interactive render branch |
| `FocusedPane` state (app/state.rs) | focus enum + precedence | **new** |
| key/mouse forwarder | KeyEvent→bytes, paste, mouse | **new** (lift smux.rs table) |
| layout expand (components/layout.rs) | widen pane while focused | add focused branch |
| panic hook + cleanup (main.rs) | drain embed registry | extend (`setup_panic_handler` @:89) |
| `configure_session` (tmux/session.rs:140) | add `bind -n C-q detach-client` | 1 additive line |
| `ainb-tui/Cargo.toml` (+ ainb-core) | ratatui 0.30, vt100 0.16, portable-pty 0.9, tui-term 0.3.4 | upgrade + add |

### Integrations
- tmux: embed = `tmux attach-session -t <name>` client; freebie = no-prefix Ctrl+Q
  `detach-client` binding on new sessions. Capture-pane poll untouched.
- crossterm: bracketed paste is ALREADY enabled and `Event::Paste` handled
  (`main.rs:19,609`); on focus-enter additionally push keyboard-enhancement flags +
  forward paste + route mouse to the inner PTY; restore on release.

### Performance Requirements
- Live PTY reader on its own task; 8KB batched reads → vt100 → dirty flag.
- Render on the dirty flag — the event loop is ALREADY split (`tick_rate` 33ms poll/
  render at `main.rs:250`, `app_tick_rate` 250ms heavy work at `:260`), so the live
  render rides the existing 33ms loop; no new decoupling required.
- Two clients (capture-pane read + attach client) on one session = no data conflict.

### Security Requirements
- No new attack surface; embed runs the same `tmux attach` the user already runs.

## User Experience

### Keymap
```
 NORMAL          SCROLL            INTERACTIVE (focused)
 ──────          ──────            ─────────────────────
 i ─▶ interactive  ↑↓/PgUp scroll   all keys ─▶ inner PTY
 a ─▶ full-screen  Esc ─▶ normal    mouse   ─▶ inner PTY
 Shift+↑↓ ─▶ scroll                 paste   ─▶ inner PTY (bracketed)
 (unchanged nav)                    Ctrl+Q  ─▶ release (ainb-intercepted)
 precedence: interactive > scroll > normal
```

| Key | Mode | Action |
|-----|------|--------|
| `i` | normal | enter interactive in-place embed (pane expands) |
| `Ctrl+Q` | interactive | ainb intercepts → kill embed client → revert pane + layout |
| `a` | normal | full-screen attach (UNCHANGED) |
| `Ctrl+Q` | full-screen attach | tmux no-prefix binding → detach-client (freebie) |
| `Ctrl+B D` | full-screen attach | still works (unchanged) |

### User Flows
1. **Interactive type**: select session → `i` → pane widens, live terminal → type →
   `Ctrl+Q` → back to TUI, list still visible.
2. **Switch while focused**: `i` → navigate away → auto-release (kill client, revert) →
   switch.
3. **Quick detach (full-screen)**: `a` → work → `Ctrl+Q` → back (no two-chord).

### Edge Cases
| Scenario | Expected Behavior |
|----------|-------------------|
| Navigate to another session while focused | auto-release embed, revert, then switch |
| Quit ainb while focused | drain registry: kill embed client, then quit |
| ainb panic while focused | panic hook kills embed child before terminal restore |
| Focus a session with no tmux session | no-op / stay read-only (guard) |
| `tmux attach` fails on enter (session gone) | flash brief notice, stay read-only (don't enter focus) |
| Embed session dies / agent exits while focused | auto-release, flash brief notice, revert to read-only preview (then shows empty/gone state) |
| Focused pane visual | distinct border + `● INTERACTIVE — Ctrl+Q release` title badge |
| Mouse event while focused | forwarded to inner PTY; ainb owns mouse when not focused |
| Multi-line paste while focused | bracketed paste ESC[200~…ESC[201~ (no premature submit) |

## Constraints & Dependencies

### Technical Constraints
- Must not change tmux lifecycle, session creation, or capture-pane poll.
- Ctrl+Q detach binding applies only to sessions created after the update.

### External Dependencies
- ratatui 0.30 (split core/widgets), vt100 0.16, portable-pty 0.9, tui-term 0.3.4.

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| ratatui 0.26→0.30 migration | Low (MEASURED) | Low | Trial `cargo check` 2026-06-02: only 2 hard errors (`inner(&Margin)`→`inner(Margin)`) + 9 `frame.size()`→`area()` renames; crossterm 0.27→0.29 clean; deps co-resolve. Residual: test-target breakage (check after the 2 lib fixes) |
| Leaked `tmux attach` clients | Med | Med | PtyWrapper owns Child + kill/Drop + registry drained by panic hook + `detach-client` belt-and-suspenders |
| window reflow churn / Claude repaint | Med | Med | expand pane on focus, size embed to expanded rect; revert on release |
| live render lag or CPU spin | Low | Med | reader task + dirty flag; rides the existing 33ms poll loop (already split from the 250ms heavy-work tick) |
| Ctrl+Q shadows flow-control/XON for inner apps | Low | Low | documented; alternate key available if it bites |
| partial key-encoding drops modified/F-keys | Med | Med | lift smux.rs key table verbatim |
| three input regimes collide | Med | Low | single explicit FocusedPane enum, precedence interactive>scroll>normal |

## Decisions Made

- **Keymap**: `a` stays full-screen; new `i` = in-place; Ctrl+Q releases. *Rationale:*
  additive, nothing existing changes.
- **Resize**: expand pane to near-full-width while interactive, revert on release.
  *Rationale:* usable terminal + deliberate one-time sensible size, avoids cramped 60%
  reflow churn.
- **Detach freebie**: ship one-key `Ctrl+Q` detach via no-prefix tmux binding in
  `configure_session`; Ctrl+B D still works. *Rationale:* free win, symmetric "get me
  out" everywhere; must be a tmux binding because ainb is suspended during full-screen
  attach.
- **Mouse**: forward to inner PTY while focused. *Rationale:* makes the embed feel like
  a real terminal.
- **Deps**: **upgrade ratatui to 0.30** + vt100 0.16 + portable-pty 0.9, use `tui-term`
  as a real dep (NOT vendor). *Rationale (user override of DE's vendor recommendation):*
  fewer vendored LOC, cleaner long-term, real upstream widget. *Accepted cost:* large
  one-time migration blast radius — isolated to Phase 0.
- **On switch/quit/panic**: auto-release (kill client, not session), cleanup wired into
  panic hook. *Rationale:* no leaks, fluid.
- **Embed source**: `tmux attach-session -t <name>` (the same session ainb manages) —
  NOT a direct-PTY run of the agent. *Rationale:* preserves detach/reattach persistence
  across ainb restart; keeps tmux working exactly as today.
- **Focus cue**: distinct border color + title badge `● INTERACTIVE — Ctrl+Q release`
  on the pane while focused (read-only preview keeps its dim border). *Rationale:*
  unmistakable mode signal; keybinding hint sits on the control, not a global help bar.
- **Embed death** (attach fails / session dies while focused): auto-release, flash a
  brief notice in the pane, revert to the read-only preview (which then shows the
  empty/gone state). *Rationale:* graceful, never a stuck dead pane.
- **Capture poll while focused**: leave the existing 5s capture-pane poll UNCHANGED for
  all sessions, including the focused one. *Rationale:* honors "don't change anything";
  the redundant 5s capture is harmless (two clients on one session is fine).

### Deferred Decisions
- Alternate detach key if Ctrl+Q/XON overlap proves annoying — defer until observed.

## Implementation Notes

### Priority Order (gated phases)
1. **Phase 0 — dependency migration (prerequisite, own PR):** ratatui 0.26→0.30, vt100
   0.16, portable-pty 0.9; fix all components/* + widgets/* + tests; green gate.
2. **Phase 1 — PtyWrapper rewrite + lifecycle:** own Child, kill/Drop, process-global
   registry, panic-hook drain. Unit-test cleanup.
3. **Phase 2 — embed render:** spawn `tmux attach` PTY, reader task + vt100 + dirty
   flag (rides the existing 33ms poll loop), `tui-term` PseudoTerminal render branch in
   TmuxPreviewPane.
4. **Phase 3 — focus mode + input:** FocusedPane enum + precedence, `i` enter (verify
   free vs events.rs:1587) / Ctrl+Q release, key table (smux.rs), keyboard-enhancement
   flags + forward paste (paste capture already exists), mouse routing, auto-release on
   switch/quit.
5. **Phase 4 — pane expansion:** widen layout while focused, size embed to rect, revert.
6. **Phase 5 — detach freebie:** `bind -n C-q detach-client` in configure_session.
7. **Phase 6 — verify:** tmux list-clients leak check, full-screen-after-embed size
   check, perf under Claude Code burst, all DE conditions.

### DE Conditions Mapped
- PtyWrapper Child+kill+Drop → Phase 1.
- panic-hook PTY-awareness → Phase 1.
- expand/size to avoid reflow → Phase 4.
- explicit input-regime enum + locked keymap → Phase 3.
- dirty-flag render on the existing 33ms poll loop (decoupling already shipped on main) → Phase 2.
- deps: upgrade (user choice) → Phase 0.

### Technical Debt Accepted
- ratatui 0.30 migration — MEASURED small (2 errors + 9 renames; crossterm clean),
  one-time, isolated to Phase 0. (Earlier "large/biggest-risk" framing was an estimate,
  falsified by the trial `cargo check`.)

## Open Questions
- [ ] Exact ainb key bound to "navigate away" that should trigger auto-release (covered
      by the FocusedPane precedence; confirm during Phase 3).
- [ ] Confirm tui-term 0.3.4 renders correctly against ratatui 0.30 final (validate
      early in Phase 0/2).

---

*Generated through systematic interview of the design forks surfaced by the DE critique.*
