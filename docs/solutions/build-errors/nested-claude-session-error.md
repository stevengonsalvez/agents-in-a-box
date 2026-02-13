---
title: "Claude Code nested session error on startup"
category: build-errors
tags: [claude-code, nested-session, tmux, remote-session]
symptoms:
  - "Error: Claude Code cannot be launched inside another Claude Code session"
  - "Nested sessions share runtime resources and will crash all active sessions"
  - "To bypass this check, unset the CLAUDECODE environment variable"
root_cause: "Existing tmux session with running Claude instance, or hook invoking claude CLI"
key_insight: "Kill existing tmux Claude sessions or fix hooks that invoke `claude` CLI"
created: 2026-02-13
confidence: high
---

## Problem

When starting a new Claude Code session (especially remote sessions), you get:

```
Error: Claude Code cannot be launched inside another Claude Code session.
Nested sessions share runtime resources and will crash all active sessions.
To bypass this check, unset the CLAUDECODE environment variable
```

## Common Causes

### 1. Existing tmux sessions with Claude running

Check for existing Claude instances:
```bash
ps aux | grep -i claude | grep -v grep
tmux list-sessions
```

Kill orphaned Claude sessions:
```bash
# Find and kill Claude processes in specific tmux session
tmux kill-session -t <session-name>

# Or kill specific Claude process
kill <PID>
```

### 2. Hooks invoking `claude` CLI

Check settings.json for claude invocations:
```bash
grep -n "claude " ~/.claude/settings.json
```

Known problematic patterns:
- `"command": "claude handover;..."` in PreCompact hook
- `"command": "claude --model haiku ..."` in statusline hooks
- Any spawn-agent scripts that invoke claude without CLAUDECODE check

### 3. Detached subprocesses inheriting CLAUDECODE env var

Scripts that spawn detached processes may inherit CLAUDECODE:
```javascript
// BAD - inherits CLAUDECODE
spawn('bash', ['-c', 'claude ...'], { detached: true });

// GOOD - explicitly unset
spawn('bash', ['-c', 'unset CLAUDECODE; claude ...'], { detached: true });
```

## Solutions

### Quick fix: Kill existing sessions
```bash
# List all tmux sessions
tmux list-sessions

# Kill problematic session
tmux kill-session -t build-ios-1770620167
```

### Permanent fix: Guard scripts against nesting
```bash
# Add to any script that invokes claude
if [ -n "${CLAUDECODE:-}" ]; then
    echo "Already inside Claude Code session, skipping"
    exit 0
fi
```

### Remove claude invocations from hooks
```bash
# Check and fix settings.json
cat ~/.claude/settings.json | grep -A2 -B2 "claude "
```

## Files to check

| Location | What to look for |
|----------|------------------|
| `~/.claude/settings.json` | Hooks with `claude` command |
| `~/.claude/hooks/*.js` | spawn/exec calling claude |
| `~/.claude/utils/spawn-agent-lib.sh` | Agent spawning without guard |
| `~/.claude/utils/swarm-lib.sh` | Swarm spawning without guard |

## Related

- CLAUDECODE environment variable is set by Claude Code on startup
- Used to detect nested invocations and prevent resource conflicts
