Output "/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_skill-manager/explainers/recordings/journeys/cli-sync-edit.gif"
Set Theme "Dracula"
Set FontSize 16
Set Width 1400
Set Height 760
Set Padding 20
Set Shell bash
Type "source /tmp/sm-rr-cse4/env.sh; export PATH=/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_feat_skill-manager/ainb-tui/target/debug:$PATH; clear"
Enter
Sleep 1s
Type "ainb skill sync --to-repo --yes"
Enter
Sleep 4s
Type "git -C /tmp/sm-rr-cse4/sandbox-remote.git log --oneline | head -3"
Enter
Sleep 3s
