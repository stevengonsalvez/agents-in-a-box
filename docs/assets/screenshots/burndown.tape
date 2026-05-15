# vhs tape: capture the burndown analytics screen against REAL $HOME.
#
# Run from the ainb-tui/ workspace root after `just stage-plugins`:
#   vhs ../docs/assets/screenshots/burndown.tape
#
# Uses the contributor's real ~/.claude/projects so burndown renders
# the actual usage dashboard rather than a synthetic fixture. Not
# reproducible across machines — the screenshot you commit is a
# point-in-time snapshot of your own usage. Cold-cache scan on a
# 100k+ call dataset takes ~40s; the sleep below gives margin.

Output ../docs/assets/screenshots/burndown.png

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1600
Set Height 900
Set Padding 20
Set Shell bash

Type "./target/debug/ainb tui"
Enter

# Home screen settle: workspace scan + auth probe + plugin discovery
# (~30s cold on real $HOME with populated ~/.agents-in-a-box).
Sleep 75s

# Open Analytics (burndown screen). Burndown plugin only starts ingesting
# ~/.claude/projects once 'i' triggers refresh — real-HOME 2.2GB / 179
# project dirs takes 40-60s to fully scan before all panels populate.
Type "i"
Sleep 60s

# Tab expands panels into the full dashboard layout.
Tab
Sleep 2s

Screenshot ../docs/assets/screenshots/burndown.png

# Esc back home then quit.
Escape
Sleep 500ms
Type "q"
Sleep 500ms
