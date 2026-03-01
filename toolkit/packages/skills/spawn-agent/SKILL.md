---
name: spawn-agent
description: Spawn Claude Agent in tmux Session
user-invocable: true
---

# /spawn-agent - Spawn Claude Agent in tmux Session

Spawn a Claude Code agent in a separate tmux session with optional handover context.

## Usage

```bash
/spawn-agent "implement user authentication"
/spawn-agent "refactor the API layer" --with-handover
/spawn-agent "implement feature X" --with-worktree
/spawn-agent "review the PR" --with-worktree --with-handover
```

## Implementation

```bash
#!/bin/bash

# Function: Wait for Claude Code to be ready for input
wait_for_claude_ready() {
    local SESSION=$1
    local TIMEOUT=30
    local START=$(date +%s)

    echo "Waiting for Claude to initialize..."

    while true; do
        # Capture pane output (suppress errors if session not ready)
        PANE_OUTPUT=$(tmux capture-pane -t "$SESSION" -p 2>/dev/null)

        # Check for Claude prompt/splash (any of these indicates readiness)
        if echo "$PANE_OUTPUT" | grep -qE "Claude Code|Welcome back|------|Style:|bypass permissions"; then
            # Verify not in error state
            if ! echo "$PANE_OUTPUT" | grep -qiE "error|crash|failed|command not found"; then
                echo "Claude initialized successfully"
                return 0
            fi
        fi

        # Timeout check
        local ELAPSED=$(($(date +%s) - START))
        if [ $ELAPSED -gt $TIMEOUT ]; then
            echo "Timeout: Claude did not initialize within ${TIMEOUT}s"
            echo "Capturing debug output..."
            tmux capture-pane -t "$SESSION" -p > "/tmp/spawn-agent-${SESSION}-failure.log" 2>&1
            echo "Debug output saved to /tmp/spawn-agent-${SESSION}-failure.log"
            return 1
        fi

        sleep 0.2
    done
}

# Parse arguments
TASK="$1"
WITH_HANDOVER=false
WITH_WORKTREE=false
shift

# Parse flags
while [[ $# -gt 0 ]]; do
    case $1 in
        --with-handover)
            WITH_HANDOVER=true
            shift
            ;;
        --with-worktree)
            WITH_WORKTREE=true
            shift
            ;;
        *)
            shift
            ;;
    esac
done

if [ -z "$TASK" ]; then
    echo "Task description required"
    echo "Usage: /spawn-agent \"task description\" [--with-handover] [--with-worktree]"
    exit 1
fi

# Generate session info
TASK_ID=$(date +%s)
SESSION="agent-${TASK_ID}"

# Setup working directory (worktree or current)
if [ "$WITH_WORKTREE" = true ]; then
    # Detect transcrypt (informational only - works transparently with worktrees)
    if git config --get-regexp '^transcrypt\.' >/dev/null 2>&1; then
        echo "Transcrypt detected - worktree will inherit encryption config automatically"
        echo ""
    fi

    # Get current branch as base
    CURRENT_BRANCH=$(git branch --show-current 2>/dev/null || echo "HEAD")

    # Generate task slug from task description
    TASK_SLUG=$(echo "$TASK" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9 -]//g' | tr -s ' ' '-' | cut -c1-40 | sed 's/-$//')

    # Source worktree utilities
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    source "$SCRIPT_DIR/../utils/git-worktree-utils.sh"

    # Create worktree with task slug
    echo "Creating isolated git worktree..."
    WORK_DIR=$(create_agent_worktree "$TASK_ID" "$CURRENT_BRANCH" "$TASK_SLUG")
    AGENT_BRANCH="agent/agent-${TASK_ID}"

    echo "Worktree created:"
    echo "   Directory: $WORK_DIR"
    echo "   Branch: $AGENT_BRANCH"
    echo "   Base: $CURRENT_BRANCH"
    echo ""
else
    WORK_DIR=$(pwd)
    AGENT_BRANCH=""
fi

echo "Spawning Claude agent in tmux session..."
echo ""

# Generate handover if requested
HANDOVER_CONTENT=""
if [ "$WITH_HANDOVER" = true ]; then
    echo "Generating handover context..."

    # Get current branch and recent commits
    CURRENT_BRANCH=$(git branch --show-current 2>/dev/null || echo "unknown")
    RECENT_COMMITS=$(git log --oneline -5 2>/dev/null || echo "No git history")
    GIT_STATUS=$(git status -sb 2>/dev/null || echo "Not a git repo")

    # Create handover content
    HANDOVER_CONTENT=$(cat << EOF

# Handover Context

## Current State
- Branch: $CURRENT_BRANCH
- Directory: $WORK_DIR
- Time: $(date)

## Recent Commits
$RECENT_COMMITS

## Git Status
$GIT_STATUS

## Your Task
$TASK

---
Please review the above context and proceed with the task.
EOF
)

    echo "Handover generated"
    echo ""
fi

# Create tmux session
tmux new-session -d -s "$SESSION" -c "$WORK_DIR"

# Verify session creation
if ! tmux has-session -t "$SESSION" 2>/dev/null; then
    echo "Failed to create tmux session"
    exit 1
fi

echo "Created tmux session: $SESSION"
echo ""

# Start Claude Code in the session
tmux send-keys -t "$SESSION" "claude --dangerously-skip-permissions" C-m

# Wait for Claude to be ready (not just sleep!)
if ! wait_for_claude_ready "$SESSION"; then
    echo "Failed to start Claude agent - cleaning up..."
    tmux kill-session -t "$SESSION" 2>/dev/null
    exit 1
fi

# Additional small delay for UI stabilization
sleep 0.5

# Send handover context if generated (line-by-line to handle newlines)
if [ "$WITH_HANDOVER" = true ]; then
    echo "Sending handover context to agent..."

    # Send line-by-line to handle multi-line content properly
    echo "$HANDOVER_CONTENT" | while IFS= read -r LINE || [ -n "$LINE" ]; do
        # Use -l flag to send literal text (handles special characters)
        tmux send-keys -t "$SESSION" -l "$LINE"
        tmux send-keys -t "$SESSION" C-m
        sleep 0.05  # Small delay between lines
    done

    # Final Enter to submit
    tmux send-keys -t "$SESSION" C-m
    sleep 0.5
fi

# Send the task (use literal mode for safety with special characters)
echo "Sending task to agent..."
tmux send-keys -t "$SESSION" -l "$TASK"
tmux send-keys -t "$SESSION" C-m

# Small delay for Claude to start processing
sleep 1

# Verify task was received by checking if Claude is processing
CURRENT_OUTPUT=$(tmux capture-pane -t "$SESSION" -p 2>/dev/null)
if echo "$CURRENT_OUTPUT" | grep -qE "Thought for|Forming|Creating|Implement|*|~"; then
    echo "Task received and processing"
elif echo "$CURRENT_OUTPUT" | grep -qE "error|failed|crash"; then
    echo "Warning: Detected error in agent output"
    echo "Last 10 lines of output:"
    tmux capture-pane -t "$SESSION" -p | tail -10
else
    echo "Task sent (unable to confirm receipt - agent may still be starting)"
fi

echo ""
echo "----------------------------------------------------------------"
echo "Agent spawned successfully!"
echo "----------------------------------------------------------------"
echo ""
echo "Session: $SESSION"
echo "Task: $TASK"
echo "Directory: $WORK_DIR"
echo ""
echo "To monitor:"
echo "  tmux attach -t $SESSION"
echo ""
echo "To send more commands:"
echo "  tmux send-keys -t $SESSION \"your command\" C-m"
echo ""
echo "To kill session:"
echo "  tmux kill-session -t $SESSION"
echo ""

# Source spawn-agent-lib for enhanced metadata functions
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [ -f "$SCRIPT_DIR/../utils/spawn-agent-lib.sh" ]; then
    source "$SCRIPT_DIR/../utils/spawn-agent-lib.sh"

    # Save enhanced metadata with transcript path support
    save_agent_metadata "$SESSION" "$TASK" "$WORK_DIR" "$WITH_HANDOVER" "$WITH_WORKTREE" "$AGENT_BRANCH"

    # Try to update transcript path after a brief delay (transcript may not exist immediately)
    (sleep 5 && update_agent_transcript "$SESSION" "$WORK_DIR" 2>/dev/null) &
else
    # Fallback: save basic metadata if library not found
    mkdir -p ~/.claude/agents
    cat > ~/.claude/agents/${SESSION}.json <<EOF
{
  "session": "$SESSION",
  "task": "$TASK",
  "directory": "$WORK_DIR",
  "created": "$(date -Iseconds)",
  "status": "running",
  "with_handover": $WITH_HANDOVER,
  "with_worktree": $WITH_WORKTREE,
  "worktree_branch": "$AGENT_BRANCH"
}
EOF
fi

exit 0
```

