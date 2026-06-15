# vhs tape: the MCP pool observability overlay in the ainb TUI.
#
# Drives scripts/mcp-overlay-journey.sh — sets up an isolated $HOME with a
# daemon + a fake context7 server + two named sessions (web-session,
# api-session), launches the TUI, then opens the overlay with `m`. Proves the
# live table shows the shared server and which sessions share it.
#
# Run from the ainb-tui/ workspace root:
#   cargo build --release
#   vhs ../docs/assets/screenshots/mcp-overlay.tape
#
# Output: ../docs/assets/screenshots/mcp-overlay.gif

Output ../docs/assets/screenshots/mcp-overlay.gif

Set Theme "Catppuccin Mocha"
Set FontSize 15
Set Width 1500
Set Height 900
Set Padding 20
Set Shell bash

Type "AINB_BIN=./target/release/ainb bash ../scripts/mcp-overlay-journey.sh"
Enter

# TUI startup + workspace/auth probe against the isolated $HOME.
Sleep 13s

# Dismiss the first-run notify-install prompt so it doesn't swallow `m`.
Escape
Sleep 1s

# Open the MCP pool overlay from the sidebar shortcut.
Type "m"
Sleep 3s

# Hold on the overlay so the recording lingers on the table (server,
# shared ×2, the two session names).
Sleep 4s
