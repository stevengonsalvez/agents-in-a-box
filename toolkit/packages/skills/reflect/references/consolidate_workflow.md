# Consolidate Workflow (`/reflect --consolidate`)

Bulk-merge orphaned worktree memory directories into a git-tracked `.agents/MEMORY.md` in the repo root.

**When to use:** After deleting git worktrees, or periodically to consolidate scattered learnings.

## Workflow

1. **Discover project identity**:
   ```bash
   bash {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.sh project-id
   # -> e.g. "shotclubhouse"
   ```

2. **Find orphaned memory dirs**:
   ```bash
   bash {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.sh discover
   # Lists all ~/.claude/projects/*<repo-name>*/memory/MEMORY.md files
   # (excludes current session's memory dir)
   ```

3. **Read all MEMORY.md files** from matched directories

4. **Deduplicate and categorize**: Group by section, remove redundant entries (fuzzy — same concept = skip)

5. **Route skill-worthy content**: For entries that match existing skill topics (check `{{TOOL_DIR}}/skills/` and `{{HOME_TOOL_DIR}}/skills/`), propose adding them to those skills (same approval flow as Step 5-6)

6. **Write `.agents/MEMORY.md`**: Consolidated, deduped, within 200-line limit. Create from template if it doesn't exist:
   ```markdown
   # Project Memory

   > Auto-consolidated by /reflect. 200-line max. Detailed learnings route to skills.

   ## Architecture & Patterns

   ## Build & Deploy

   ## Gotchas & Workarounds

   ## Testing

   ## Environment & Config
   ```
   Sections are dynamic — empty sections get removed. New sections are created as needed.

7. **Ensure agent config reference**: Check if the project agent config file (`{{TOOL_DIR}}/AGENTS.md` or `{{TOOL_DIR}}/CLAUDE.md`) contains `@.agents/MEMORY.md` — if not, add it at the top

8. **Propose orphaned dir cleanup**: Show list and ask user to confirm deletion
   ```bash
   # After approval:
   bash {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.sh cleanup /tmp/reflect-cleanup-dirs.txt
   ```

9. **Report**: Show summary of what was consolidated, deleted, and routed to skills

## Stats

Check orphaned memory status without making changes:

```bash
bash {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.sh stats
# -> Repo: shotclubhouse
# -> Orphaned memory dirs: 7
# -> Total lines across all: 283
```
