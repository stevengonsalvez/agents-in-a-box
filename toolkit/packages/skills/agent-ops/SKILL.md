---
name: agent-ops
description: |
  Enterprise SRE patterns for AI agent operations. Provides cost caps,
  circuit breakers, stall detection, observability, and runbook-driven
  incident response for autonomous agent workflows.

  Use when: (1) Running long autonomous agent sessions, (2) Managing multi-agent
  swarms, (3) Monitoring agent costs and performance, (4) Debugging stuck or
  expensive agent loops, (5) Setting up agent observability.
---

# Agent Ops — SRE for AI Agents

## Quick Reference

| Pattern | Purpose |
|---------|---------|
| Cost Cap | Hard-stop agent when spend exceeds budget |
| Circuit Breaker | Halt after N consecutive failures |
| Stall Detector | Kill agent stuck in loops |
| Health Dashboard | Real-time agent metrics |
| Runbook | Automated incident response |

## Cost Cap Pattern

Set maximum spend per session or per agent:

```bash
# Check current session cost
METRICS_FILE="$HOME/{{TOOL_DIR}}/metrics/costs.jsonl"
if [[ -f "$METRICS_FILE" ]]; then
    SESSION_ID="${CLAUDE_SESSION_ID:-default}"
    TOTAL=$(grep "$SESSION_ID" "$METRICS_FILE" | \
        python3 -c "import sys,json; print(sum(json.loads(l)['estimated_cost_usd'] for l in sys.stdin))")
    echo "Session cost: \$$TOTAL"
fi
```

**Implementation in agent workflows:**

```python
# Cost cap check — add to any long-running agent loop
MAX_COST_USD = float(os.environ.get("AGENT_COST_CAP", "5.00"))

def check_cost_cap(session_id: str) -> bool:
    """Return True if within budget, False if exceeded."""
    metrics_file = Path.home() / ".claude" / "metrics" / "costs.jsonl"
    if not metrics_file.exists():
        return True

    total = 0.0
    for line in metrics_file.read_text().splitlines():
        try:
            row = json.loads(line)
            if row.get("session_id") == session_id:
                total += row.get("estimated_cost_usd", 0)
        except json.JSONDecodeError:
            continue

    return total < MAX_COST_USD
```

**Environment variables:**

| Variable | Default | Description |
|----------|---------|-------------|
| `AGENT_COST_CAP` | 5.00 | Max USD per session |
| `AGENT_COST_CAP_WARN` | 0.80 | Warning threshold (fraction of cap) |
| `AGENT_COST_ACTION` | stop | Action on cap: stop, warn, log |

## Circuit Breaker Pattern

Stop after N consecutive failures to prevent cost waste:

```python
class CircuitBreaker:
    """Stop agent after consecutive failures."""

    def __init__(self, max_failures: int = 3, reset_after_success: bool = True):
        self.max_failures = max_failures
        self.reset_after_success = reset_after_success
        self.consecutive_failures = 0
        self.state = "closed"  # closed=healthy, open=tripped, half-open=testing

    def record_success(self):
        if self.reset_after_success:
            self.consecutive_failures = 0
        self.state = "closed"

    def record_failure(self, error: str = ""):
        self.consecutive_failures += 1
        if self.consecutive_failures >= self.max_failures:
            self.state = "open"
            raise CircuitBreakerTripped(
                f"Circuit breaker tripped after {self.max_failures} consecutive failures. "
                f"Last error: {error}"
            )

    def allow_request(self) -> bool:
        return self.state != "open"
```

**When to use:**
- API calls to external services
- Build/test commands that may fail repeatedly
- Agent tool use in autonomous loops

## Stall Detector Pattern

Detect and kill agents stuck in repetitive loops:

```python
class StallDetector:
    """Detect agent stuck in loops by tracking action hashes."""

    def __init__(self, window_size: int = 10, similarity_threshold: float = 0.8):
        self.window_size = window_size
        self.similarity_threshold = similarity_threshold
        self.action_history: list[str] = []

    def record_action(self, action_summary: str):
        self.action_history.append(action_summary)
        if len(self.action_history) > self.window_size * 2:
            self.action_history = self.action_history[-self.window_size * 2:]

    def is_stalled(self) -> bool:
        if len(self.action_history) < self.window_size:
            return False

        recent = self.action_history[-self.window_size:]
        unique_ratio = len(set(recent)) / len(recent)

        # If most recent actions are repetitive, we're stalled
        return unique_ratio < (1 - self.similarity_threshold)
```

