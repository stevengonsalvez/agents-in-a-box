---
name: reflect:consolidate
description: |
  Project-level memory consolidation. Merges orphaned worktree memory
  directories into a single .agents/MEMORY.md for the current project.
  Deduplicates, sections, and proposes cleanup of orphan dirs.
  Does NOT index into the global knowledge base — use reflect:ingest for that.
version: "3.0.0"
user-invocable: true
triggers:
  - reflect:consolidate
  - consolidate memories
  - merge worktree memories
  - clean up memories
allowed-tools:
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Bash
---

# Reflect: Consolidate — Project Memory Cleanup

Merge orphaned worktree memory directories for the current project into a single,
tidy `.agents/MEMORY.md`. This is a **project-level** operation — it does not
touch the global knowledge base.

**For global indexing** (GraphRAG + QMD), use `/reflect:ingest` after consolidating.

## When to Use

- After deleting git worktrees
- When multiple worktree sessions left orphaned memory dirs
- To keep `.agents/MEMORY.md` current and deduped
- Before running `/reflect:ingest` (consolidate first, then index)

## What It Does

1. Discovers orphaned worktree memory dirs for the current project
2. Reads all MEMORY.md files from those dirs
3. Deduplicates entries (fuzzy — same concept = skip)
4. Routes skill-worthy content to existing skills
5. Writes consolidated `.agents/MEMORY.md` (200-line max)
6. Ensures agent config references it
7. Proposes cleanup of orphaned dirs

## What It Does NOT Do

- Does NOT index into GraphRAG or QMD (that's `reflect:ingest`)
- Does NOT scan other tools (Codex, Copilot, Gemini) — project-scoped only
- Does NOT archive originals to `~/.learnings/` — that's ingest's job

## Pipeline

### Step 1: Discover Project Identity

```bash
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py project-id
# -> e.g. "shotclubhouse"
```

### Step 2: Find Orphaned Memory Dirs

```bash
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py discover --provider claude
# Lists all worktree memory dirs for the current repo
# Excludes current session's memory dir
```

### Step 3: Read All MEMORY.md Files

Read from each matched directory. Extract individual entries (sections, bullets, paragraphs).

### Step 4: Deduplicate and Categorize

Group by section, remove redundant entries:
- Same concept = skip
- Same error + same solution = skip
- More detailed version = keep the detailed one

### Step 5: Route Skill-Worthy Content

Check if entries match existing skill topics (check `{{HOME_TOOL_DIR}}/skills/` and
`.claude/skills/`). Propose adding matching content to those skills.

### Step 6: Write `.agents/MEMORY.md`

Consolidated, deduped, within 200-line limit:

```markdown
# Project Memory

> Auto-consolidated by /reflect:consolidate. 200-line max.
> Detailed learnings route to skills. Global index via /reflect:ingest.

## Architecture & Patterns

## Build & Deploy

## Gotchas & Workarounds

## Testing

## Environment & Config
```

Sections are dynamic — empty sections removed, new sections created as needed.

### Step 7: Ensure Agent Config Reference

Check if `.claude/CLAUDE.md` or `.claude/AGENTS.md` contains `@.agents/MEMORY.md`.
If not, add it at the top.

### Step 8: Propose Orphan Cleanup

Show list of orphaned dirs and ask for confirmation:

```bash
# After user approval:
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py cleanup /tmp/reflect-cleanup-dirs.txt --execute
```

### Step 9: Report

```markdown
## Consolidation Complete

- **Orphaned dirs processed**: 7
- **Entries consolidated**: 42
- **Duplicates removed**: 15
- **Routed to skills**: 3
- **.agents/MEMORY.md**: 127 lines
- **Dirs cleaned up**: 7

Next: Run `/reflect:ingest` to index into GraphRAG + QMD.
```

## Stats (Quick Check)

Check orphaned memory status without making changes:

```bash
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py stats
# -> Repo: shotclubhouse
# -> Orphaned memory dirs: 7
# -> Total lines across all: 283
```

## Safety

- NEVER auto-apply without user approval
- NEVER delete orphaned dirs without explicit confirmation
- Always show what will be written to `.agents/MEMORY.md` before writing
- Preserve original structure of existing `.agents/MEMORY.md`
