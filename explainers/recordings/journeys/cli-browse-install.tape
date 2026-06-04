Output "/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_skill-manager/explainers/recordings/journeys/cli-browse-install.gif"
Set Theme "Dracula"
Set FontSize 16
Set Width 1400
Set Height 760
Set Padding 20
Set Shell bash
Type "source /tmp/sm-rr-cbi/env.sh; export PATH=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_skill-manager/ainb-tui/target/debug:$PATH; export AINB_CATALOG_MOCK=1; clear"
Enter
Sleep 1s
Type "ainb skill browse react"
Enter
Sleep 3s
Type "ainb skill install git:file:///tmp/sm-rr-cbi/sandbox-remote.git@main/skills/initial-skill --targets claude --yes"
Enter
Sleep 4s
