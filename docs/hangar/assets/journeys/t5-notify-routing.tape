# T5 — notification routing (verify-converged-goal.md journey catalogue).
Output "t5-notify-routing.gif"

Set Shell "bash"
Set FontSize 13
Set Width 2200
Set Height 1000
Set Padding 12

Hide
Type "HOME='/tmp/t5nr.V5lSwE' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.cache/dist/plugins' exec '/Users/stevengonsalvez/.cache/ccc-shared-target/debug/ainb' tui"
Enter
Sleep 12s
Show

Type "g"
Sleep 4s

# --- Settings (,) -> Notifications section (j clamps there regardless of the
#     starting section) — the seeded default: ask routes to web+os ---
Type ","
Sleep 2s
Type "jjjjjjjj"
Sleep 2s
Screenshot "t5-1-notify-grid-default.png"

# --- flip the ask rule: space on phone (col 0) ENABLES it ---
Type " "
Sleep 1s
# --- move right twice (phone -> web -> os), space DISABLES os ---
Type "ll"
Sleep 1s
Type " "
Sleep 2s
Screenshot "t5-2-notify-grid-flipped.png"

# --- hold: the background step waits for the flip to land, then raises a real
#     ASK via the hook seam; the daemon's attention-ingest tick (3s) picks it up ---
Sleep 18s

# --- Control Center (C): the routed ASK, now visible on the board ---
Type "C"
Sleep 3s
Screenshot "t5-3-control-center-ask-routed.png"
Sleep 1s
