# T4a — squad-card fan-out (verify-converged-goal.md journey catalogue).
Output "t4-squad-fanout.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='/tmp/t4af.ew4faJ' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.cache/dist/plugins' exec '/Users/stevengonsalvez/.cache/ccc-shared-target/debug/ainb' tui"
Enter
Sleep 12s
Show

Type "g"
Sleep 4s

# --- Boards (B): the squad card, unblocked, not yet run ---
Type "B"
Sleep 3s
Screenshot "t4a-1-board-squad-card.png"

# --- Run (Enter opens Run ▾, Enter again commits Headless) ---
Enter
Sleep 1s
Enter
Sleep 4s
Screenshot "t4a-2-fanout-running.png"

# --- hold: three fanned tasks blocked on the release sentinel, each on its own
#     live ainb/<slug> worktree (shell-verified in t4a-worktree-poll.log); the
#     state-driven release fires ~6s after it observes 3/3 running ---
Sleep 14s
Screenshot "t4a-3-fanout-done.png"
Sleep 1s
