# tui-lifecycle.tape — VHS recording for scenario "tui-lifecycle" (surface: tui)
#
# Title: TUI lifecycle: launch -> discovery banner -> import -> navigate
#        -> filter -> sync -> quit
#
# Drives the real `ainb` binary against a seeded sandbox at
# /tmp/sm-rec-tui-lifecycle (full tier, onboarding pre-completed so the
# first-run wizard does not eat keystrokes). The sandbox env vars
# (HOME + AINB_HOME + AINB_TOOL_HOME_CLAUDE + AINB_TOOL_HOME_CODEX) are
# shipped inline on the launch line so the binary points at the sandbox
# instead of the real ~/.claude / ~/.codex.
#
# Stand up the sandbox before running this tape:
#   bash scripts/skill-manager-sandbox.sh up --tier full --root /tmp/sm-rec-tui-lifecycle
#   mkdir -p /tmp/sm-rec-tui-lifecycle/.agents-in-a-box/config
#   (write onboarding.toml with completed=true)

Output explainers/recordings/tui-lifecycle.gif

Set Theme "Dracula"
Set FontSize 16
Set Width 1400
Set Height 800
Set Padding 20
Set Shell bash

# ── Step 1: Launch — run `ainb` with HOME pointed at the seeded sandbox.
#    Lands on the HomeScreen tile grid; wait for first cold paint to settle.
Hide
Type "HOME=/tmp/sm-rec-tui-lifecycle AINB_HOME=/tmp/sm-rec-tui-lifecycle/.agents-in-a-box AINB_TOOL_HOME_CLAUDE=/tmp/sm-rec-tui-lifecycle/.claude AINB_TOOL_HOME_CODEX=/tmp/sm-rec-tui-lifecycle/.codex GIT_TERMINAL_PROMPT=0 GIT_ASKPASS=/bin/true /Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_skill-manager/ainb-tui/target/debug/ainb"
Enter
Sleep 30s
Show
Sleep 1s

# ── Step 2: Press `m` — open the Skills (manager) screen from the Home
#    tile. Sources / Units / Detail panes render against the seeded
#    manifest (sandbox source + initial-skill + commit units).
Type "m"
Sleep 3s

# ── Step 3-4: Banner / import already settled (full-tier manifest is
#    pre-seeded), so the Units table is populated. Give it a beat.
Sleep 2s

# ── Step 5: Press `Down` (twice) — move the selection through the visible
#    Units; the Detail pane (lower band) recomputes for each highlight.
Down
Sleep 2s
Down
Sleep 2s

# ── Step 6: Press `/` — open the search input overlay; Units table enters
#    filter-input mode.
Type "/"
Sleep 2s

# ── Step 7: Type a filter substring matching a seeded skill name — the
#    Units table narrows case-insensitively to matching rows.
Type "commit"
Sleep 3s

# ── Step 8: Press `Esc` — dismiss the search overlay and restore the full
#    Units list (filter cleared), selection preserved.
Escape
Sleep 2s

# ── Step 9-10: Press `s` — route the selected unit to bidirectional sync
#    (fires SkillManagerSync); a `sync: <unit-name>` notification surfaces.
#    Wait for it to render so the tape captures the result line.
Type "s"
Sleep 3s

# ── Step 11: Press `q` — leave the Skill Manager (SkillManagerBack) and
#    return to the HomeScreen; tile markers visible again = clean exit.
Type "q"
Sleep 3s

Screenshot explainers/recordings/tui-lifecycle.png
