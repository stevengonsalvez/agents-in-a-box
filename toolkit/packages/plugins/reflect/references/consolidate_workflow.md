# Consolidate Workflow

Bulk-merge orphaned worktree memory directories, episode files, and unindexed
knowledge notes into a git-tracked `.agents/MEMORY.md` and the global GraphRAG
knowledge base.

**When to use:** After deleting git worktrees, or periodically to consolidate scattered learnings.

## Workflow

1. **Discover project identity**:
   ```bash
   python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py project-id
   # -> e.g. "shotclubhouse"
   ```

2. **Find orphaned memory dirs**:
   ```bash
   python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py discover
   # Discovers memories across all enabled providers (Claude, Codex, Gemini)
   # Excludes current session's memory dir
   ```

3. **Read all MEMORY.md files** from matched directories

4. **Deduplicate and categorize**: Group by section, remove redundant entries (fuzzy -- same concept = skip)

5. **Route skill-worthy content**: For entries that match existing skill topics (check `{{HOME_TOOL_DIR}}/skills/` and `.claude/skills/`), propose adding them to those skills (same approval flow as Step 5-6)

6. **Write `.agents/MEMORY.md`**: Consolidated, deduped, within 200-line limit. Create from template if it doesn't exist:
   ```markdown
   # Project Memory

   > Auto-consolidated by /reflect:consolidate. 200-line max. Detailed learnings route to skills.

   ## Architecture & Patterns

   ## Build & Deploy

   ## Gotchas & Workarounds

   ## Testing

   ## Environment & Config
   ```
   Sections are dynamic -- empty sections get removed. New sections are created as needed.

7. **Ensure agent config reference**: Check if the project agent config file (`.claude/AGENTS.md` or `.claude/CLAUDE.md`) contains `@.agents/MEMORY.md` -- if not, add it at the top

8. **Propose orphaned dir cleanup**: Show list and ask user to confirm deletion
   ```bash
   # After approval:
   python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py cleanup /tmp/reflect-cleanup-dirs.txt
   ```

9. **Report**: Show summary of what was consolidated, deleted, and routed to skills

## Stats

Check orphaned memory status without making changes:

```bash
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py stats
# -> Repo: shotclubhouse
# -> Orphaned memory dirs: 7
# -> Total lines across all: 283
```

## Extended Sources (Ingest-Memories)

Beyond worktree memories, consolidation also discovers:

| Source | Location | What to do |
|--------|----------|------------|
| Episode files | `~/.reflect/episodes/ep-*.md` | Extract learnings not yet indexed |
| Unindexed notes | `docs/solutions/**/*.md` without `.entities.yaml` | Generate sidecars, re-index |
| Stale project memories | `~/.claude/projects/*/memory/MEMORY.md` (>30 days) | Review and consolidate |

## Entity Sidecar Generation

When creating knowledge notes during consolidation, ALWAYS generate the
`.entities.yaml` sidecar alongside the `.md` file. See `knowledge_format.md`
for entity types, relationship types, and sidecar format.