**Indicators of stall:**
- Same file read/written > 3 times in a row
- Same error message appearing repeatedly
- Agent alternating between 2-3 actions without progress
- Token usage climbing without task completion

## Health Dashboard

Quick command to check agent health:

```bash
#!/usr/bin/env bash
# agent-health.sh — Quick health check for running agents

echo "=== Agent Health Dashboard ==="
echo ""

# Active tmux sessions (agents)
echo "## Active Agent Sessions"
tmux list-sessions 2>/dev/null | grep -E "^(dev-|agent-|swarm-)" || echo "  No active sessions"
echo ""

# Cost metrics
echo "## Cost Metrics (last 24h)"
METRICS="$HOME/{{TOOL_DIR}}/metrics/costs.jsonl"
if [[ -f "$METRICS" ]]; then
    CUTOFF=$(date -u -v-24H +%Y-%m-%dT%H:%M:%S 2>/dev/null || date -u -d '24 hours ago' +%Y-%m-%dT%H:%M:%S)
    python3 -c "
import json, sys
total = 0
count = 0
for line in open('$METRICS'):
    try:
        r = json.loads(line)
        if r['timestamp'] >= '$CUTOFF':
            total += r.get('estimated_cost_usd', 0)
            count += 1
    except: pass
print(f'  Sessions: {count}')
print(f'  Total cost: \${total:.4f}')
print(f'  Avg cost/session: \${total/max(count,1):.4f}')
"
else
    echo "  No metrics file found"
fi
echo ""

# Disk usage
echo "## Knowledge Base Size"
du -sh "$HOME/{{TOOL_DIR}}/global-learnings/" 2>/dev/null || echo "  No knowledge base"
du -sh "$HOME/{{TOOL_DIR}}/metrics/" 2>/dev/null || echo "  No metrics"
```

## Runbook: Common Agent Incidents

### Agent Stuck in Loop
1. Check tmux session: `tmux capture-pane -t <session> -p -S -50`
2. If stalled: `tmux kill-session -t <session>`
3. Review last actions in transcript
4. Restart with more specific instructions

### Cost Spike
1. Check `{{HOME_TOOL_DIR}}/metrics/costs.jsonl` for recent sessions
2. Identify high-cost session IDs
3. Set cost cap: `export AGENT_COST_CAP=2.00`
4. Consider using haiku for sub-tasks: model parameter in Agent tool

### Agent Producing Wrong Output
1. Check if agent has correct context (CLAUDE.md loaded?)
2. Review agent type — is it specialized enough?
3. Add constraints to agent prompt
4. Consider /reflect to capture the correction

### Swarm Coordination Failure
1. Check team config: `cat {{HOME_TOOL_DIR}}/teams/*/config.json`
2. Verify task list: use TaskList tool
3. Check for blocked tasks with unresolved dependencies
4. Send broadcast to all agents if needed

## Observability Integration

### Structured Logging

All agent operations should log structured events:

```python
import json
from datetime import datetime, timezone
from pathlib import Path

def log_agent_event(event_type: str, data: dict, agent_id: str = "main"):
    """Append structured event to agent log."""
    log_dir = Path.home() / ".claude" / "metrics" / "events"
    log_dir.mkdir(parents=True, exist_ok=True)

    event = {
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "agent_id": agent_id,
        "event_type": event_type,
        **data,
    }

    log_file = log_dir / f"{datetime.now().strftime('%Y-%m-%d')}.jsonl"
    with open(log_file, "a") as f:
        f.write(json.dumps(event) + "\n")
```

### Event Types

| Event | Fields | When |
|-------|--------|------|
| `agent.start` | model, task_summary | Agent spawned |
| `agent.complete` | duration_ms, tokens_used | Agent finished |
| `agent.error` | error_type, message | Agent failed |
| `agent.cost` | input_tokens, output_tokens, cost_usd | Per-turn cost |
| `circuit.trip` | failures, last_error | Circuit breaker tripped |
| `stall.detect` | action_count, unique_ratio | Stall detected |
| `cost.warn` | current_cost, cap | Approaching cost cap |
| `cost.exceed` | current_cost, cap | Cost cap exceeded |
