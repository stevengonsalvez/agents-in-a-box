# /goal Spike 2 is answered with measurements: a Rust VT emulator fed by tmux control mode reproduces a live agent screen byte-equal to a direct PTY, and the report names the crate and the R2 scope it flips

— CONTEXT —
· Project: agents-in-a-box (ainb) desktop programme, phase R2 (daemon-side headless emulator as a cache over tmux, tmux stays the PTY owner). Spec: `docs/plans/2026-09-11-multi-surface-decisions-spec.md` D10, phase row R2, spike rows 2 and 7, edge cases "one pane floods", "daemon feed dies". Prior results: `research/2026-09-11_multi-surface_SPIKE-1-osc-through-tmux.md` (control mode delivers OSC frames verbatim; `-f ignore-size` does not pin size on tmux 3.4, `window-size manual` does) and `SPIKE-7-control-mode-flow.md` (`refresh-client -f pause-after=2` pauses only the flooding pane; `%continue` drops the paused interval; quote `-A "%<id>:continue"`; `pause-after` switches the client to `%extended-output`). Programme DAG: `docs/plans/2026-09-12-desktop-programme.md`.
· Stack: tmux 3.4 (verify `tmux -V` on this box), Rust scratch crate, candidate emulator crates `vt100`, `alacritty_terminal`, `wezterm-term` (evaluate at least two), `portable-pty` for the direct-PTY control, real agent TUIs available on the box (`claude`, `codex`) plus synthetic fixtures for alt-screen, mouse tracking, wide characters (CJK, emoji), OSC 8 links, and a custom OSC status frame.
· Current state: no emulator code exists in the repo; R2 is planned after R1; this spike gates R2's scope and the "daemon-owned PTY" do-not-build row reopens only if fidelity is poor.
· Working dir: the Orca worktree this session was launched in, branched from `origin/v2`; all code lives under a scratch directory outside the crates (for example `research/spikes/spike-2/` or `/tmp`), never inside `ainb-tui/crates`.
· Constraints: read-only on the repo except the report file `research/2026-09-11_multi-surface_SPIKE-2-control-mode-fidelity.md` (force-add, the directory is gitignored) and an optional PR to `v2` that updates the spike-2 row and the R2 row in the programme doc; private tmux server only, every tmux command uses `-L ainb-spike2`, never `tmux kill-server` or any bulk kill, clean up by exact session name; no product names of prior-art systems anywhere; no em-dashes; signed commits; `timeout` on every run; measure RSS per emulator at 100 sessions and CPU during `cat` of 50 MB; confirm `#{window_width}` seen by a second attached client never changes while the daemon-style control client is attached; feed = `tmux -C attach` on a pipe with `refresh-client -A "%<pane>:on"` and `-f pause-after=2`, parse `%extended-output`.
· Audience: the R2 implementer and Stevie, who decide the emulator crate and whether R2 stays on the tmux hybrid.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. For each fixture (alt-screen TUI, mouse-tracking mode, wide chars, OSC 8 link, custom OSC status frame) the emulator snapshot at 120x40 and at 40x20 from the control-mode feed is byte-equal after ANSI normalisation to the same fixture driven through a direct `portable-pty`, or every difference is listed with the exact bytes and a verdict (cosmetic, fixable, blocker).
2. The report has a crate comparison table (fidelity per fixture, RSS at 100 emulators, CPU during the 50 MB flood, API fit for snapshot-then-tail and re-snapshot on `%continue`) and names one crate with a one-paragraph rationale, plus the `#{window_width}` proof and the pause and resume behaviour reproduced once.
3. The "Decision input" section states whether R2 proceeds on the tmux hybrid as specified, what R2's live-window and re-snapshot rules must be, and, if fidelity is a blocker on every crate, the concrete case for reopening the daemon-owned PTY row.
4. Final deliverable runs without errors
5. You can show proof (screenshot · test output · URL)

— OPERATING RULES — NON-NEGOTIABLE —
1. PLAN FIRST. Output a numbered task list before writing any code.
2. WORK AUTONOMOUSLY. Don't ask clarifying Qs unless genuinely blocked.
3. SELF-VERIFY. After every step: run tests, inspect output, confirm it worked.
4. DEBUG YOURSELF. If it fails, diagnose + fix. Don't hand it back.
5. USE EVERY TOOL. MCPs · terminal · web · code exec · pull real data.
6. NO PLACEHOLDERS. No TODOs · no stubs · real components + real states.
7. PROGRESS LOG. Track completed · in-flight · decisions · blockers.
8. STAY ON GOAL. Discoveries off-spec? Note + keep moving.
9. IF BLOCKED. Log the wall · continue everything parallelizable.
10. CHECK SUCCESS BEFORE STOPPING. Re-read criteria · confirm each is met.

— QUALITY BAR —
· Code: clean, typed, follows project conventions
· Design: looks like a well-funded startup shipped it
· Output: survives a senior code review
· Docs: every new pattern / env var / decision logged

— FINAL DELIVERABLE —
✅ Confirmation each criterion is satisfied
📂 Every file created / modified
🚀 How to run / test / deploy
📊 Proof (screenshot · test output · URL)
📝 Decisions made + anything to know
⚠️ Known limitations + follow-ups

Begin by outputting your plan. Then execute end-to-end without checking
in until done or genuinely blocked.
