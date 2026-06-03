# cli-lifecycle.tape — vhs recording for scenario "cli-lifecycle" (surface: cli)
#
# Demonstrates the full `ainb skill ...` CLI lifecycle against a hermetic
# sandbox seeded by scripts/skill-manager-sandbox.sh:
#   folder skill -> source add -> install -> drift check -> edit ->
#   sync (to-repo, dry-run) -> remove.
#
# The sandbox is stood up + torn down by the spawning agent, not by this
# tape. We just source its env.sh and drive the binary against it.
#
# Binary path is absolute (debug build). env.sh exports HOME, AINB_HOME,
# AINB_TOOL_HOME_CLAUDE, AINB_TOOL_HOME_CODEX and the GIT_* guards so every
# command below targets the sandbox, never the real home.

Output explainers/recordings/cli-lifecycle.gif

Set Theme "Dracula"
Set FontSize 16
Set Width 1400
Set Height 800
Set Padding 20
Set Shell bash

# ── source the sandbox env so HOME/AINB_* point at /tmp/sm-rec-cli-lifecycle ──
Type "source /tmp/sm-rec-cli-lifecycle/env.sh"
Enter
Sleep 1s

# alias for readability in the recording
Type "B=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_skill-manager/ainb-tui/target/debug/ainb"
Enter
Sleep 1s

Type "G=git:file:///tmp/sm-rec-cli-lifecycle/sandbox-remote.git"
Enter
Sleep 1s

# ── 1. register the bare remote as a source (fetches + records in lockfile) ──
Type "$B source add $G"
Enter
Sleep 3s

# ── 2. list sources to show the new SourceEntry ──
Type "$B source list"
Enter
Sleep 3s

# ── 3. install the seeded 'initial-skill' unit to every accepting tool home ──
Type "$B skill install $G@main/skills/initial-skill --yes"
Enter
Sleep 4s

# ── 4. drift check — InSync immediately after install ──
Type "$B skill check"
Enter
Sleep 3s

# ── 5. machine-readable drift report ──
Type "$B skill check --json"
Enter
Sleep 3s

# ── 6. mutate the deployed claude home file so home diverges from upstream ──
Type 'printf "\n# local edit\n" >> "$AINB_TOOL_HOME_CLAUDE/skills/initial-skill/SKILL.md"'
Enter
Sleep 2s

Type 'tail -n 4 "$AINB_TOOL_HOME_CLAUDE/skills/initial-skill/SKILL.md"'
Enter
Sleep 3s

# ── 7. re-run drift after the edit ──
Type "$B skill check --json"
Enter
Sleep 3s

# ── 8. content sync toward the repo (dry-run: show the plan, mutate nothing) ──
Type "$B skill sync $G@main/skills/initial-skill --to-repo --dry-run"
Enter
Sleep 4s

# ── 9. remove the unit from every target tool home; lockfile entry dropped ──
Type "$B skill remove $G@main/skills/initial-skill --yes"
Enter
Sleep 4s

# ── 10. confirm the deployed file is gone and the lockfile is empty ──
Type 'ls "$AINB_TOOL_HOME_CLAUDE/skills/initial-skill" 2>&1; $B skill check --json'
Enter
Sleep 3s

Screenshot explainers/recordings/cli-lifecycle.png
