# P0 Surface Safety Handoff

**Generated:** 2026-09-12

## Scope

Continue only the approved implementation slice in
`docs/plans/2026-09-05-desktop-p0-surface-safety.md`: P0 and Phase S.
`docs/plans/2026-09-04-desktop-shared-core-spec.md` remains the master
architecture roadmap. Do not start P1-P6 or D1-D4 desktop implementation in
this handoff.

The desktop V2 remains a later, separate slice. Its side-by-side Homebrew
distribution requirement belongs with D4 planning, not this P0 plus S run.

## Portable Repository State

- Branch: `v2`
- Base: rebased onto `origin/v2`
- Working tree: clean after replay
- P0 plus S commits: replayed from `anthias` without changing `anthias`
- Beads: `agents-in-a-box-j6b` and `agents-in-a-box-j6b.2` remain `in_progress`

Recent completed work includes:

- P0 keymap data, TOML override safety, and parity coverage.
- S-A config locking, atomic writes, process guards, and concurrent-save tests.
- S-B Hangar connection registry, presence lifecycle, and ACP provenance tests.
- Focused-plugin input forwarding and stale preview-scroll clearing.
- Daemon fixture isolation. Full `ainb-hangar-daemon` package suite passed.

## Current Gate State

| Gate | State | Evidence |
| --- | --- | --- |
| Daemon package suite | passed | `/tmp/ainb-hangar-daemon-full-final3-2026-09-12.log`, `EXIT:0` |
| P0 core suite | blocked | `tripwire_burndown_heatmap` failed after 20.65s |
| Burndown Esc fixture review | findings open | Review of `2e6492b3` found P1 and P2 fixture isolation defects |
| Human G6 checkpoint | pending | Stevie has not performed final two-terminal manual validation |
| Desktop V2 | not started | No `ainb-app` or `ainb-desktop` crate exists |

The heatmap failure is reproducible in
`ainb-tui/crates/ainb-core/tests/tripwire_burndown_heatmap.rs`:
after `Left`, plain `tmux capture-pane` output did not change. The current UI
uses `local_now()` and honors `AINB_NOW` for its heatmap anchor. Likely test
observation gap: selection styling may change while plain text capture does
not. Reproduce and inspect before choosing a fix.

## First Resume Work

1. Repair only `ainb-tui/crates/ainb-core/tests/tripwire_burndown_esc_returns_home.rs`:
   - Name the seeded origin tmux session with the required `tmux_` prefix.
   - Set `AINB_HOME` to the fixture `.agents-in-a-box` directory in the launch command.
   - Assert the `burndown-origin` row renders before opening Analytics with `i`.
   - Run the focused tripwire in an exact named tmux session.
   - Commit that one changed file with `git commit -S`.
2. Reproduce and fix the heatmap tripwire without weakening the user-visible
   cursor movement contract. Preserve an end-to-end assertion that detects the
   selected day changing.
3. Run the remaining P0 plus S gates, review every completed phase, and fix
   every actionable review finding before marking plan checkpoints complete.
4. Keep one changed file per signed commit. Do not use `git add -A`.

## New Orca Machine

Orca Run IDs and terminal handles are machine-local. Do not resume
`run_e4a8abe344d8`. That run has no active worker after shutdown and remains
historical evidence only.

Before dispatching a Claude worker on the new machine:

1. Open Orca against the checked-out `v2` branch after this handoff is pushed.
2. Run `claude login` in an Orca terminal and accept the trusted-workspace prompt.
3. Run `orca account add --agent claude`.
4. Verify `orca account list --json` reports one active Claude account.
5. Create a new Orca Run for P0 plus S closure, then dispatch only bounded tasks
   from this handoff. Use a new worktree for any task that will edit source.

On the old machine, `orca account add --agent claude` failed because macOS
Keychain lacked `Claude Code-credentials`. Direct `claude login` reached the
trusted-workspace prompt, but authentication was deliberately stopped before
completion. New machine must prove account registration before orchestration.

## Old Orca Run Record

- Historical Run: `run_e4a8abe344d8`
- D1 preflight Dispatch: `ctx_c35b8fc2fbb9`, explicitly stopped and released.
- One old failed P0 dispatch remains retained with no resource. It has no live
  terminal and needs no action on the new machine.
- No global tmux shutdown occurred. The finished
  `test-ainb-p0-core-final3-20260912` session was removed by exact name.

## Required Reading

1. `docs/plans/2026-09-05-desktop-p0-surface-safety.md`
2. `docs/plans/2026-09-04-desktop-shared-core-spec.md`
3. This handoff
4. `AGENTS.md`
