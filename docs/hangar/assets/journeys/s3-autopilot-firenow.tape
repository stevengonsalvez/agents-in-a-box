# S3 — autopilot fire-now (verify-converged-goal.md journey catalogue).
Output "s3-autopilot-firenow.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='/tmp/s3af.70xH0v' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.cache/dist/plugins' exec '/Users/stevengonsalvez/.cache/ccc-shared-target/debug/ainb' tui"
Enter
Sleep 12s
Show

Type "g"
Sleep 4s

# --- Autopilots manager (4): the seeded row before any fire ---
Type "4"
Sleep 3s
Screenshot "s3-1-autopilots-open.png"

# --- fire-now (r): dispatch → claim → fake-provider completion, all within ~1s ---
Type "r"
Sleep 3s
Screenshot "s3-2-fired-completed.png"

# --- Usage (U): the completed run landed in recent-runs history ---
Type "U"
Sleep 3s
Screenshot "s3-3-usage-recent-runs.png"
Sleep 1s
