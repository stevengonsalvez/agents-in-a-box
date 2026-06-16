# vhs tape: importing MCP servers from the pool overlay in the ainb TUI.
#
# Drives scripts/mcp-import-journey.sh — isolated $HOME, an empty pool daemon,
# and a project whose .mcp.json declares one stdio server. Launches the TUI in
# that project, opens the overlay with `p` (empty), then presses `i` to import.
# Proves: the import writes the global user config AND the server appears live
# in the table, with a "▸ imported …" result line.
#
# Run from the ainb-tui/ workspace root:
#   cargo build --release
#   vhs ../docs/assets/screenshots/mcp-import.tape
#
# Output: ../docs/assets/screenshots/mcp-import.gif

Output ../docs/assets/screenshots/mcp-import.gif

Set Theme "Catppuccin Mocha"
Set FontSize 15
Set Width 1500
Set Height 900
Set Padding 20
Set Shell bash

Type "AINB_BIN=./target/release/ainb bash ../scripts/mcp-import-journey.sh"
Enter

# TUI startup + workspace/auth probe against the isolated $HOME.
Sleep 13s

# Dismiss the first-run notify-install prompt so it doesn't swallow `p`.
Escape
Sleep 1s

# Open the pool overlay — empty, "No servers pooled yet" (with the `i` hint).
Type "p"
Sleep 3s

# Import: reads the launch dir's .mcp.json + Claude user scope, writes the
# global user config, registers
# the server with the live daemon. The table gains the row + a result line.
Type "i"
Sleep 4s

# Linger so the recording holds on the imported server + result line.
Sleep 3s
