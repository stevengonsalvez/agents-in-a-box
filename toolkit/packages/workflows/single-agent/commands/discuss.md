# Discuss Phase

Capture implementation decisions BEFORE planning. Produces a CONTEXT.md
with locked decisions that /plan must honor.

## When to Use

- Before `/plan` when the phase has ambiguities
- When starting a new phase in `.planning/ROADMAP.md`
- When the team needs to agree on approach before implementation

## Process

### Step 1: Load Phase Context

1. Read `.planning/ROADMAP.md` to identify the target phase
2. Read `.planning/STATE.md` for current position
3. If a phase number is provided as argument, use that
4. If no argument, use current phase from STATE.md

### Step 2: Identify Gray Areas

Based on what the phase is building, identify 3-4 ambiguities:

| Domain | Typical Gray Areas |
|--------|-------------------|
| UI/Frontend | Layout approach, interaction patterns, responsive behavior, state management |
| API/Backend | Response formats, error handling, auth strategy, data validation |
| Data/Storage | Schema design, migration approach, indexing strategy |
| Infrastructure | Deployment target, scaling approach, monitoring |

Present them:
```
Phase [N]: [Name]

I've identified these areas that need decisions before planning:

1. **[Area 1]**: [Why this is ambiguous]
2. **[Area 2]**: [Why this is ambiguous]
3. **[Area 3]**: [Why this is ambiguous]

Which areas would you like to discuss? (or "all")
```

### Step 3: Deep-Dive Questions

For each selected area, ask 3-4 probing questions using AskUserQuestion:
- Concrete choices (not abstract preferences)
- Trade-off acknowledgment
- Edge case handling
- Prior art / references

### Step 4: Generate CONTEXT.md

Write to `.planning/phases/{phase}/CONTEXT.md`:

```markdown
# Phase [N] Context: [Phase Name]

## Scope (from Roadmap)
[Copy from ROADMAP.md]

## Implementation Decisions

### [Area 1]
- **Decision**: [Concrete choice]
- **Rationale**: [Why this choice]
- **Trade-off**: [What we're giving up]

### [Area 2]
...

## Deferred Ideas
- [Idea mentioned but deferred to later phase]

---
*Locked on [date]. /plan must honor these decisions.*
```

### Step 5: Update State

Update `.planning/STATE.md` to record that discussion is complete for this phase.

## Without .planning/

If no `.planning/` directory exists, /discuss still works:
- Ask what area/feature the user wants to discuss
- Run the same gray-area analysis and questioning
- Output to `plans/discuss_[topic]_[date].md` instead
