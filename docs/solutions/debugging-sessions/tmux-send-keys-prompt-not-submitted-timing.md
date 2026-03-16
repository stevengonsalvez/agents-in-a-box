---
title: "Tmux send-keys prompt not submitted on large projects — REPL timing fix"
category: debugging-sessions
tags: [tmux, claude-code, send-keys, timing, swarm, spawn-agent, repl]
symptoms:
  - "Claude Code agent spawned via tmux but prompt never submitted"
  - "Prompt text visible in tmux pane but Enter was swallowed"
  - "Swarm agents start but don't process their assigned task"
  - "Agent session shows bypass permissions splash but no activity"
root_cause: "When spawn_agent_tmux spawns a Claude session, it sends prompt text via tmux send-keys -l then immediately sends C-m (Enter). The 0.5s delay between detecting Claude's splash screen and pasting is insufficient for the REPL to be ready to accept input on large projects. The C-m gets swallowed."
key_insight: "Increase post-ready delay from 0.5s to 3s, and add a retry loop that captures the pane content and re-sends Enter if the prompt appears stuck (splash screen visible but no processing indicators)"
created: "2026-03-16"
confidence: high
language: bash
framework: tmux
---

## Problem

When spawning Claude Code agents via tmux (used by swarm orchestration and
spawn-agent skill), the agent's prompt text gets pasted but never submitted.
The Enter key (C-m) is sent too early — before the Claude REPL is ready to
accept input — and gets swallowed silently.

This is intermittent and more likely on large projects where Claude takes
longer to initialize.

## Solution

### Pattern: Delay + Verify + Retry

```bash
# Wait for REPL to be fully ready (large projects need more time)
sleep 3  # was 0.5

# Send the task
tmux send-keys -t "$SESSION" -l "$TASK"
tmux send-keys -t "$SESSION" C-m

# Wait and verify the prompt was actually submitted
sleep 2

# Check if prompt is still sitting in input (not submitted)
local PANE_CHECK=$(tmux capture-pane -t "$SESSION" -p 2>/dev/null || echo "")
if echo "$PANE_CHECK" | grep -qE "bypass permissions|⏵⏵" && \
   ! echo "$PANE_CHECK" | grep -qE "Thought for|Forming|Creating|⏳|✽|∴|Reading|Searching"; then
    # Prompt may not have been submitted - retry Enter
    sleep 1
    tmux send-keys -t "$SESSION" C-m
    sleep 2
fi
```

### Detection Logic
- **Stuck indicators**: "bypass permissions" or "⏵⏵" visible (splash/input prompt)
- **Processing indicators**: "Thought for", "Forming", "Creating", "⏳", "✽", "∴",
  "Reading", "Searching" (Claude is working)
- If stuck indicators present AND no processing indicators → re-send Enter

### Files Modified
- `~/.claude/utils/spawn-agent-lib.sh` — `spawn_agent_tmux()` function
- `~/.claude/utils/swarm-lib.sh` — `swarm_spawn_leader()` and `swarm_spawn_agent()` fallback blocks

## Context

The same pattern applies to any tmux-based agent spawning. The core issue is that
tmux send-keys delivers keystrokes to the terminal immediately, but the receiving
application (Claude REPL) may not have its input handler ready yet. The retry
verification pattern (capture-pane + grep for indicators) is the reliable fix.