## Notes

- Default agent is Claude Code with `--dangerously-skip-permissions`
- Agent runs in tmux session named `agent-{timestamp}`
- Use `tmux attach -t agent-{timestamp}` to monitor
- Use `tmux send-keys` to send additional prompts
- Metadata saved to `~/.claude/agents/agent-{timestamp}.json`
- **Session Recovery**: If tmux dies (crash/shutdown), use `/recover-sessions` to resume
- Transcript path tracked for `--resume` capability
- Session events logged to `~/.claude/agents/registry.jsonl`
- Robust readiness detection with 30s timeout
- Multi-line input handled correctly via line-by-line sending
- Verification that task was received and processing started
- Debug logs saved to `/tmp/spawn-agent-{session}-failure.log` on failures

## Worktree Isolation

- Use `--with-worktree` flag for isolated git workspace
- Creates worktree in `worktrees/agent-{timestamp}-{task-slug}`
  - Example: `worktrees/agent-1763250000-implement-user-auth`
- Creates branch `agent/agent-{timestamp}` based on current branch
- Transcrypt encryption inherited automatically (no special setup needed)
- Use `/list-agent-worktrees` to see all worktrees
- Use `/cleanup-agent-worktree {timestamp}` to remove when done

## Troubleshooting

If spawn-agent fails:
1. Check debug log: `/tmp/spawn-agent-{session}-failure.log`
2. Verify Claude Code is installed: `which claude`
3. Verify tmux is installed: `which tmux`
4. Check existing sessions: `tmux list-sessions`
5. Manually attach to debug: `tmux attach -t agent-{timestamp}`
