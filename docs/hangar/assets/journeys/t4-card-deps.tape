# T4b — card-dependency chain (verify-converged-goal.md journey catalogue).
Output "t4-card-deps.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='/tmp/t4bd.6F3gzP' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.cache/dist/plugins' exec '/Users/stevengonsalvez/.cache/ccc-shared-target/debug/ainb' tui"
Enter
Sleep 12s
Show

Type "g"
Sleep 4s

# --- Boards (B): A unblocked, B depends-on A (renders 🔒[issue-dep-a]) ---
Type "B"
Sleep 3s
Screenshot "t4b-1-board-both-cards.png"

# --- focus B (j), attempt Run -> REFUSED (daemon note: "run failed: blocked ...") ---
Type "j"
Sleep 1s
Enter
Sleep 1s
Enter
Sleep 3s
Screenshot "t4b-2-blocked-refused.png"

# --- focus back to A (k), Run it (Headless) ---
Type "k"
Sleep 1s
Enter
Sleep 1s
Enter
Sleep 3s

# --- hold: A blocked on the release sentinel; state-driven release fires ~4s
#     after A is observed running (shell-verified in the poll log) ---
Sleep 18s
Screenshot "t4b-3-both-done-autoran.png"
Sleep 1s
