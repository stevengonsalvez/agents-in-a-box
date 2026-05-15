# vhs tape: capture the burndown analytics screen.
#
# Run from the ainb-tui/ workspace root after `just stage-plugins`:
#   vhs ../docs/assets/screenshots/burndown.tape
#
# Plugin path enabled (no AINB_DISABLE_PLUGINS) so this captures the
# real subprocess plugin running end-to-end against the seeded
# fixture. The post-Tab view (all panels expanded) is the canonical
# burndown screenshot — the compact pre-Tab state isn't useful for
# docs because users press Tab immediately.

Output ../docs/assets/screenshots/burndown.png

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1600
Set Height 900
Set Padding 20
Set Shell bash

Env AINB_SCREENSHOT_HOME "/tmp/ainb-screenshot-home"

Type "../docs/assets/screenshots/seed-and-run.sh"
Enter

# Cold start: home screen + plugin discovery + session-reader scan +
# burndown chunk ingest. Generous wait so every chunk has landed.
Sleep 6s

# Open Analytics (burndown screen).
Type "i"
Sleep 4s

# Tab expands the panel set into the full dashboard layout. Without
# this we capture the compact pre-focus render where only the top
# strip is populated.
Tab
Sleep 2s

Screenshot ../docs/assets/screenshots/burndown.png

# Esc back home then quit.
Escape
Sleep 500ms
Type "q"
Sleep 500ms
