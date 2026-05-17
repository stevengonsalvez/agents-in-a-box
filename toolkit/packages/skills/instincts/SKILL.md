---
name: instincts
description: |
  Atomic behavioral instincts system. Captures micro-learnings as lightweight
  YAML entries with confidence scoring (0.3-0.9). Project-scoped instincts
  are stored locally; universal instincts feed into the reflect-kb GraphRAG
  knowledge base for cross-project retrieval.

  Use when: (1) A behavioral pattern should be remembered but is too small
  for a full learning note, (2) Building up project-specific conventions,
  (3) User wants quick lightweight corrections captured, (4) Accumulating
  micro-patterns during a session, (5) User requests /instincts.
---

# Instincts — Atomic Behavioral Micro-Learnings

## Philosophy

Not every learning needs a full knowledge note. Some are tiny:
- "This project uses tabs, not spaces"
- "Always run `pnpm` not `npm` here"
- "The API returns dates as Unix timestamps, not ISO"

These are **instincts** — atomic rules with confidence that grow stronger through
reinforcement and decay through contradiction.

## Quick Reference

| Command | Action |
|---------|--------|
| `/instincts` | Show active instincts for current project |
| `/instincts add` | Manually add an instinct |
| `/instincts review` | Review and adjust confidence of existing instincts |
| `/instincts promote` | Promote high-confidence instincts to global learnings |
| `/instincts prune` | Remove low-confidence or stale instincts |

## Instinct Format

Each instinct is a single YAML entry:

```yaml
- id: inst-20260310-a1b2c3
  rule: "Use pnpm instead of npm for this project"
  confidence: 0.7
  scope: project          # project | domain | universal
  category: tooling       # tooling | style | api | testing | architecture | convention
  created: 2026-03-10
  last_reinforced: 2026-03-10
  reinforcement_count: 1
  source: "User corrected npm to pnpm"
  tags: [pnpm, npm, package-manager]
```

## Storage

### Project-Scoped Instincts

Stored in `.agents/instincts.yaml` at the project root:

```yaml
# .agents/instincts.yaml
# Auto-managed by /instincts skill. Edit manually if needed.
version: 1
instincts:
  - id: inst-20260310-a1b2c3
    rule: "Use pnpm instead of npm"
    confidence: 0.7
    scope: project
    category: tooling
    created: 2026-03-10
    last_reinforced: 2026-03-10
    reinforcement_count: 1
    source: "User corrected npm to pnpm"
    tags: [pnpm, npm]
```

### Universal Instincts (Global Knowledge Base)

When an instinct reaches high confidence (>=0.8) and universal scope,
promote it to the reflect-kb knowledge base via the `reflect` CLI:

```bash
# Promote instinct to a full learning note
if command -v reflect >/dev/null 2>&1; then
    reflect add docs/solutions/instincts/{instinct-id}.md \
        --entities docs/solutions/instincts/{instinct-id}.entities.yaml
elif [[ -x "$HOME/.local/bin/reflect" ]]; then
    "$HOME/.local/bin/reflect" add docs/solutions/instincts/{instinct-id}.md \
        --entities docs/solutions/instincts/{instinct-id}.entities.yaml
fi
```

## Confidence Scoring

### Scale

| Confidence | Meaning | Source |
|------------|---------|--------|
| 0.3 | Weak signal | Single observation, no explicit confirmation |
| 0.5 | Moderate | Confirmed once or observed multiple times |
| 0.7 | Strong | Explicitly stated by user or confirmed 3+ times |
| 0.9 | Certain | Repeatedly reinforced, never contradicted |

### Confidence Dynamics

```
Reinforcement: confidence = min(0.9, confidence + 0.1)
Contradiction: confidence = max(0.0, confidence - 0.3)
Decay: confidence = max(0.0, confidence - 0.05) per 30 days inactive
```

**Thresholds:**
- `< 0.3` — Auto-prune (too weak to keep)
- `0.3 - 0.5` — Low confidence, apply cautiously
- `0.5 - 0.7` — Moderate confidence, apply by default
- `>= 0.8` — High confidence, candidate for promotion to global

### Contradiction Handling

When a new observation contradicts an existing instinct:

1. **Reduce confidence** of existing instinct by 0.3
2. **Create competing instinct** with confidence 0.5
3. If existing drops below 0.3, auto-prune it
4. If both survive, flag for user review at next `/instincts review`

## Workflow

### Capturing Instincts (During Session)

Instincts are captured from:

1. **Explicit corrections**: "No, use tabs here" -> instinct about indentation
2. **Approved patterns**: User accepts a specific approach -> instinct about preference
3. **Repeated observations**: Same tool/command used 3+ times -> instinct about convention
4. **/reflect output**: Low-confidence signals from reflect that don't warrant full notes

### Step 1: Detect Signal

During conversation, watch for:
- Corrections to assumptions about project conventions
- Explicit preferences ("always", "never", "prefer", "use X not Y")
- Patterns that repeat across the session

### Step 2: Check for Existing Instinct

```bash
# Check if we already have an instinct for this topic
grep -i "{keyword}" .agents/instincts.yaml 2>/dev/null
```

- If match found: **reinforce** (bump confidence by 0.1)
- If contradicts: **handle contradiction** (see above)
- If new: **create** with initial confidence

### Step 3: Set Initial Confidence

| Signal Source | Initial Confidence |
|--------------|-------------------|
| Explicit user correction ("always do X") | 0.7 |
| User approval of approach | 0.5 |
| Repeated observation (3+ times) | 0.5 |
| Single observation | 0.3 |
| From /reflect low-confidence signal | 0.3 |

### Step 4: Write Instinct

Append to `.agents/instincts.yaml`. Create the file if it doesn't exist.

### Step 5: Apply Instincts (Session Start)

At session start, load active instincts:

1. Read `.agents/instincts.yaml`
2. Filter to confidence >= 0.3
3. Apply as soft rules (not hard requirements)
4. Higher confidence = stronger adherence

## Promotion to Global Learnings

When running `/instincts promote`:

1. Filter instincts with confidence >= 0.8 and scope = universal
2. For each candidate:
   a. Generate a learning note (using reflect learning_template.md format)
   b. Generate entity sidecar (.entities.yaml)
   c. Save to `docs/solutions/instincts/{id}.md`
   d. Index via `learnings add`
3. Mark instinct as `promoted: true` (don't delete — keep for local quick reference)

## Pruning

When running `/instincts prune`:

1. Remove instincts with confidence < 0.3
2. Decay instincts inactive for 30+ days
3. Show remaining instincts sorted by confidence
4. Allow user to manually remove any

## Integration

### With /reflect
- `/reflect` captures HIGH/MEDIUM signals as full learnings
- LOW signals route to instincts instead of being discarded
- This ensures nothing is lost, even weak signals

### With Session Start
- Load `.agents/instincts.yaml` at session start
- Apply as soft context for the session

### With .agents/MEMORY.md
- Instincts are complementary to MEMORY.md
- MEMORY.md: free-form project notes (architecture, decisions)
- Instincts: structured rules with confidence tracking
- Instincts that stabilize at high confidence can be "hardened" into MEMORY.md entries
