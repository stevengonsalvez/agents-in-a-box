# P7 / D17 Squads journey — tmux-verify frame-truth recording.
# Launches the real ainb TUI against a running hangar daemon seeded with a
# workspace-scoped squad "shippers" led by the agent lead-bot, with agent members
# worker-bot + deploy-bot and one human member. Opens the Squads screen (tab S),
# reads the rendered team, then presses `x` to fan the current issue across the
# squad — the leader brief + parallel member dispatch — surfacing the green
# "briefed agent-2 + 2 members" note. Paths are baked in (vhs 0.11 has no
# `Set Env`); the daemon is stood up by record-squads-journey.sh.
Output "squads-journey.gif"

Set Shell "bash"
Set FontSize 13
Set Width 1900
Set Height 950
Set Padding 12

# Launch the TUI (hidden while it cold-loads).
Hide
Type "HOME='/tmp/sq.TUE07r' AINB_PLUGIN_ROOT='/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_hangar-parity/ainb-tui/dist/plugins' exec '/Users/stevengonsalvez/.cache/ccc-shared-target/debug/ainb' tui"
Enter
Sleep 12s
Show

# Home screen is up. Open the Hangar plugin screen.
Type "g"
Sleep 4s

# Open the Squads screen (tab S). The seeded squad renders live from the daemon:
# "shippers", leader: <dot> lead-bot (3 members), then the indented member rows.
Type "S"
Sleep 5s
Screenshot "squads-1-open.png"

# PAUSE on the rendered team (squad name, leader tag, member rows w/ agent/human).
Sleep 3s
Screenshot "squads-2-squad.png"

# Fan the current issue across the squad (x → hangar/squad_fanout). The daemon
# briefs the leader agent + enqueues one task per AGENT member (the human is
# skipped), and the screen raises the green "briefed agent-2 + 2 members" note.
Type "x"
Sleep 6s
Screenshot "squads-3-briefed.png"
Sleep 2s
