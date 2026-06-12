# vhs tape: overlay-panels — open the Stats panel from the HOME menu and
# Esc back to HOME (panel returns to its origin screen).
#
# Not push-button reproducible: it runs against a seeded fixture $HOME so
# burndown renders deterministic numbers. From the ainb-tui/ workspace
# root, after `just stage-plugins`:
#
#   1. Seed an isolated $HOME:
#        - ~/.agents-in-a-box/config/onboarding.toml  (completed=true)
#        - ~/.agents-in-a-box/install.json  ({"agents":[],"prompt_dismissed":true})
#        - copy crates/ainb-core/tests/fixtures/tripwire_keys/{claude,codex}
#          into ~/.claude/projects and ~/.codex/sessions
#      (this is exactly what tripwire_burndown_keys.rs::seed_fixture_home does)
#   2. Launch with AINB_NOW pinned to the fixture's FIXTURE_NOW.txt:
#        HOME=<seed> AINB_PLUGIN_ROOT=<dist/plugins> AINB_NOW=<now> ainb tui
#
# The Hide block below absorbs the cold-start; the GIF starts at HOME.

Output overlay-home-stats-home.gif

Set FontSize 12
Set Width 1500
Set Height 880
Set Padding 8

Hide
Type "HOME=$HOME_FIXTURE AINB_PLUGIN_ROOT=$PLUGINS AINB_NOW=$NOW exec ./target/debug/ainb tui"
Enter
Sleep 30s
Show

Sleep 2.5s
Type "i"
Sleep 11s
Escape
Sleep 4s
