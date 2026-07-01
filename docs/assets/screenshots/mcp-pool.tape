# vhs tape: shared MCP pool — two sessions, one real context7 process.
#
# Drives scripts/mcp-pool-demo.sh, which spins up the pool daemon, attaches
# two independent `ainb mcp proxy` clients to a REAL context7 (npx) server,
# and proves both sessions share ONE backend process group. Deterministic:
# isolated $HOME inside the script, no Claude auth/TUI.
#
# Run from the ainb-tui/ workspace root (so ./target/release/ainb exists):
#   cargo build --release
#   vhs ../docs/assets/screenshots/mcp-pool.tape
#
# Output: ../docs/assets/screenshots/mcp-pool.gif

Output ../docs/assets/screenshots/mcp-pool.gif

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1500
Set Height 940
Set Padding 24
Set Shell bash

# Point the demo at the release binary built from this workspace.
Type "AINB_BIN=./target/release/ainb bash ../scripts/mcp-pool-demo.sh"
Enter

# The script paces itself with banners + sleeps: daemon up (2s), context7
# npx cold-start + session A attach (poll ≤30s), session B (≤15s), status +
# process-group proof, then a 4s hold on the final headline. Budget covers
# the worst-case cold start; if the script ends early the last frame holds.
Sleep 55s
