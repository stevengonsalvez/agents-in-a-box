---
name: swarm-create
description: Create a new swarm team from a Beads epic with N worker agents
user-invocable: true
argument-hint: [epic-id]
---

# /swarm-create

Create a tmux-persistent multi-agent swarm team for parallel task execution.

## Usage

```bash
/swarm-create --epic <epic-id> --agents <count> [--isolation <mode>] [--dry-run]
```

## Arguments

| Argument | Required | Default | Description |
|----------|----------|---------|-------------|
| `--epic` | Yes | - | Beads epic ID to create swarm from |
| `--agents` | No | 2 | Number of worker agents (max 4) |
| `--isolation` | No | (ask user) | Isolation mode: `shared` or `worktree` |
| `--dry-run` | No | false | Show what would be created without executing |

## Isolation Modes

**Default: Shared Mode** - For most swarm operations, shared mode is recommended.
Worktree mode adds complexity (merge step) and is only needed when agents might
modify the same files. Shared mode worked well for the 6-phase Pulse Comments
implementation (~$12-15 cost, 20 mins).

**You MUST ask the user which isolation mode they want if not specified.**

Use the AskUserQuestion tool:

```
question: "How should agents be isolated?"
header: "Isolation"
options:
  - label: "Shared Branch (Recommended)"
    description: "All agents work in same directory on independent files. Faster, no merge step, but requires careful task partitioning to avoid conflicts."
  - label: "Git Worktrees"
    description: "Each agent gets own git worktree and branch. Full isolation, agents commit independently, merge at end. Slower startup, handles conflicts better."
```

| Mode | Pros | Cons | Best For |
|------|------|------|----------|
| **shared** | Fast startup, no merge step | Risk of file conflicts | Well-partitioned tasks, small teams |
| **worktree** | Full isolation, safer | Slower startup, merge step needed | Large teams, overlapping files |

## Process

When you receive this command:

1. **Parse Arguments**
   ```bash
   EPIC_ID="$ARGUMENTS"  # Extract --epic value
   AGENT_COUNT=2         # Extract --agents value or default
   ISOLATION_MODE=""     # Extract --isolation value if provided
   DRY_RUN=false         # Check for --dry-run flag
   ```

2. **Ask About Isolation Mode (if not specified)**

   If `--isolation` was not provided, use AskUserQuestion to ask the user.

3. **Validate Epic** (if not dry-run)
   ```bash
   # Check epic exists in Beads
   bd show "$EPIC_ID" --json || echo "Epic not found"
   ```

4. **Initialize Team Directory**
   ```bash
   # Use bash invocation — the script exposes subcommands, not exported shell functions
   bash $HOME/{{TOOL_DIR}}/utils/swarm-lib.sh create-team "$EPIC_ID" "$AGENT_COUNT" "$ISOLATION_MODE" 2>&1
   ```
   Output is the team ID (e.g. `swarm-1774612409`).

5. **Spawn Leader**
   ```bash
   TEAM_ID="swarm-XXXXXX"  # from step 4
   bash $HOME/{{TOOL_DIR}}/utils/swarm-lib.sh spawn-leader "$TEAM_ID" "optional-lead-prompt" 2>&1
   ```
   Output is the tmux session name (e.g. `swarm-XXXXXX-leader`).

6. **Spawn Worker Agents**
   ```bash
   for i in $(seq 1 $AGENT_COUNT); do
     bash $HOME/{{TOOL_DIR}}/utils/swarm-lib.sh spawn-agent "$TEAM_ID" "agent-$i" "optional-prompt" 2>&1
     sleep 2  # Stagger spawning
   done
   ```
   Output is the tmux session name for each agent.

7. **Verify & Report**
   ```bash
   # Get status
   bash $HOME/{{TOOL_DIR}}/utils/swarm-lib.sh status "$TEAM_ID"

   echo ""
   echo "========================================"
   echo "Swarm Created: $TEAM_ID"
   echo "========================================"
   echo "Epic: $EPIC_ID"
   echo "Isolation: $ISOLATION_MODE"
   echo "Leader: ${TEAM_ID}-leader"
   echo "Agents: $AGENT_COUNT"
   echo ""
   echo "Commands:"
   echo "  Attach to leader: tmux attach -t ${TEAM_ID}-leader"
   echo "  Check status:     /swarm-status $TEAM_ID"
   if [[ "$ISOLATION_MODE" == "worktree" ]]; then
     echo "  Merge worktrees:  bash $HOME/{{TOOL_DIR}}/utils/swarm-lib.sh merge-worktrees $TEAM_ID"
   fi
   echo "  Shutdown:         /swarm-shutdown $TEAM_ID"
   echo "========================================"
   ```

