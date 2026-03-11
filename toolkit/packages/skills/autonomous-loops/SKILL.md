---
name: autonomous-loops
description: |
  Six proven autonomous agent loop patterns with guard rails. Provides
  reusable patterns for generate->validate->fix, explore->hypothesize->test,
  and other autonomous workflows. Includes the reviewer-never-authored
  principle for quality assurance.

  Use when: (1) Building autonomous agent workflows, (2) Designing
  self-correcting pipelines, (3) Implementing agent retry/fix loops,
  (4) Setting up multi-agent review processes,
  (5) User asks about agent loop patterns.
---

# Autonomous Loop Patterns

## Core Principle: Reviewer Never Authored

**The agent that reviews work must never be the agent that authored it.**

This is the single most important principle for autonomous quality. Self-review is
unreliable -- the same blind spots that caused the error will miss it during review.

Implementation:
- Use a separate agent instance (different `subagent_type` or `name`) for review
- The reviewer receives only the output + acceptance criteria, not the generation prompt
- Reviewer can request changes but never edits directly -- sends feedback to the author

## Pattern 1: Generate -> Validate -> Fix

The most common autonomous loop. Generate output, validate against criteria, fix if needed.

```
+----------+     +----------+     +----------+
| Generate |---->| Validate |---->|   Fix    |--+
|          |     |          |     |          |  |
+----------+     +----+-----+     +----------+  |
                      | Pass                     |
                      v                          |
                 +----------+                    |
                 |  Accept  |<-------------------+
                 +----------+     (max 3 iterations)
```

**When to use:** Code generation, document creation, configuration authoring

```python
MAX_ITERATIONS = 3

for iteration in range(MAX_ITERATIONS):
    if iteration == 0:
        output = generate(prompt, context)
    else:
        output = fix(output, validation_errors, context)

    is_valid, errors = validate(output, acceptance_criteria)

    if is_valid:
        return accept(output)

return escalate_to_human(output, errors)
```

**Guard rails:**
- Hard cap on iterations (3 is typical, never exceed 5)
- Each iteration must reduce error count -- if errors increase, break
- Track token cost per iteration -- escalate if cost exceeds threshold

## Pattern 2: Explore -> Hypothesize -> Test

For debugging and investigation. Gather evidence, form theory, validate.

```
+----------+     +-------------+     +----------+
| Explore  |---->| Hypothesize |---->|   Test   |--+
| (gather  |     | (form       |     | (verify  |  |
|  evidence)|    |  theory)    |     |  theory) |  |
+----------+     +-------------+     +----+-----+  |
                                          | Fail   |
                                          v        |
                                     +----------+  |
                                     | Refine   |--+
                                     | hypothesis|
                                     +----------+
```

**When to use:** Bug investigation, root cause analysis, codebase exploration

**Guard rails:**
- Track hypotheses tested to avoid circular reasoning
- Max 5 hypotheses before requesting human input
- Evidence must be concrete (file:line references, error messages)

## Pattern 3: Plan -> Execute -> Verify -> Adjust

For multi-step implementation tasks.

```
+----------+     +----------+     +----------+     +----------+
|   Plan   |---->| Execute  |---->|  Verify  |---->|  Adjust  |--+
| (steps)  |     | (step N) |     | (tests)  |     | (plan)   |  |
+----------+     +----------+     +----------+     +----------+  |
     ^                                                            |
     +------------------------------------------------------------+
```

**When to use:** Feature implementation, refactoring, migration tasks

**Guard rails:**
- Plan must be approved before execution starts
- Verify after EACH step, not just at the end
- Adjustment can only modify future steps, never rewrite completed ones
- If >50% of plan needs adjustment, re-plan from scratch

## Pattern 4: Diverge -> Converge -> Select

For creative or design tasks where multiple approaches are valid.

```
+------------+     +------------+     +----------+
|  Diverge   |---->|  Converge  |---->|  Select  |
| (generate  |     | (evaluate  |     | (pick    |
|  N options)|     |  trade-offs)|    |  best)   |
+------------+     +------------+     +----------+
```

**When to use:** Architecture decisions, API design, UI alternatives

**Guard rails:**
- Generate minimum 3 options (avoids false dichotomies)
- Evaluation criteria defined BEFORE divergence (prevents bias)
- Selection must reference criteria -- no "gut feeling"

## Pattern 5: Seed -> Expand -> Prune

For building up content or code incrementally.

```
+----------+     +----------+     +----------+
|   Seed   |---->|  Expand  |---->|  Prune   |--+
| (minimal |     | (add     |     | (remove  |  |
|  version)|     |  features)|    |  bloat)  |  |
+----------+     +----------+     +----------+  |
                      ^                          |
                      +--------------------------+
                      (until scope complete)
```

**When to use:** MVP development, documentation, test suite building

**Guard rails:**
- Seed must be complete and working before expansion
- Each expansion adds ONE feature/section
- Prune after every 3 expansions
- Prune agent is separate from expand agent (reviewer-never-authored)

## Pattern 6: Observe -> Orient -> Decide -> Act (OODA)

For reactive, event-driven agent workflows.

```
+----------+     +----------+     +----------+     +----------+
| Observe  |---->|  Orient  |---->|  Decide  |---->|   Act    |
| (monitor |     | (analyze |     | (choose  |     | (execute |
|  events) |     |  context)|     |  action) |     |  action) |
+----------+     +----------+     +----------+     +----------+
     ^                                                    |
     +----------------------------------------------------+
```

**When to use:** Monitoring, incident response, CI/CD automation

**Guard rails:**
- Observation must be fresh (re-check state before acting)
- Orientation must include context from previous loops
- Decision must be logged for audit trail
- Action must be reversible or confirmed

## Applying Patterns

### Choosing the Right Pattern

| Task Type | Recommended Pattern |
|-----------|-------------------|
| Code generation / editing | Generate -> Validate -> Fix |
| Bug investigation | Explore -> Hypothesize -> Test |
| Feature implementation | Plan -> Execute -> Verify -> Adjust |
| Architecture / design | Diverge -> Converge -> Select |
| Incremental building | Seed -> Expand -> Prune |
| Monitoring / ops | OODA |

### Combining Patterns

Patterns can be nested. For example:
- **Plan -> Execute** where each Execute step uses **Generate -> Validate -> Fix**
- **Diverge -> Converge** where each option is built with **Seed -> Expand -> Prune**
- **OODA** where the Act phase uses **Plan -> Execute -> Verify -> Adjust**

### Universal Guard Rails

Apply these to ALL patterns:

1. **Max iterations**: Every loop has a hard cap (typically 3-5)
2. **Cost tracking**: Monitor token spend per iteration
3. **Progress check**: Each iteration must demonstrably advance toward the goal
4. **Escalation path**: Clear handoff to human when loop exhausts iterations
5. **Audit trail**: Log each iteration's input, output, and decision
