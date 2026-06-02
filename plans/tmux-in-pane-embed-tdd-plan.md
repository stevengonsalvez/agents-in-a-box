# TDD Plan: Interactive In-Place tmux Pane (ainb-tui)

**From spec:** plans/tmux-in-pane-embed-spec.md (re-grounded on origin/main v1.3.1)
**Branch:** feat/tmux-in-pane-2
**Code root:** `ainb-tui/crates/ainb-core/src/`
**Date:** 2026-06-02

Every phase is RED → GREEN → REFACTOR: failing tests first, minimal impl to green,
then clean up. Each test below is tagged with its locked type.

## Testing Strategy (locked via /interview)

```
 RENDER ──── BOTH layers: vt100 Screen-cell asserts (vt100_helper)
                          + ratatui TestBackend buffer asserts
 REALNESS ── HYBRID: real tmux+PTY for lifecycle/leak/e2e;
                     pure-fn units for encoder + state machine
 LEAK ────── BOTH: unit (Drop kills stub child) + e2e (tmux list-clients)
 ENCODER ─── BOTH: exhaustive table tests + proptest invariants
```

| Type tag | Tool / infra | Disclosure |
|----------|--------------|------------|
| `unit-pure` | std `#[test]`, pretty_assertions — no tmux, no PTY | "// pure (no tmux)" |
| `unit-proc` | spawns a real child process (sleep/cat) but NO tmux | "// real child, no tmux" |
| `prop` | **proptest** (NEW dev-dep) — invariants over generated input | "// property" |
| `render-vt100` | `vt100::Parser` + tests/helpers/vt100_helper.rs | "// pure (no tmux)" |
| `render-buf` | ratatui `TestBackend` buffer (4 files already use it) | "// pure (no tmux)" |
| `e2e-tmux` | **rexpect** + real tmux, extends tests/behavioral/tmux_lifecycle.rs + tests/e2e_pty_tests.rs | "// REAL tmux" |

**Disclosure rule:** every test fn carries a one-line comment marking real-tmux vs pure
(per the mocked-vs-live disclosure habit). CI must run `cargo test --workspace` so the
`crates/ainb-core/tests/` integration + e2e suites are not silently skipped.

## Test File Organization

```
crates/ainb-core/
├── src/
│   ├── tmux/pty_wrapper.rs        # + #[cfg(test)] mod tests (unit-proc: Drop/kill)
│   ├── tmux/embed_client.rs       # NEW: reader task + vt100 parser owner
│   ├── components/tmux_preview.rs # + interactive render branch (render-buf)
│   ├── app/embed_input.rs         # NEW: KeyEvent->bytes encoder (unit-pure + prop)
│   └── app/state.rs               # FocusedPane enum (unit-pure: precedence)
└── tests/
    ├── helpers/vt100_helper.rs    # reuse for render-vt100
    ├── behavioral/tmux_lifecycle.rs   # extend: leak/detach e2e
    ├── e2e_pty_tests.rs           # extend: embed attach + input round-trip
    └── embed_acceptance.rs        # NEW: Phase 6 full leak sweep + user-visible demo
```

Add to `crates/ainb-core/Cargo.toml [dev-dependencies]`: `proptest = "1"` (Phase 3;
may ride the Phase 0 dep PR to keep dep churn in one place).

---

## Phase 0 — Dependency upgrade (gated prerequisite PR, NO feature code)

**Nature (MEASURED — trial `cargo check` on bumped deps, 2026-06-02):** SMALL, not the
big rock first estimated. Bumping the 5 coupled deps (ratatui 0.30.0, crossterm 0.29.0,
vt100 0.16.2, portable-pty 0.9.0, ansi-to-tui 8.0.1; tui-term 0.3.4 added in P2) yields
exactly **2 hard errors + 9 deprecation renames**. crossterm 0.27→0.29 (the bit flagged
as uncertain) compiled CLEAN. Do NOT re-plan the event-loop decouple (already 33ms/250ms)
or bracketed paste (already on).