## Dry Run Output

When `--dry-run` is specified, show what would happen without executing:

```
DRY RUN: Would create swarm from epic: bd-epic-123

Isolation Mode: worktree

Would create:
  - Team directory: {{HOME_TOOL_DIR}}/swarm/swarm-XXXXXXXXXX/
  - Team file: {{HOME_TOOL_DIR}}/swarm/swarm-XXXXXXXXXX/team.json
  - Inbox files:
    - inbox/leader.jsonl
    - inbox/agent-1.jsonl
    - inbox/agent-2.jsonl
  - Shared directory: {{HOME_TOOL_DIR}}/swarm/swarm-XXXXXXXXXX/shared/
  - Worktrees (if worktree mode):
    - worktrees/agent-1/ (branch: swarm-XXXXXXXXXX-agent-1)
    - worktrees/agent-2/ (branch: swarm-XXXXXXXXXX-agent-2)

Would spawn tmux sessions:
  - swarm-XXXXXXXXXX-leader (Team Leader)
  - swarm-XXXXXXXXXX-agent-1 (Worker)
  - swarm-XXXXXXXXXX-agent-2 (Worker)

Would run:
  bd swarm create bd-epic-123

No changes made.
```

## Example

```bash
# Ask about isolation mode (recommended)
/swarm-create --epic bd-epic-platform-rebuild --agents 3

# Explicit shared mode (fast, for independent tasks)
/swarm-create --epic bd-epic-123 --agents 3 --isolation shared

# Explicit worktree mode (isolated, for overlapping files)
/swarm-create --epic bd-epic-123 --agents 3 --isolation worktree

# Dry run to preview
/swarm-create --epic bd-epic-123 --agents 2 --dry-run
```

## Worktree Mode - Post-Swarm Workflow

When using worktree isolation, after the swarm completes:

```bash
# 1. Shutdown the swarm (agents stop)
/swarm-shutdown $TEAM_ID

# 2. Merge all agent branches back to main
bash $HOME/{{TOOL_DIR}}/utils/swarm-lib.sh merge-worktrees $TEAM_ID

# 3. Review the merged changes
git log --oneline -10
git diff HEAD~5

# 4. Commit or amend as needed
git commit --amend -m "feat: combined swarm output"
```

## Integration with Beads

The swarm integrates with Beads task tracking:

- **Epic**: The parent issue containing all related work
- **Swarm**: `bd swarm create` generates task DAG with dependencies
- **Ready Work**: Leader uses `bd ready --unassigned` to find available tasks
- **Assignment**: Workers claim tasks with `bd update <id> --assignee <agent>`
- **Completion**: Workers close tasks with `bd close <id>`

## Error Handling

| Error | Resolution |
|-------|------------|
| `swarm_create_team: not found` | Use `bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh create-team` — the script exposes subcommands, not shell functions |
| Epic not found | Verify epic ID exists: `bd show <epic-id>` |
| tmux not available | Install tmux: `brew install tmux` |
| jq not available | Install jq: `brew install jq` |
| Max agents exceeded | Limit is 4 workers per team |
| Session already exists | Previous swarm not cleaned up - run `/swarm-shutdown` |
| Worktree conflict | Branch already exists - delete with `git branch -D` |
| Merge conflict | Resolve manually then `git merge --continue` |

## Files Created

### Shared Mode
```
{{HOME_TOOL_DIR}}/swarm/{team-id}/
+-- team.json           # Team metadata (isolation_mode: "shared")
+-- inbox/
|   +-- leader.jsonl    # Leader's message inbox
|   +-- agent-1.jsonl   # Agent 1's inbox
|   +-- agent-2.jsonl   # Agent 2's inbox
+-- shared/
    +-- (shared context files)
```

### Worktree Mode
```
{{HOME_TOOL_DIR}}/swarm/{team-id}/
+-- team.json           # Team metadata (isolation_mode: "worktree")
+-- inbox/
|   +-- leader.jsonl
|   +-- agent-1.jsonl
|   +-- agent-2.jsonl
+-- shared/
+-- worktrees/
    +-- agent-1/        # Git worktree (branch: {team-id}-agent-1)
    |   +-- (full repo copy)
    +-- agent-2/        # Git worktree (branch: {team-id}-agent-2)
        +-- (full repo copy)
```
