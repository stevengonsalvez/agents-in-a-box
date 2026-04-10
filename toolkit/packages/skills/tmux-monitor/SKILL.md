---
name: tmux-monitor
description: Monitor and report status of all tmux sessions including dev environments, spawned agents, and running processes. Uses tmuxwatch for enhanced visibility.
version: 1.0.0
---

# tmux-monitor Skill

## Purpose

Provide comprehensive visibility into all active tmux sessions, running processes, and spawned agents. This skill enables checking what's running where without needing to manually inspect each session.

## Capabilities

1. **Session Discovery**: Find and categorize all tmux sessions
2. **Process Inspection**: Identify running servers, dev environments, agents
3. **Port Mapping**: Show which ports are in use and by what
4. **Status Reporting**: Generate detailed reports with recommendations
5. **tmuxwatch Integration**: Use tmuxwatch for enhanced real-time monitoring
6. **Metadata Extraction**: Read session metadata from .tmux-dev-session.json and agent JSON files

## When to Use

- User asks "what's running?"
- Before starting new dev environments (check port conflicts)
- After spawning agents (verify they started correctly)
- When debugging server/process issues
- Before session cleanup
- When context switching between projects

## Implementation

### Step 1: Check tmux Availability

```bash
if ! command -v tmux &> /dev/null; then
    echo "❌ tmux is not installed"
    exit 1
fi

if ! tmux list-sessions 2>/dev/null; then
    echo "✅ No tmux sessions currently running"
    exit 0
fi
```

### Step 2: Discover All Sessions

```bash
# Get all sessions with metadata
SESSIONS=$(tmux list-sessions -F '#{session_name}|#{session_windows}|#{session_created}|#{session_attached}')

# Count sessions
TOTAL_SESSIONS=$(echo "$SESSIONS" | wc -l | tr -d ' ')
```

### Step 3: Categorize Sessions

Group by prefix pattern:

- `dev-*` → Development environments
- `agent-*` → Spawned agents
- `claude-*` → Claude Code sessions
- `monitor-*` → Monitoring sessions
- Others → Miscellaneous

```bash
DEV_SESSIONS=$(echo "$SESSIONS" | grep "^dev-" || true)
AGENT_SESSIONS=$(echo "$SESSIONS" | grep "^agent-" || true)
CLAUDE_SESSIONS=$(echo "$SESSIONS" | grep "^claude-" || true)
```

### Step 4: Extract Details for Each Session

For each session, gather:

**Window Information**:
```bash
tmux list-windows -t "$SESSION" -F '#{window_index}:#{window_name}:#{window_panes}'
```

**Running Processes** (from first pane of each window):
```bash
tmux capture-pane -t "$SESSION:0.0" -p -S -10 -E 0
```

**Port Detection** (check for listening ports):
```bash
# Extract ports from session metadata
if [ -f ".tmux-dev-session.json" ]; then
    BACKEND_PORT=$(jq -r '.backend.port // empty' .tmux-dev-session.json)
    FRONTEND_PORT=$(jq -r '.frontend.port // empty' .tmux-dev-session.json)
fi

# Or detect from process list
lsof -nP -iTCP -sTCP:LISTEN | grep -E "node|python|uv|npm"
```

### Step 5: Load Session Metadata

**Dev Environment Metadata** (`.tmux-dev-session.json`):
```bash
if [ -f ".tmux-dev-session.json" ]; then
    PROJECT=$(jq -r '.project' .tmux-dev-session.json)
    TYPE=$(jq -r '.type' .tmux-dev-session.json)
    BACKEND_PORT=$(jq -r '.backend.port // "N/A"' .tmux-dev-session.json)
    FRONTEND_PORT=$(jq -r '.frontend.port // "N/A"' .tmux-dev-session.json)
    CREATED=$(jq -r '.created' .tmux-dev-session.json)
fi
```

**Agent Metadata** (`{{HOME_TOOL_DIR}}/agents/*.json`):
```bash
if [ -f "$HOME/{{TOOL_DIR}}/agents/${SESSION}.json" ]; then
    AGENT_TYPE=$(jq -r '.agent_type' "$HOME/{{TOOL_DIR}}/agents/${SESSION}.json")
    TASK=$(jq -r '.task' "$HOME/{{TOOL_DIR}}/agents/${SESSION}.json")
    STATUS=$(jq -r '.status' "$HOME/{{TOOL_DIR}}/agents/${SESSION}.json")
    DIRECTORY=$(jq -r '.directory' "$HOME/{{TOOL_DIR}}/agents/${SESSION}.json")
    CREATED=$(jq -r '.created' "$HOME/{{TOOL_DIR}}/agents/${SESSION}.json")
fi
```

### Step 6: tmuxwatch Integration

If tmuxwatch is available, offer enhanced view:

