---
name: reflect:consolidate
description: |
  Batch consolidation of orphaned learnings into GraphRAG. Discovers scattered
  worktree memories, project memory dirs, and episode files. Deduplicates,
  classifies, and indexes them with entity sidecars for searchable knowledge.
version: "3.0.0"
user-invocable: true
triggers:
  - reflect:consolidate
  - consolidate learnings
  - merge memories
  - ingest memories
allowed-tools:
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Bash
---

# Reflect: Consolidate - Batch Learning Ingestion

Bulk-merge orphaned and scattered learnings into the structured knowledge base.
Combines the consolidate (worktree memories) and ingest-memories (episode files,
project memory dirs) workflows into a single pipeline.

## When to Use

- After deleting git worktrees
- Periodically to consolidate scattered learnings
- When orphaned memory directories accumulate
- After multiple sessions create episode files that haven't been indexed
- When project memory dirs have useful content not yet in GraphRAG

## Sources

The consolidate skill discovers learnings from these sources:

| Source | Location | Discovery Method |
|--------|----------|-----------------|
| Orphaned worktree memories | Auto-detected across Claude, Codex, Gemini | `memory_discovery.py discover` |
| Project memory dirs | `.agents/MEMORY.md` in repo roots | Glob scan |
| Episode files | `~/.reflect/episodes/` | Glob for `ep-*.md` |
| Unindexed knowledge notes | `docs/solutions/` (any repo) | Check against `learnings stats` |
| Stale project memories | Auto-detected across all providers | Files older than staleness threshold |

## Pipeline

### Step 1: Discover Files

```bash
# Find orphaned worktree memories
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py discover --json

# Check stats
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py stats
```

Also scan for:
- Episode files in `~/.reflect/episodes/`
- Unindexed `docs/solutions/**/*.md` files (those without matching `.entities.yaml`)
- `.agents/MEMORY.md` in the current repo

### Step 2: Read All Discovered Files

Read every discovered file and extract individual learning entries.
Each entry is a distinct piece of knowledge (a section, a bullet point, or a paragraph).

### Step 3: Deduplicate Against Existing

Check each entry against the existing knowledge base:

```bash
LEARNINGS_CLI="$HOME/.claude/global-learnings/cli/learnings"
# Check existing learnings
"$LEARNINGS_CLI" stats
# Search for potential duplicates
"$LEARNINGS_CLI" search "keyword from entry"
```

**Dedup rules:**
- Same concept = skip (fuzzy match -- identical root cause or key insight)
- Same error message + same solution = skip
- Same topic but different angle = keep both, note relationship
- More detailed version of existing = update existing (supersede)

### Step 4: Classify Each Entry

For each unique entry, determine:

1. **Type**: behavioral (agent update) vs knowledge (learning note)
2. **Category**: Use category auto-detection from `references/classification_rules.md`
3. **Scope**: `universal`, `domain:{tech}`, or `project:{name}`
4. **Confidence**: Based on source quality
   - From explicit user corrections: HIGH
   - From solved bugs with confirmed fix: HIGH
   - From memory files: MEDIUM (context may be lost)
   - From episode files: MEDIUM
   - From unverified observations: LOW
5. **Skill routing**: Check if entry matches existing skill topics

### Step 5: Route Skill-Worthy Content

For entries that match existing skill topics (check `{{HOME_TOOL_DIR}}/skills/` and
`.claude/skills/`), propose adding them to those skills rather than creating
standalone notes.

### Step 6: Present for Approval

Show the user a summary table:

