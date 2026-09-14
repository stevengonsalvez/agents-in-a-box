# /goal The four slice-1 regressions the G6 checkpoint found (#987, #988, #989, #990) are fixed on v2 with a regression test each, and G6 steps 4 and 5 pass on a binary built from v2 tip

— CONTEXT —
· Project: agents-in-a-box (ainb) TUI, slice 1 of the desktop programme (`docs/plans/2026-09-12-desktop-programme.md`). The G6 human checkpoint (`docs/plans/2026-09-12-g6-human-checkpoint.md`, run 2026-09-13 against `v2` at 026f40828, evidence on PR #964) found four regressions: #987 the Config screen save overlays the whole in-memory `AppConfig` onto the file so another writer's settings revert (S-A was meant to fix exactly this); #988 quitting a second TUI SIGTERMs the shared headroom proxy another TUI still uses (S-A's proxy lock covers spawn, not shutdown); #989 `enter` on the Config screen no longer opens the setting editor (commit fc1d3d4df dropped the hardcoded arm and `keymap_defaults.rs` has no `enter` row in the `config` context, only `config.search`) and typed characters in config search do not reach the query; #990 selecting the TUI's own tmux session under `Other tmux` panics in vt100 `grid.rs:683` (subtract with overflow).
· Stack: Rust workspace under `ainb-tui/`; config save path `ainb-core/src/app/events.rs` (`persist_config_screen`) and `ainb-app/src/config/mod.rs` (`save`, `write_keys_into`, `config/lock.rs`, `tests/config_concurrent_save.rs`); headroom proxy lifecycle under `ainb-core/src/headroom/` (spawn lock, watchdog, quit path); keymap `ainb-core/src/app/keymap_defaults.rs` and the keymap tripwire tests; tmux observer and vt100 preview in `ainb-core/src/components/` and the observer module.
· Current state: `v2` at 526e71789 or later; P1 moved config and state into `ainb-app`, so some of these files now live under `ainb-tui/crates/ainb-app/`, check with `rg` before editing. Lane F is concurrently moving side effects out of `ainb-core/src/app/*` into `ainb-app` (P2, branch `stevengonsalvez/p2-effects`); keep every fix minimal and local so it rebases cleanly.
· Working dir: the Orca worktree this session was launched in (`g6-verify` on claude-gcp), currently on `fix/sa-regressions` from `origin/v2` with a warm build target from `v2` tip; use one branch per issue instead (`fix/987-config-merge-save`, `fix/988-proxy-shared-shutdown`, `fix/989-config-enter-search`, `fix/990-own-session-preview`), each from `origin/v2`, switching in this worktree sequentially.
· Constraints: one PR per issue targeting `v2`, each with a regression test that fails on `origin/v2` and passes with the fix (show both runs), small commits by named paths, commit with `git -c commit.gpgsign=false commit` (the orchestrator re-signs at merge) and say so in each PR body; #987: the save writes only the keys the writer changed through the read-merge-write that `write_keys_into` already does, under the existing flock, no format change to `config.toml`; #988: a TUI quitting stops the proxy only when it is the last user (reference count or liveness check against the pid file's owners, pick the simpler one that survives a crashed TUI) and the watchdog must not respawn a proxy nobody uses; #989: add the `enter` row to the `config` context in `keymap_defaults.rs` (and the categories and auth-provider `enter` arms fc1d3d4df dropped, if the keymap test shows them missing) and make search typing reach the query, with the keymap tripwire updated; #990: the own-session row must render a placeholder instead of mirroring itself (an observer attached to the pane that hosts the TUI recurses), and the vt100 subtract must be guarded so no pane geometry can panic the TUI; never touch `ainb-app/src/app/*` reducers beyond the arms these fixes need; gates per PR: `cargo test -p ainb-core -p ainb-app`, `cargo clippy --workspace -- -D warnings`, and G6 step 4 (for #987, #989) and step 5 (for #988) re-run by you against the fixed binary with the exact commands in the checkpoint doc, results posted on the PR; merge `origin/v2` before touching any file lane F changed and rebase daily; open the PRs as they are ready, do not wait to batch them.
· Audience: Stevie, who drives G6 steps 8 to 10 next and wants steps 4 and 5 green first; the desktop host, which will share config and the proxy with the TUI.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. Four PRs on `v2`, one per issue, each carrying a regression test shown failing on `origin/v2` and passing with the fix, clippy clean, and closing its issue.
2. G6 step 4 passes on the fixed binary: two TUIs change different settings, both survive on disk and after restart, `enter` opens the editor from the settings list, and typing in search filters the rows; G6 step 5 passes: quitting B leaves `proxy.pid` and the process untouched while A runs, B's log records no SIGTERM, and quitting A last stops the proxy.
3. Selecting the TUI's own tmux session under `Other tmux` shows a placeholder and the TUI keeps drawing; a test drives the vt100 preview with the geometry that panicked and passes.
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
