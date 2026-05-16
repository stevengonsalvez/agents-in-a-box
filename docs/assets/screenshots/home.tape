# vhs tape: capture the ainb home screen against REAL $HOME.
#
# Run from the ainb-tui/ workspace root:
#   vhs ../docs/assets/screenshots/home.tape
#
# Uses the contributor's real ~/.agents-in-a-box so the workspaces
# list, recent sessions, etc. reflect actual state. Plugins disabled
# for this shot — the burndown tape covers the plugin-enabled path.

Output ../docs/assets/screenshots/home.png

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1600
Set Height 900
Set Padding 20
Set Shell bash

Env AINB_DISABLE_PLUGINS "1"

Type "exec ./target/debug/ainb tui"
Enter

# Real $HOME — workspace scan over ~/.agents-in-a-box (repos/, sessions.json,
# auth probe) takes ~30-60s cold against a populated contributor home.
# Confirmed by live tmux capture: TUI didn't paint until ~t+30s. Give margin.
Sleep 75s

Screenshot ../docs/assets/screenshots/home.png

Ctrl+c
Sleep 200ms