```markdown
## Consolidation Summary

| # | Source | Entry | Type | Category | Action |
|---|--------|-------|------|----------|--------|
| 1 | worktree-abc/MEMORY.md | "Always use RLS policies..." | behavioral | Security | Update security-agent |
| 2 | worktree-abc/MEMORY.md | "Fixed missing index on..." | knowledge | debugging-sessions | Create note + sidecar |
| 3 | episode ep-20260401-abc | "Chose cursor pagination..." | knowledge | architecture-decisions | Create note + sidecar |
| 4 | docs/solutions/build-errors/foo.md | (missing sidecar) | knowledge | build-errors | Generate sidecar only |

### Duplicates Skipped: 3
- "Use const over var" -- already in frontend-developer.md
- "Tokio spawn_blocking" -- already indexed as lrn-tokio-abc123
- "React hydration fix" -- already indexed

### Proposed Actions

**Agent updates**: 1
**Knowledge notes to create**: 2
**Sidecars to generate**: 1
**Orphaned dirs to clean up**: 5

Apply? (Y/N/modify/1,2,3)
```

### Step 7: Execute Approved Actions

For each approved entry:

**Knowledge notes:**
1. Create `docs/solutions/{category}/{filename}.md` with YAML frontmatter
2. **ALWAYS** generate `.entities.yaml` sidecar alongside the `.md` file
3. Index globally:
   ```bash
   LEARNINGS_CLI="$HOME/.claude/global-learnings/cli/learnings"
   "$LEARNINGS_CLI" add docs/solutions/{category}/{filename}.md \
       --entities docs/solutions/{category}/{filename}.entities.yaml
   ```

**Agent updates:**
1. Apply behavioral changes to target agent files using Edit tool
2. Follow agent mapping rules from `references/agent_mappings.md`

**Project memory consolidation:**
1. Write consolidated `.agents/MEMORY.md` (200-line max, deduped, sectioned)
2. Ensure agent config references it: check for `@.agents/MEMORY.md` in `.claude/CLAUDE.md`
3. Template for new `.agents/MEMORY.md`:
   ```markdown
   # Project Memory

   > Auto-consolidated by /reflect:consolidate. 200-line max. Detailed learnings route to skills.

   ## Architecture & Patterns

   ## Build & Deploy

   ## Gotchas & Workarounds

   ## Testing

   ## Environment & Config
   ```
   Sections are dynamic -- empty sections get removed, new sections created as needed.

**Missing sidecars:**
1. Read the existing `.md` knowledge note
2. Extract entities and relationships
3. Write the `.entities.yaml` sidecar
4. Re-index: `learnings add file.md --entities file.entities.yaml`

### Step 8: Cleanup Orphaned Dirs

After successful indexing, propose cleanup of orphaned directories:

```bash
# Show list and ask user to confirm
# After approval:
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py cleanup /tmp/reflect-cleanup-dirs.txt
```

### Step 9: Update Metrics

```bash
python {{HOME_TOOL_DIR}}/skills/reflect/scripts/metrics_updater.py \
    --accepted N --rejected M --confidence high:X,medium:Y,low:Z
```

### Step 10: Report

Show final summary:

```markdown
## Consolidation Complete

- **Entries processed**: 15
- **Knowledge notes created**: 8 (with sidecars)
- **Agent updates applied**: 3
- **Duplicates skipped**: 4
- **Orphaned dirs cleaned**: 5
- **Project memory updated**: .agents/MEMORY.md (127 lines)
```

## Entity Sidecar Generation

When creating knowledge notes during consolidation, ALWAYS generate entity sidecars.
See `references/knowledge_format.md` for entity types, relationship types, and format.

**Entity extraction guidelines:**
- Extract 3-8 entities per learning
- Always include at least one `solves` relationship for bug-fix type
- Strength: 9-10 direct/causal, 5-7 moderate, 1-4 weak
- Entity names normalized to lowercase canonical form

## Critical Patterns

See `references/critical-patterns.md` for patterns that should be checked
against during consolidation. If a discovered learning matches a critical
pattern, it may already be covered.

## Safety

- NEVER auto-apply without user approval
- NEVER delete orphaned dirs without explicit confirmation
- Always show what will be created/modified before doing it
- Preserve original files until indexing is confirmed successful
