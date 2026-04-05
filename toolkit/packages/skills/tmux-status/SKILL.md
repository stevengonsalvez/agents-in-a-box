---
name: tmux-status
description: Show status of all tmux sessions including dev environments, spawned agents, and running processes
user-invocable: true
---

# /tmux-status - Overview of All tmux Sessions

Show status of all tmux sessions including dev environments, spawned agents, and running processes.

## Usage

```bash
/tmux-status              # Compact overview
/tmux-status --detailed   # Full report with metadata
/tmux-status --json       # JSON export
```

## Process

Invokes the `tmux-monitor` skill to discover and report on all active tmux sessions.

```bash
# Get path to monitor script
MONITOR_SCRIPT=".claude/skills/tmux-monitor/scripts/monitor.sh"

[ ! -f "$MONITOR_SCRIPT" ] && echo "❌ tmux-monitor skill not found at $MONITOR_SCRIPT" && exit 1

# Determine output mode
OUTPUT_MODE="compact"
[[ "$1" == "--detailed" ]] || [[ "$1" == "-d" ]] && OUTPUT_MODE="detailed"
[[ "$1" == "--json" ]] || [[ "$1" == "-j" ]] && OUTPUT_MODE="json"

# Execute monitor script
bash "$MONITOR_SCRIPT" "$OUTPUT_MODE"
```

## Output Modes

### Compact (Default)

Quick overview:

```
5 active sessions:
- dev-myapp-1705161234 (fullstack, 4 windows, active)
- dev-api-service-1705159000 (backend-only, 4 windows, detached)
- agent-1705160000 (codex, running)
- agent-1705161000 (aider, completed ✓)
- claude-work (main session, current)

3 running servers on ports: 8432,3891,5160

Use /tmux-status --detailed for full report
```

### Detailed

Full report with metadata, services, ports, and recommendations.

### JSON

Programmatic output:

```json
{
  "sessions": [
    {
      "name": "dev-myapp-1705161234",
      "type": "dev-environment",
      "windows": 4,
      "panes": 8,
      "attached": true
    }
  ],
  "summary": {
    "total_sessions": 5,
    "total_windows": 12,
    "total_panes": 28
  }
}
```

## Contextual Recommendations

After displaying status, provide recommendations based on findings:

**Completed agents**:
```
⚠️  Found completed agent sessions
Recommendation: Review and clean up: tmux kill-session -t <agent-session>
```

**Long-running detached sessions**:
```
💡 Found dev sessions running >2 hours
Recommendation: Check if still needed: tmux attach -t <session-name>
```

**Many sessions (>5)**:
```
🧹 Found 5+ active sessions
Recommendation: Review and clean up unused sessions
```

## Use Cases

### Before Starting New Environment

```bash
/tmux-status
# Check for port conflicts and existing sessions before /start-local
```

### Monitor Agent Progress

```bash
/tmux-status
# See status of spawned agents (running, completed, etc.)
```

### Session Discovery

```bash
/tmux-status --detailed
# Find specific session by project name or port
```

## Notes

- Read-only, never modifies sessions
- Uses tmux-monitor skill for discovery
- Integrates with tmuxwatch if available
- Detects metadata from `.tmux-dev-session.json` and `~/.claude/agents/*.json`
