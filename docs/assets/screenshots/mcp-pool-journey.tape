# vhs tape: shared MCP pool — a from-scratch USER GUIDE.
#
# Drives scripts/mcp-pool-journey.sh — the real commands a user runs
# (`ainb mcp import`, `ainb mcp status`) against a REAL context7 server,
# proving two sessions share ONE backend process. Reads as a step-by-step
# guide. Deterministic: isolated $HOME inside the script, no Claude auth/TUI.
#
# Run from the ainb-tui/ workspace root:
#   cargo build --release
#   vhs ../docs/assets/screenshots/mcp-pool-journey.tape
#
# Output: ../docs/assets/screenshots/mcp-pool-journey.gif

Output ../docs/assets/screenshots/mcp-pool-journey.gif

Set Theme "Catppuccin Mocha"
Set FontSize 15
Set Width 1500
Set Height 1000
Set Padding 24
Set Shell bash

Type "AINB_BIN=./target/release/ainb bash ../scripts/mcp-pool-journey.sh"
Enter

# Six narrated steps with built-in pauses, then context7 npx cold-start
# (session-1 polls ≤40s), session-2, status, and the process-group proof,
# ending on a 5s hold on the summary. Budget covers worst-case cold start.
Sleep 70s
