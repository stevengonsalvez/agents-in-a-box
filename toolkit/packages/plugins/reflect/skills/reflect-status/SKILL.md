---
name: reflect-status
description: |
  Show reflection metrics, pending reviews, sidecar coverage, and GraphRAG health.
  Read-only views into the reflect system state. Can also approve/reject pending
  low-confidence items.
version: "3.0.0"
user-invocable: true
triggers:
  - reflect-status
  - reflect status
  - reflect review
  - reflection metrics
  - learning stats
allowed-tools:
  - Read
  - Grep
  - Glob
  - Bash
---

# Reflect: Status - Metrics & Review

Show the current state of the reflect system: metrics, pending reviews, sidecar
coverage, and GraphRAG health. Also handles review of pending low-confidence items.

## When to Use

- To check how many learnings have been captured
- To review pending low-confidence items
- To audit sidecar coverage (find knowledge notes missing sidecars)
- To check GraphRAG health (node/edge counts)
- To see which agents have been updated most

## Status View

When invoked, display the following sections:

### 1. Reflect State

```bash
python {{HOME_TOOL_DIR}}/skills/reflect/scripts/state_manager.py status
```

Shows:
- Auto-reflect: enabled/disabled
- Last reflection: timestamp
- Pending reviews: count

### 2. Aggregate Metrics

```bash
python {{HOME_TOOL_DIR}}/skills/reflect/scripts/metrics_updater.py --show
```

Shows:
- Sessions analyzed
- Total signals detected
- Changes proposed / accepted (acceptance rate)
- Skills created
- Estimated time saved
- Confidence breakdown (high/medium/low)
- Most updated agents (top 5)

### 3. Sidecar Coverage

Check how many knowledge notes have entity sidecars:

```bash
# Count knowledge notes
NOTES=$(find docs/solutions -name "*.md" -not -name "README.md" 2>/dev/null | wc -l | tr -d ' ')

# Count sidecars
SIDECARS=$(find docs/solutions -name "*.entities.yaml" 2>/dev/null | wc -l | tr -d ' ')

# Find notes missing sidecars
for md in $(find docs/solutions -name "*.md" -not -name "README.md" 2>/dev/null); do
    sidecar="${md%.md}.entities.yaml"
    if [[ ! -f "$sidecar" ]]; then
        echo "MISSING: $md"
    fi
done
```

Display:
```
Sidecar Coverage: 15/18 (83%)
Missing sidecars:
  - docs/solutions/build-errors/webpack-chunk-error.md
  - docs/solutions/testing-patterns/playwright-retry.md
  - docs/solutions/api-integrations/stripe-webhook.md
```

**If coverage < 100%**: Suggest running `/reflect:consolidate` to generate missing sidecars.

### 4. GraphRAG Health

```bash
if command -v reflect >/dev/null 2>&1; then
    reflect stats
elif [[ -x "$HOME/.local/bin/reflect" ]]; then
    "$HOME/.local/bin/reflect" stats
fi
```

Shows:
- Total indexed learnings
- Node count (entities)
- Edge count (relationships)
- Last indexing timestamp

### 5. Orphaned Memory Stats

```bash
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py stats
```

Shows:
- Repo name
- Orphaned memory dirs count
- Total lines across all orphaned dirs

**If orphaned dirs > 0**: Suggest running `/reflect:consolidate` to merge them.
**If unindexed memories found**: Suggest running `/reflect:ingest` to index into GraphRAG + QMD.

### 6. Project Memory Health

Check if `.agents/MEMORY.md` exists and its line count:

```bash
if [[ -f .agents/MEMORY.md ]]; then
    LINES=$(wc -l < .agents/MEMORY.md | tr -d ' ')
    echo "Project memory: .agents/MEMORY.md ($LINES/200 lines)"
else
    echo "Project memory: not found (run /reflect:consolidate to create)"
fi
```

## Review Mode

When invoked as `reflect review` or when there are pending items, enter review mode.

### Pending Low-Confidence Items

```bash
python {{HOME_TOOL_DIR}}/skills/reflect/scripts/state_manager.py pending
```

For each pending item, show:
- Signal text
- Detection date
- Source quote
- Category

**Review actions:**
- `approve N` -- Promote item N to full confidence, apply the change
- `reject N` -- Remove item N from pending queue
- `approve all` -- Promote all pending items
- `reject all` -- Clear all pending items
- `skip` -- Leave pending for later

When approving:
1. Apply the behavioral change or create the knowledge note
2. Generate entity sidecar if knowledge type
3. Index via `learnings add` CLI
4. Remove from pending queue:
   ```bash
   # Done internally via state_manager
   python -c "
   import sys; sys.path.insert(0, '{{HOME_TOOL_DIR}}/skills/reflect/scripts')
   from state_manager import clear_pending_review
   clear_pending_review(INDEX)
   "
   ```
5. Update metrics

### Staleness Check

Flag items that have been pending for more than 7 days:

```
STALE (14 days): "Consider using memoization for expensive renders"
  - Detected: 2026-03-30
  - Recommend: reject (too old, context lost) or approve if still relevant
```

## Output Format

Present everything in a clean dashboard format:

```markdown
# Reflect Status Dashboard

## System State
| Metric | Value |
|--------|-------|
| Auto-Reflect | Disabled |
| Last Reflection | 2026-04-12 14:30:00 |
| State Directory | ~/.reflect |

## Aggregate Metrics
| Metric | Value |
|--------|-------|
| Sessions Analyzed | 42 |
| Signals Detected | 156 |
| Changes Proposed | 114 |
| Changes Accepted | 89 (78%) |
| Skills Created | 5 |
| Estimated Time Saved | ~7.4 hours |

## Confidence Breakdown
| Level | Count |
|-------|-------|
| High | 45 |
| Medium | 32 |
| Low | 12 |

## Most Updated Agents
| Agent | Updates |
|-------|---------|
| code-reviewer | 23 |
| backend-developer | 18 |
| frontend-developer | 12 |
| security-agent | 8 |
| solution-architect | 5 |

## Sidecar Coverage
15/18 knowledge notes have sidecars (83%)
3 notes missing sidecars -- run /reflect:consolidate to fix

## GraphRAG Health
- Indexed learnings: 34
- Entities: 112
- Relationships: 87
- Last indexed: 2026-04-12

## Orphaned Memories
- Orphaned dirs: 3
- Total lines: 127
- Suggest: /reflect:consolidate

## Pending Reviews: 2
1. [MEDIUM] "Consider cursor-based pagination for large datasets" (5 days)
2. [LOW] "Memoize expensive component renders" (12 days, STALE)

Review pending items? (approve N / reject N / skip)
```

## Troubleshooting

**Metrics show 0:**
- Run `/reflect` at least once to initialize metrics
- Check state directory: `ls ~/.reflect/`

**GraphRAG stats unavailable:**
- Check if reflect-kb CLI is installed: `command -v reflect`
- Install if missing: `uv tool install --upgrade 'git+https://github.com/stevengonsalvez/reflect-kb.git[graph]'`

**Sidecar count is 0:**
- Knowledge notes may exist without sidecars (pre-v3 behavior)
- Run `/reflect:consolidate` to generate missing sidecars