TDD here = "the existing suite is the test; keep it green."

| Step | Action |
|------|--------|
| RED | Bump deps in `ainb-tui/Cargo.toml [workspace.dependencies]`: ratatui 0.30, crossterm 0.29, vt100 0.16, portable-pty 0.9, ansi-to-tui 8.0.1. Run `cargo check --workspace --all-targets`. (Trial already enumerated the failing state.) |
| GREEN | The 2 hard fixes: `area.inner(&Margin{..})`→`area.inner(Margin{..})` at `components/claude_chat.rs:42` and `components/tmux_preview.rs:232`. The 9 clippy-gate renames: `frame.size()`→`frame.area()` in `components/layout.rs` (lines 67,96,143,150,155,161,166,170 + 1). Then re-run `--all-targets` to surface any TEST-only breaks (residual unknown: vt100 0.16 moved `set_size` Parser→Screen — `tests/helpers/vt100_helper.rs` may need it; possible TestBackend tweaks) and fix those. |
| + new test | `render-buf` smoke: render `tui_term::widget::PseudoTerminal::new(&parser.screen())` of a trivial `vt100::Parser` into a `TestBackend` and assert the buffer shows the expected glyphs — proves tui-term integrates with ratatui 0.30. |
| REFACTOR | none beyond the ~11 edits; keep the diff mechanical. |
| GATE | `cargo test --workspace` green + `cargo clippy -- -D warnings` clean. Can be the first commit of the feature PR (no longer needs to be a separate gated PR, given the measured size). |

Acceptance: full pre-existing suite green on the new deps; clippy clean; tui-term smoke
render passes. (Measured non-risk: crossterm event/key/mouse compiled clean.)

---

## Phase 1 — PtyWrapper rewrite + lifecycle/registry/panic-hook

**Why first (after deps):** DE's #1 risk is leaked clients. Build the kill machinery and
its proof before anything spawns a real `tmux attach`.

```
RED (unit-proc, no tmux):
  - pty_wrapper: spawn `sleep 60`; assert child alive (try_wait()==None);
    call kill(); assert dead (waitpid/ kill -0 -> ESRCH).
  - pty_wrapper: spawn `cat`; drop the wrapper; assert child dead (Drop kills).
  - registry: register 2 stub children; drain(); assert both killed + registry empty.
  - idempotency: kill() then drop() does not panic / double-free.
GREEN:
  - Rewrite PtyWrapper: retain `Box<dyn Child + Send>`; add `kill()`; impl `Drop`
    that kills (was: dropped `_child` @pty_wrapper.rs:38).
  - Process-global registry: `static EMBED_CHILDREN: Lazy<Mutex<Vec<..>>>`; register on
    spawn, remove on explicit kill, drain-all helper.
  - Panic hook (`setup_panic_handler` main.rs:89): drain registry BEFORE cleanup_terminal.
REFACTOR:
  - Single kill path shared by explicit kill + Drop + registry drain.
```

Acceptance: all unit-proc leak/kill/registry tests green; a focused test that invokes
the panic-hook drain path kills a registered child. Test type: `unit-proc`.

---

## Phase 2 — Embed render (vt100 → PseudoTerminal)

```
RED (render-vt100, pure):
  - Feed known bytes ("\x1b[31mHI\x1b[0m") to vt100::Parser; assert via vt100_helper
    that Screen cell (0,0)='H' fg=red, (0,1)='I', etc.
RED (render-buf, pure):
  - PseudoTerminal::new(&screen) rendered into TestBackend(rect); assert the buffer
    contains "HI" with red fg at the right cells; assert border/title of the pane.
RED (e2e-tmux, real):
  - Spawn a real tmux session running `printf 'EMBEDMARK\n'`; attach a PTY via the
    Phase-1 PtyWrapper; pump reader; assert the vt100 Screen contains "EMBEDMARK".
GREEN:
  - NEW embed_client.rs: reader task — 8KB batched master reads → vt100::Parser.process
    → set an AtomicBool dirty flag. (Rides the existing 33ms poll loop; no new decouple.)
  - tmux_preview.rs: add interactive render branch — when the pane is focused + an embed
    client exists, render PseudoTerminal from the live Screen instead of the Paragraph.
  - Spawn `tmux attach-session -t <name>` through PtyWrapper on focus-enter.
REFACTOR:
  - Extract an `EmbedPane { client, parser, dirty }` struct off TmuxPreviewPane.
```