```bash
if command -v tmuxwatch &> /dev/null; then
    echo ""
    echo "📊 Enhanced Monitoring Available:"
    echo "   Real-time TUI: tmuxwatch"
    echo "   JSON export:   tmuxwatch --dump | jq"
    echo ""

    # Optional: Use tmuxwatch for structured data
    TMUXWATCH_DATA=$(tmuxwatch --dump 2>/dev/null || echo "{}")
fi
```

### Step 7: Generate Comprehensive Report

```markdown
# tmux Sessions Overview

**Total Active Sessions**: {count}
**Total Windows**: {window_count}
**Total Panes**: {pane_count}

---

## Development Environments ({dev_count})

### 1. dev-myapp-1705161234
- **Type**: fullstack
- **Project**: myapp
- **Status**: ⚡ Active (attached)
- **Windows**: 4 (servers, logs, claude-work, git)
- **Panes**: 8
- **Backend**: Port 8432 → http://localhost:8432
- **Frontend**: Port 3891 → http://localhost:3891
- **Created**: 2025-01-13 14:30:00 (2h ago)
- **Attach**: `tmux attach -t dev-myapp-1705161234`

---

## Spawned Agents ({agent_count})

### 2. agent-1705160000
- **Agent Type**: codex
- **Task**: Refactor authentication module
- **Status**: ⚙️  Running (15 minutes)
- **Working Directory**: /Users/stevie/projects/myapp
- **Git Worktree**: worktrees/agent-1705160000
- **Windows**: 1 (work)
- **Panes**: 2 (agent | monitoring)
- **Last Output**: "Analyzing auth.py dependencies..."
- **Attach**: `tmux attach -t agent-1705160000`
- **Metadata**: `{{HOME_TOOL_DIR}}/agents/agent-1705160000.json`

### 3. agent-1705161000
- **Agent Type**: aider
- **Task**: Generate API documentation
- **Status**: ✅ Completed (5 minutes ago)
- **Output**: Documentation written to docs/api/
- **Attach**: `tmux attach -t agent-1705161000` (review)
- **Cleanup**: `tmux kill-session -t agent-1705161000`

---

## Running Processes Summary

| Port | Service      | Session                  | Status  |
|------|--------------|--------------------------|---------|
| 8432 | Backend API  | dev-myapp-1705161234     | Running |
| 3891 | Frontend Dev | dev-myapp-1705161234     | Running |
| 5160 | Supabase     | dev-shotclubhouse-xxx    | Running |

---

## Quick Actions

**Attach to session**:
```bash
tmux attach -t <session-name>
```

**Kill session**:
```bash
tmux kill-session -t <session-name>
```

**List all sessions**:
```bash
tmux ls
```

**Kill all completed agents**:
```bash
for session in $(tmux ls | grep "^agent-" | cut -d: -f1); do
    STATUS=$(jq -r '.status' "{{HOME_TOOL_DIR}}/agents/${session}.json" 2>/dev/null)
    if [ "$STATUS" = "completed" ]; then
        tmux kill-session -t "$session"
    fi
done
```

---

## Recommendations

{generated based on findings}
```

### Step 8: Provide Contextual Recommendations

**If completed agents found**:
```
⚠️  Found 1 completed agent session:
   - agent-1705161000: Task completed 5 minutes ago

Recommendation: Review results and clean up:
  tmux attach -t agent-1705161000  # Review
  tmux kill-session -t agent-1705161000  # Cleanup
```

**If long-running detached sessions**:
```
💡 Found detached session running for 2h 40m:
   - dev-api-service-1705159000

Recommendation: Check if still needed:
  tmux attach -t dev-api-service-1705159000
```

**If port conflicts detected**:
```
⚠️  Port conflict detected:
   - Port 3000 in use by dev-oldproject-xxx
   - New session will use random port instead

Recommendation: Clean up old session if no longer needed
```

## Output Formats

### Compact (Default)

```
5 active sessions:
- dev-myapp-1705161234 (fullstack, 4 windows, active)
- dev-api-service-1705159000 (backend-only, 4 windows, detached)
- agent-1705160000 (codex, running 15m)
- agent-1705161000 (aider, completed ✓)
- claude-work (main session, current)

3 running servers:
- Port 8432: Backend API (dev-myapp)
- Port 3891: Frontend Dev (dev-myapp)
- Port 5160: Supabase (dev-shotclubhouse)
```

### Detailed (Verbose)

Full report with all metadata, sample output, recommendations.

### JSON (Programmatic)

```json
{
  "sessions": [
    {
      "name": "dev-myapp-1705161234",
      "type": "dev-environment",
      "category": "fullstack",
      "windows": 4,
      "panes": 8,
      "status": "attached",
      "created": "2025-01-13T14:30:00Z",
      "ports": {
        "backend": 8432,
        "frontend": 3891
      },
      "metadata_file": ".tmux-dev-session.json"
    },
    {
      "name": "agent-1705160000",
      "type": "spawned-agent",
      "agent_type": "codex",
      "task": "Refactor authentication module",
      "status": "running",
      "runtime": "15m",
      "directory": "/Users/stevie/projects/myapp",
      "worktree": "worktrees/agent-1705160000",
      "metadata_file": "{{HOME_TOOL_DIR}}/agents/agent-1705160000.json"
    }
  ],
  "summary": {
    "total_sessions": 5,
    "total_windows": 12,
    "total_panes": 28,
    "running_servers": 3,
    "active_agents": 1,
    "completed_agents": 1
  },
  "ports": [
    {"port": 8432, "service": "Backend API", "session": "dev-myapp-1705161234"},
    {"port": 3891, "service": "Frontend Dev", "session": "dev-myapp-1705161234"},
    {"port": 5160, "service": "Supabase", "session": "dev-shotclubhouse-xxx"}
  ]
}
```

