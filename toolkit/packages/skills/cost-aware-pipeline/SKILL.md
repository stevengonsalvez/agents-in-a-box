---
name: cost-aware-pipeline
description: |
  Cost-aware LLM pipeline patterns for optimal model routing, narrow retry
  strategies, and prompt caching. Reduces API costs 40-70% through intelligent
  model selection, targeted retries, and cache-friendly prompt structures.

  Use when: (1) Building multi-model pipelines, (2) Optimizing API costs,
  (3) Designing retry strategies for LLM calls, (4) Implementing prompt caching,
  (5) Choosing between haiku/sonnet/opus for sub-tasks.
---

# Cost-Aware LLM Pipeline

## Model Routing Strategy

Route tasks to the cheapest model that can handle them reliably.

### Pricing Reference (per 1M tokens, USD)

| Model | Input | Output | Relative Cost |
|-------|-------|--------|---------------|
| Haiku | $0.80 | $4.00 | 1x (baseline) |
| Sonnet | $3.00 | $15.00 | ~4x |
| Opus | $15.00 | $75.00 | ~19x |

### Routing Rules

| Task Complexity | Route To | Examples |
|-----------------|----------|----------|
| **Simple** (< 100 lines, clear pattern) | Haiku | File renaming, simple search, format conversion, status checks |
| **Moderate** (100-500 lines, some judgment) | Sonnet | Code review, test writing, refactoring, documentation |
| **Complex** (500+ lines, deep reasoning) | Opus | Architecture design, debugging subtle issues, multi-file refactoring |
| **Creative** (open-ended, high quality bar) | Opus | System design, novel algorithms, critical security review |

### Implementation with Claude Code Agent Tool

```python
# In agent orchestration, specify model based on task complexity
def route_to_model(task_description: str, estimated_complexity: str) -> str:
    """Return model parameter for Agent tool."""
    routing = {
        "simple": "haiku",
        "moderate": "sonnet",
        "complex": "opus",
        "creative": "opus",
    }
    return routing.get(estimated_complexity, "sonnet")
```

When spawning sub-agents:
- Use `model: "haiku"` for Explore agents doing simple file searches
- Use `model: "sonnet"` (default) for most implementation work
- Use `model: "opus"` only for architecture reviews, complex debugging, distinguished-engineer tasks

### Cost Savings Examples

| Scenario | Before (all Opus) | After (routed) | Savings |
|----------|-------------------|----------------|---------|
| 10 file searches | $0.15 | $0.008 (haiku) | 95% |
| 5 code reviews | $0.75 | $0.20 (sonnet) | 73% |
| 1 architecture review | $0.15 | $0.15 (opus) | 0% |
| **Typical session** | **$1.05** | **$0.36** | **66%** |

## Narrow Retry Strategy

When an LLM call fails, retry intelligently — not blindly.

### Principles

1. **Retry the minimum scope** — Don't re-run the entire pipeline, retry only the failed step
2. **Escalate model on retry** — If haiku fails, retry with sonnet, not haiku again
3. **Add context on retry** — Include the error in the retry prompt
4. **Max 2 retries** — If it fails 3 times, escalate to human

### Retry Escalation Ladder

```
Attempt 1: haiku (cheapest)
    | fail
    v
Attempt 2: sonnet + error context
    | fail
    v
Attempt 3: opus + error context + examples
    | fail
    v
Escalate to human
```

### Implementation

```python
RETRY_LADDER = [
    {"model": "haiku", "extra_context": None},
    {"model": "sonnet", "extra_context": "Previous attempt failed: {error}"},
    {"model": "opus", "extra_context": "Two attempts failed. Errors: {errors}. Please reason carefully."},
]

async def call_with_retry(prompt: str, task: str) -> str:
    errors = []

    for attempt in RETRY_LADDER:
        try:
            result = await call_llm(
                model=attempt["model"],
                prompt=prompt,
                extra_context=attempt["extra_context"].format(
                    error=errors[-1] if errors else "",
                    errors="; ".join(errors)
                ) if attempt["extra_context"] and errors else prompt,
            )
            return result
        except Exception as e:
            errors.append(str(e))

    raise EscalationNeeded(f"Failed after {len(RETRY_LADDER)} attempts: {errors}")
```

## Prompt Caching Strategies

Structure prompts to maximize cache hits and reduce costs.

### Cache-Friendly Prompt Structure

```
+-------------------------------------+
| STATIC SYSTEM PROMPT (cached)       |  <- Rarely changes, high cache hit rate
| - Agent instructions                |
| - Tool definitions                  |
| - Project context (CLAUDE.md)       |
+-------------------------------------+
| SEMI-STATIC CONTEXT (cached)        |  <- Changes per-task, not per-turn
| - File contents being worked on     |
| - Relevant documentation            |
| - Test results                      |
+-------------------------------------+
| DYNAMIC CONTENT (not cached)        |  <- Changes every turn
| - User message                      |
| - Latest tool results               |
| - Conversation tail                 |
+-------------------------------------+
```

### Key Principles

1. **Front-load static content** — Put rarely-changing content at the start of the prompt
2. **Batch file reads** — Read all needed files at once, not one at a time
3. **Minimize prompt churn** — Avoid reformatting the same content differently each turn
4. **Use system prompts for stable context** — CLAUDE.md, agent definitions, skill bodies

### Cost Impact of Caching

With Anthropic's prompt caching:
- Cached tokens cost 90% less on input
- Cache write costs 25% more (one-time)
- Cache TTL: 5 minutes (refreshed on use)

**Strategy**: Keep system prompt + project context stable across turns. Only the user message and tool results should change.

## Budget Planning

### Estimating Session Costs

| Session Type | Typical Tokens | Estimated Cost |
|-------------|----------------|----------------|
| Quick fix (5 min) | 50K in / 10K out | $0.15 - $0.90 |
| Feature implementation (30 min) | 500K in / 100K out | $1.50 - $9.00 |
| Deep debugging (1 hr) | 1M in / 200K out | $3.00 - $18.00 |
| Multi-agent swarm (2 hr) | 5M in / 1M out | $15.00 - $90.00 |

### Setting Budgets

```bash
# Per-session cost cap (used by agent-ops skill)
export AGENT_COST_CAP=5.00

# Per-agent cost cap for sub-agents
export SUBAGENT_COST_CAP=1.00

# Warning threshold (fraction of cap)
export AGENT_COST_CAP_WARN=0.80
```

### Monitoring

```bash
# View today's costs
grep "$(date +%Y-%m-%d)" /.claude/metrics/costs.jsonl | \
  python3 -c "import sys,json; rows=[json.loads(l) for l in sys.stdin]; \
  print(f'Sessions: {len(set(r[\"session_id\"] for r in rows))}'); \
  print(f'Total: \${sum(r[\"estimated_cost_usd\"] for r in rows):.4f}')"
```