Acceptance: render-vt100 + render-buf green; the real-tmux render e2e shows captured
output in the parsed Screen. Read-only Paragraph path unchanged when NOT focused.

---

## Phase 3 — Focus mode + input forwarding + mouse + auto-release

```
RED (unit-pure):
  - FocusedPane precedence: construct state, assert interactive > scroll > normal
    routing (a key that scroll-mode would eat goes to the PTY when interactive).
  - 'i'-is-free TRIPWIRE: in the session-list/preview context, pressing 'i' yields the
    NEW EnterInteractive event — NOT whatever events.rs:1587 ('i'/'I') does elsewhere.
    (Guards the keybinding collision flagged in re-grounding.)
RED (unit-pure, encoder — exhaustive table):
  - One case per mapping: Enter->[0x0A], Backspace->[0x08], Tab->[0x09], Esc->[0x1B],
    Left->[0x1B,'[','D'] (+ Right/Up/Down), Home/End/PgUp/PgDn/Delete/Insert/BackTab,
    Ctrl-C->[0x03], Ctrl-D->[0x04], Ctrl-Z->[0x1A], printable 'a'->[0x61].
RED (prop, encoder invariants):
  - proptest: any printable ASCII char with no modifiers -> its own byte.
  - proptest: Ctrl + ['a'..='z'] -> a single byte in 0x01..=0x1A.
RED (e2e-tmux, real — round-trip + release):
  - Enter interactive on a real session running a shell; forward keys "echo OK\n";
    capture-pane; assert "OK" present. Forward a bracketed paste; assert no premature
    submit. Send Ctrl+Q; assert focus released (TUI nav keys work again) and the
    embed client is torn down (tmux list-clients no longer shows it) while session lives.
GREEN:
  - FocusedPane enum in state.rs + precedence routing in the main loop.
  - embed_input.rs encoder fn (lift the smux.rs table).
  - On focus-enter: push keyboard-enhancement flags; forward Event::Paste bytes
    (bracketed) to PTY (paste is ALREADY captured at main.rs:609 — just forward).
  - Route mouse events to the PTY while focused; restore flags/mouse on release.
  - Auto-release on session-switch / quit (drain via Phase-1 registry).
  - Repurpose the no-op DetachTmuxSession event (events.rs:156) as focus-release.
REFACTOR:
  - One input-router that owns the focused/scroll/normal decision.
```

Acceptance: state-machine + encoder table + proptest green; 'i'-free tripwire green;
round-trip + release e2e green. Test types: `unit-pure`, `prop`, `e2e-tmux`.

---

## Phase 4 — Pane expansion while focused

```
RED (render-buf):
  - Not focused: layout gives the preview the 40/60 split (unchanged).
  - Focused: layout gives the embed pane near-full-width; assert the rendered Rect
    width + that the session list is hidden/narrowed; assert revert on release.
RED (unit-pure):
  - On focus-enter the embed PTY is resized to the EXPANDED rect cols/rows: capture the
    resize calls (master.resize + vt100 set_size) and assert dims match the expanded Rect.
RED (e2e-tmux, optional):
  - After expand, the inner program reflows: capture-pane shows content using the wider
    width (e.g. a line that only fits when wide).
GREEN:
  - layout.rs focused branch: widen the pane (collapse/narrow the 40% list).
  - Size embed PTY to the expanded rect on enter; revert layout + size on release.
REFACTOR:
  - Single "compute embed rect" helper used by both render + resize.
```