## Integration with Commands

This skill is used by:
- `/tmux-status` command (user-facing command)
- Automatically before starting new dev environments (conflict detection)
- By spawned agents to check session status

## Dependencies

- `tmux` (required)
- `jq` (required for JSON parsing)
- `lsof` (optional, for port detection)
- `tmuxwatch` (optional, for enhanced monitoring)

## File Structure

```
{{HOME_TOOL_DIR}}/agents/
  agent-{timestamp}.json           # Agent metadata

.tmux-dev-session.json             # Dev environment metadata (per project)

/tmp/tmux-monitor-cache.json       # Optional cache for performance
```

## Related Commands

- `/tmux-status` - User-facing wrapper around this skill
- `/spawn-agent` - Creates sessions that this skill monitors
- `/start-local`, `/start-ios`, `/start-android` - Create dev environments

---

## Steering Protocol

Beyond monitoring, use these patterns to actively manage coding sessions.

### Session Naming Convention

Use consistent names so sessions are self-documenting and parseable:

```
{tool}-{scope}-{id}[-{desc}]
```

| Segment | Values | Example |
|---------|--------|---------|
| `tool` | `cc` (Claude Code), `codex`, `pi` | `cc` |
| `scope` | `issue`, `fix`, `pr`, `feature`, `task` | `issue` |
| `id` | Issue number, PR number, or short identifier | `174` |
| `desc` | Optional short slug | `auth-refactor` |

**Examples:** `cc-issue-174-auth-refactor`, `codex-fix-1520`, `cc-pr-186`

### Monitoring Signals

When monitoring a coding session, look for these signals:

| Signal | Action |
|--------|--------|
| Model is generating code | Let it work |
| "I'll now..." planning text | Verify approach is correct |
| Error/stack trace repeating | Intervene (steer) |
| Idle / waiting for input | Check if stuck or done |
| "I've completed..." summary | Move to verify phase |
| Session has exited | Check output / PR status |

### Steering (when intervention is needed)

```bash
# Correct course
tmux send-keys -t "${SESSION}" \
  "STOP. You're going the wrong direction. <specific correction>" Enter

# Unstick
tmux send-keys -t "${SESSION}" \
  "You seem stuck. Try: <specific suggestion>" Enter

# Add context
tmux send-keys -t "${SESSION}" \
  "Additional context: <file path, pattern, constraint>" Enter

# Abort
tmux send-keys -t "${SESSION}" "/exit" Enter
```

### Steering Rules
1. **Be specific.** "Fix the types" is bad. "Change `costTotal` to `v.optional(v.float64())`" is good.
2. **Don't over-steer.** Interruptions reset context. If on track, let it work.
3. **One correction at a time.** Multiple simultaneous corrections confuse the model.
4. **Stuck 3+ times on same issue?** Kill session, rethink approach, respawn with better prompt.

### Completion Detection

Detect session state from `capture-pane` output:

```bash
OUTPUT=$(tmux capture-pane -t "$SESSION" -p | tail -30)

# Completion signals
echo "$OUTPUT" | grep -qE "I've completed|changes have been|PR.*created|committed.*pushed" && echo "DONE"

# Error signals
echo "$OUTPUT" | grep -qE "Error:|FATAL|panic|OOM|killed|context.*exhausted" && echo "ERROR"

# Stuck signals (same error repeating 3+ times)
echo "$OUTPUT" | grep -iE "error|fail|fatal|panic" | sort | uniq -c | sort -rn | head -1 | awk '$1 >= 3 {print "STUCK"}'
```

### Session Health Check

Periodically check for stale sessions:

```bash
tmux list-sessions -F '#{session_name}' 2>/dev/null | while IFS= read -r s; do
  [ -z "$s" ] && continue
  LAST_ACTIVITY=$(tmux display -t "$s" -p '#{session_activity}')
  IDLE_SECS=$(( $(date +%s) - LAST_ACTIVITY ))
  if [ $IDLE_SECS -gt 3600 ]; then
    echo "⚠️ $s idle for ${IDLE_SECS}s — consider cleanup"
  fi
done
```

---

## Notes

- This skill is read-only (monitoring/discovery), never modifies sessions
- Steering commands above are for manual intervention when needed
- Safe to run anytime without side effects
- Provides snapshot of current state
- Can be cached for performance (TTL: 10 seconds)
- Should be run before potentially conflicting operations
