# Compound

Capture the current solution as a searchable learning in `docs/solutions/`.

This command invokes the `compound-docs` skill to document a solved problem for future reference.

## Usage

```
/compound                     # Capture current solution
/compound --global            # Also promote to global knowledge base
/compound --category <cat>    # Specify category explicitly
```

## When to Use

- After fixing a bug
- After resolving an error
- After completing a debugging session
- When you say "that worked" or "problem solved"
- Any time you want to save knowledge for future sessions

## Process

Follow the compound-docs skill workflow:

1. **Gather Context**
   - What was the problem?
   - What error messages appeared?
   - What was the root cause?
   - What fixed it?

2. **Extract Key Insight**
   - THE ONE THING that solves the problem
   - Not a summary - the actionable fix

3. **Generate Learning Document**
   - YAML frontmatter with title, category, tags, symptoms
   - Markdown content with solution details

4. **Save to docs/solutions/**
   - Auto-detect category or use specified
   - Generate descriptive filename

5. **Optional: Promote to Global**
   - If universally useful, offer to promote

## Output

```markdown
## Learning Captured

**Title**: [title]
**Category**: [category]
**File**: docs/solutions/[category]/[filename].md

### Key Insight
> [the one thing that fixes it]

### Tags
[technology tags]

---
This learning will surface in future `/research` queries.
```

## Categories

| Category | Use For |
|----------|---------|
| `build-errors` | Compile, CI, bundling issues |
| `performance-issues` | Slowdowns, memory, optimization |
| `security-fixes` | Vulnerabilities, auth issues |
| `testing-patterns` | Test strategies, flaky tests |
| `debugging-sessions` | Complex investigations |
| `architecture-decisions` | Design choices, patterns |
| `api-integrations` | Third-party API issues |
| `dependency-issues` | Package conflicts |
| `deployment-fixes` | Production incidents |
| `database-migrations` | Schema changes |
| `ui-patterns` | Frontend patterns |
| `tooling-setup` | Dev environment |

## Examples

### After Fixing a Build Error

```
User: "The tokio runtime panic is fixed!"
/compound

→ Captures the solution to docs/solutions/build-errors/tokio-runtime-panic.md
```

### With Explicit Category

```
/compound --category performance-issues

→ Saves to docs/solutions/performance-issues/
```

## Global Promotion Flow

When `--global` is specified, after saving locally:

1. **Entity sidecar saved alongside document** (Step 6.5 in compound-docs skill)
   - File: `docs/solutions/[category]/[filename].entities.yaml`
   - Contains pre-extracted entities and relationships from the learning

2. **Check global learnings CLI exists**
   ```bash
   LEARNINGS_CLI="$HOME/.claude/global-learnings/cli/learnings"
   if [[ ! -x "$LEARNINGS_CLI" ]]; then
       echo "Global learnings CLI not found. Run: learnings init"
       # Fall back to local-only save
   fi
   ```

3. **Add to global knowledge base with entities**
   ```bash
   "$LEARNINGS_CLI" add docs/solutions/[category]/[filename].md \
       --entities docs/solutions/[category]/[filename].entities.yaml
   ```

4. **Show confirmation with entity/relationship counts**
   ```markdown
   ## Promoted to Global Knowledge Base

   **Entities**: 5 extracted (tokio, spawn_blocking, ...)
   **Relationships**: 3 mapped (caused_by, solves, requires)
   **Graph**: Updated in ~/.claude/global-learnings/

   This learning will surface in `/research` queries across all projects.
   ```

## Integration

- Learnings are searchable via `/research`
- `/workflow` suggests `/compound` on completion
- Global learnings accessible via `learnings` CLI
- Global search uses GraphRAG: `learnings search "[query]" --mode local`
