# vhs tape: capture the ainb home screen.
#
# Run from the ainb-tui/ workspace root:
#   vhs ../docs/assets/screenshots/home.tape
#
# Headless renderer — no real terminal needed. Drives the binary in
# its own pty, snapshots to PNG. Re-run after UX changes to refresh.

Output ../docs/assets/screenshots/home.png

Set Theme "Catppuccin Mocha"
Set FontSize 16
Set Width 1600
Set Height 900
Set Padding 20
Set Shell bash

# Disable plugins for the home shot — plugin discovery + spawn adds
# noise to the "what does ainb look like cold" picture. The burndown
# tape captures the plugin path separately.
Env AINB_DISABLE_PLUGINS "1"
Env AINB_SCREENSHOT_HOME "/tmp/ainb-screenshot-home"

# seed-and-run.sh writes an onboarding-complete marker (so the wizard
# doesn't take over the screen) + a synthetic Claude session, then
# execs ainb tui with HOME pointed at the isolated dir.
Type "../docs/assets/screenshots/seed-and-run.sh"
Enter

# Let the home screen settle: workspace scan, auth probe, first paint.
Sleep 4s

Screenshot ../docs/assets/screenshots/home.png

# Quit cleanly so vhs releases the recording.
Ctrl+c
Sleep 200ms