Acceptance: render-buf geometry (expanded vs reverted) green; resize-propagation unit
green; reflow e2e green. Test types: `render-buf`, `unit-pure`, `e2e-tmux`.

---

## Phase 5 — Ctrl+Q detach-client freebie (full-screen flow)

```
RED (e2e-tmux):
  - Create a session via configure_session; attach a client; send Ctrl+Q (no prefix);
    assert `tmux list-clients` shows the client GONE while the session SURVIVES.
  - Regression: Ctrl+B then D still detaches (unchanged).
GREEN:
  - Add `bind-key -n C-q detach-client` to configure_session (tmux/session.rs:140).
REFACTOR: none.
```

Acceptance: Ctrl+Q detaches the full-screen client; Ctrl+B D regression green. Only new
sessions get the binding (documented). Test type: `e2e-tmux`.

---

## Phase 6 — Verify / acceptance

```
e2e-tmux (LEAK SWEEP — DE #1, the headline acceptance):
  - N focus cycles + session switches + quit + simulated panic; assert ZERO leaked
    ephemeral clients (`tmux list-clients` per session) and ALL sessions still alive.
perf (measure, don't assert vibes):
  - Drive a high-rate burst (e.g. `yes` for 2s) into the embed; assert the render loop
    keeps up — dirty-flag coalescing means frames << bytes; record the number, no spin.
e2e-tmux (USER-VISIBLE acceptance):
  - Scripted rexpect (or vhs) demo: select session → 'i' → type → Ctrl+Q → assert the
    session list is visible again and the typed text reached the agent. (Feature
    acceptance must be user-visible, not just unit-green.)
regression:
  - crossterm 0.28 event/key/mouse behavioural tests stay green.
```

Acceptance gate for the whole feature: leak sweep green, user-visible demo green,
`cargo test --workspace` green, `cargo clippy -- -D warnings` clean.

---

## DE-condition → test mapping (proof, not assertion)

| DE condition | Proven by |
|--------------|-----------|
| PtyWrapper Child+kill+Drop | Phase 1 `unit-proc` Drop/kill tests |
| panic-hook PTY-awareness | Phase 1 panic-drain test + Phase 6 simulated-panic leak sweep |
| no leaked clients (#1 risk) | Phase 1 unit + Phase 6 `e2e-tmux` leak sweep (BOTH) |
| expand/size avoids reflow churn | Phase 4 render-buf geometry + resize-propagation unit |
| explicit input-regime + locked keymap | Phase 3 precedence unit + 'i'-free tripwire |
| render rides existing 33ms loop | Phase 2 reader-task dirty-flag; Phase 6 perf burst |
| deps upgrade (Phase 0) | full existing suite green on 0.30 + tui-term smoke |

## Cross-cutting gates

- Run `cargo test --workspace` (NOT `-p ainb-core` alone) so `tests/` e2e isn't skipped.
- Real-tmux tests require tmux on the CI runner (already true: tmux_lifecycle +
  e2e_pty_tests exist on main).
- Each test fn discloses `// REAL tmux` vs `// pure (no tmux)`.
- Don't guess byte/cell constants — run a new tripwire once with a wrong expected value
  to read the actual bytes/cells from the failure, then lock it.
- Phase 0 ships as its own PR and merges before feature phases; Phases 1–6 roll into the
  feature PR (split commits per phase/concern).

## Open verify-items (resolve during build)
- [ ] `layout::Alignment` → `HorizontalAlignment` — confirm whether a deprecated alias
      exists in 0.30 (affects Phase 0 sweep size).
- [ ] `'i'` truly free in the session-list context (Phase 3 tripwire is the gate).
- [ ] tui-term 0.3.4 renders correctly against ratatui 0.30.0 final (Phase 0 smoke).
