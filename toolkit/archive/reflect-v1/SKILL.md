---
name: reflect
description: |
  Unified learning capture from conversation analysis. Extracts behavioral
  corrections AND knowledge learnings (solved problems, patterns, decisions).
  Dual-indexes into QMD + GraphRAG for future retrieval.

  Philosophy: "Correct once, never again. Solve once, never re-research."

  Use when: (1) User corrects behavior, (2) Problem solved after debugging,
  (3) Session ending or context compaction, (4) User requests /reflect,
  (5) Pattern discovered worth preserving.
version: 3.0.0
author: Claude Code Toolkit
allowed-tools:
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Bash
argument-hint: [--behavioral|--knowledge|--review|--status|--consolidate|--ingest-memories]
---

# Reflect - Unified Learning Capture Skill

## Quick Reference

| Command | Action |
|---------|--------|
| `/reflect` | Full session analysis (behavioral + knowledge) |
| `/reflect --behavioral` | Only: corrections -> agent files |
| `/reflect --knowledge` | Only: solved problems -> knowledge docs |
| `/reflect --review` | Review pending low-confidence learnings |
| `/reflect --status` | Show metrics and KB stats |
| `/reflect --consolidate` | Merge orphaned worktree memories |
| `/reflect --ingest-memories` | Ingest project memory files into QMD + GraphRAG |
| `/reflect on` | Enable auto-reflection |
| `/reflect off` | Disable auto-reflection |
| `/reflect [agent]` | Focus on specific agent (behavioral only) |

If no flags and context is ambiguous, present interactive options:

```
What would you like to reflect on?
1. Full analysis (behavioral + knowledge)
2. Behavioral corrections only
3. Knowledge capture only
4. Review pending learnings
```

## Core Philosophy

**"Correct once, never again. Solve once, never re-research."**

Two complementary loops:
- **Behavioral loop**: When users correct behavior, those corrections become permanent improvements encoded into the agent system - across all future sessions.
- **Knowledge loop**: When problems are solved, the solution is captured, structured, and dual-indexed so any future session can retrieve it instantly.

## Sub-Skill Modules

These are internal modules (not user-visible commands):

| Module | Purpose |
|--------|---------|
| Signal Detection | Scan conversation for behavioral + knowledge signals |
| Entity Extraction | Extract entities + relationships for GraphRAG (from compound-docs logic) |
| Knowledge Doc Gen | Generate YAML+markdown learning notes |
| Agent Updates | Update agent config files with behavioral corrections |
| De-duplication | QMD similarity check against existing learnings |
| Indexing | `learnings` CLI: dual-index into QMD + GraphRAG |

## Workflow

### Step 1: SCAN Session for Signals

Check and initialize state files, then scan the conversation for both signal types:

```bash
# Check for existing state
python scripts/state_manager.py init

# State directory is configurable via REFLECT_STATE_DIR env var
# Default: ~/.reflect/ (portable) or $HOME/{{TOOL_DIR}}/session/ (Claude Code)
```

State includes:
- `reflect-state.yaml` - Toggle state, pending reviews
- `reflect-metrics.yaml` - Aggregate metrics
- `learnings.yaml` - Log of all applied learnings

#### Behavioral Signal Detection

Use the signal detector to identify corrections and preferences:

```bash
python scripts/signal_detector.py --input conversation.txt
```

##### Signal Confidence Levels

| Confidence | Triggers | Examples |
|------------|----------|----------|
| **HIGH** | Explicit corrections | "never", "always", "wrong", "stop", "the rule is" |
| **MEDIUM** | Approved approaches | "perfect", "exactly", accepted output |
| **LOW** | Observations | Patterns that worked, not validated |

See [signal_patterns.md](references/signal_patterns.md) for full detection rules.

#### Knowledge Signal Detection

Scan for solved problems, discovered patterns, decisions, and insights.
See [signal_patterns.md](references/signal_patterns.md) **Knowledge Signal Patterns** section for
the full detection reference (HIGH/MEDIUM/LOW confidence with regex patterns).

**HIGH confidence** — explicit resolution: root cause identified, fix confirmed, explicit decisions,
breakthrough moments ("turns out", "figured out", "the fix was").

**MEDIUM confidence** — implicit knowledge: debugging effort (time invested = value), docs-vs-reality
gaps, environment/config gotchas, performance insights, integration issues, failed approaches,
architecture insights.

**LOW confidence** — observations: implicit success ("seems to work"), library discoveries,
security findings needing validation.

**Structural signals** (non-phrase): error→investigation→fix arcs, config changes that resolved
issues, multiple approaches tried before settling on one.

### Step 2: CLASSIFY Each Signal

Classify each signal by type (behavioral vs knowledge), scope, and learning type.
See [classification_rules.md](references/classification_rules.md) for full tables covering:
- Signal type mapping (behavioral → agent file, knowledge → learning note)
- Behavioral classification → agent targets
- Knowledge learning types (pattern, correction, bug-fix, decision, anti-pattern)
- Scope auto-detection (project/domain/universal)
- Skill-worthy check criteria

**Route each signal to the correct output:**

| Signal characteristic | Route to | Why |
|-----------------------|----------|-----|
| Behavioral correction ("always do X", "never Y") | Agent file diff | Permanent rule change |
| Reusable fix, workaround, pattern, technique | Knowledge note | Needs semantic search for future discovery |
| Project-specific config, path, convention | `.agents/MEMORY.md` | Quick-reference, no search needed |
| Non-trivial debugging insight with error messages | Knowledge note | Others will search for those errors |
| Domain term, business rule | `.agents/MEMORY.md` | Project context |
| Recurring bug with generalizable fix | Knowledge note + possibly new skill | Broadest reuse |

**Knowledge Note criteria** (creates searchable docs/solutions/ + global index):
- Reusable beyond this specific project
- Non-obvious fix, workaround, or pattern
- Has identifiable entities (technologies, errors, functions)
- Someone searching for the error/symptom should find this

**Memory-only criteria** (.agents/MEMORY.md):
- Project-specific config, paths, conventions
- Quick-reference facts that don't need semantic search
- Low confidence signals awaiting validation

### Step 3: CHECK for Existing Learnings (De-duplication)

Before generating new content, check for duplicates:

```bash
# Query existing knowledge base for similar learnings
LEARNINGS_CLI="$LEARNINGS_HOME/cli/learnings"

if [[ -x "$LEARNINGS_CLI" ]]; then
    "$LEARNINGS_CLI" query --collection learnings --json "{extracted key insight}"
fi
```

**De-duplication Logic:**
- If a similar match is found (high similarity score): propose **UPDATE** to existing note instead of creating a new one
- If a partial match is found: propose **LINKING** the new note to the existing one via `links:` field
- If no match: proceed with new note creation

Also check project-local `docs/solutions/` for existing documents on the same topic:

```bash
# Check local solutions
grep -rl "{key terms}" docs/solutions/ 2>/dev/null
```

### Step 4: GENERATE Outputs

Produce proposals for both behavioral and knowledge signals.

#### Behavioral Proposals (Agent Updates)

For behavioral signals, generate proposed agent file changes:

```markdown
### Change N: Update [agent-name]

**Target**: `[file path]`
**Section**: [section name]
**Confidence**: [HIGH/MEDIUM/LOW]
**Rationale**: [why this change]

```diff
--- a/path/to/agent.md
+++ b/path/to/agent.md
@@ -82,6 +82,7 @@
 ## Section

 * Existing rule
+* New rule from learning
```
```

#### Knowledge Proposals (Learning Notes)

Generate LEARNING notes in Zettelkasten format using the template in
[assets/learning_template.md](assets/learning_template.md). Key fields: `type`, `id`,
`scope`, `confidence`, `learning_type`, `key_insight`, `symptoms`, `source_episodes`.

See [note_templates.md](references/note_templates.md) for full templates and format details.

#### Entity Sidecar (`.entities.yaml`)

Generate an entity sidecar for each learning for GraphRAG indexing.
See [knowledge_format.md](references/knowledge_format.md) for entity types, relationship
types, extraction guidelines, and sidecar format.

#### Episode Note (Auto-created, No Approval Needed)

Auto-create an episode note using [assets/episode_template.md](assets/episode_template.md).

**Write to `$LEARNINGS_HOME/documents/episodes/{YYYY-MM-DD}/ep-{YYYYMMDD}-{hash6}.md`**

Do NOT write to `{{TOOL_DIR}}/reflections/` — that was a dead-end location.
Episodes go directly to the global knowledge base where they are searchable.

The episode must include required frontmatter fields (`title`, `category: session-reflections`,
`key_insight`) so it passes `get_all_documents()` frontmatter validation.

Index immediately for search:
```bash
LEARNINGS_CLI="$LEARNINGS_HOME/cli/learnings"
if [[ -x "$LEARNINGS_CLI" ]]; then
    "$LEARNINGS_CLI" add "$LEARNINGS_HOME/documents/episodes/{date}/ep-{date}-{hash}.md"
fi
```

#### Project-Scoped Output

Project-scoped learnings are also saved to `docs/solutions/{category}/` in the project,
alongside their entity sidecars.

### Step 5: PRESENT to User for Approval

Show: signal table (with type, confidence, source quote), proposed diffs (behavioral),
proposed learning notes (knowledge), proposed new skills, and conflict check.

Use [assets/reflection_template.md](assets/reflection_template.md) for the output format.

**Review options**: `Y` (all), `N` (discard), `modify`, `1,3` (selective), `k2` (knowledge only),
`all-knowledge`, `all-behavioral`.

**NEVER auto-apply without user approval.**

### Step 6: INDEX (After Approval)

#### For Behavioral Changes (Agent Updates)

1. Apply each change using Edit tool
2. Run `git add` on modified files
3. Update learnings log

#### For Knowledge Changes (Learning Notes)

1. Save learning note to `docs/solutions/{category}/{filename}.md`
2. Save entity sidecar to `docs/solutions/{category}/{filename}.entities.yaml`
3. Index into global knowledge base:

```bash
# Add to knowledge base with dual indexing (QMD + GraphRAG)
LEARNINGS_CLI="$LEARNINGS_HOME/cli/learnings"

if [[ -x "$LEARNINGS_CLI" ]]; then
    "$LEARNINGS_CLI" add docs/solutions/{category}/{filename}.md \
        --entities docs/solutions/{category}/{filename}.entities.yaml
    # This triggers: GraphRAG insert + qmd embed + git commit
fi
```

4. Episode note: auto-created (no approval needed, raw provenance)

#### Consolidate to Project Memory

When approved learnings include **project-specific items** (not agent updates or new skills),
write them to `.agents/MEMORY.md` in the repo root:

1. Read `.agents/MEMORY.md` (create from template if it doesn't exist -- see Project Memory section)
2. Classify each approved learning into a category section
3. Deduplicate against existing lines (fuzzy match -- same concept = skip)
4. Append to the appropriate section
5. If file exceeds 200 lines: warn user, suggest routing verbose items to skills
6. Stage `.agents/MEMORY.md` alongside other reflect changes in the commit

**Decision flow -- Agent File vs `.agents/MEMORY.md`:**

| Signal | Target |
|--------|--------|
| Behavioral ("always do X") | Agent file |
| Project-specific architecture, gotcha, env quirk | `.agents/MEMORY.md` |
| Recurring bug with reusable fix | New skill |
| Domain term / business rule | `.agents/MEMORY.md` |

#### Commit

Commit all changes with a descriptive message:

```
reflect: add learnings from session [date]

Agent updates:
- [learning 1 summary]

Knowledge learnings:
- [learning 2 summary]

New skills:
- [skill-name]: [brief description]

Extracted: [N] signals ([H] high, [M] medium, [L] low confidence)
```

### Step 7: UPDATE Metrics

```bash
python scripts/metrics_updater.py --accepted 3 --rejected 1 --confidence high:2,medium:1
```

Also update `$LEARNINGS_HOME/metrics.yaml` with:
- Total signals detected (behavioral vs knowledge)
- Signals accepted vs rejected
- By type breakdown (pattern, correction, bug-fix, decision, anti-pattern)
- By scope breakdown (universal, domain, project)
- Knowledge base stats (total learnings, total entities, total episodes)

## Toggle Commands

### Enable Auto-Reflection

```bash
/reflect on
# Sets auto_reflect: true in state file
# Will trigger on PreCompact hook
```

### Disable Auto-Reflection

```bash
/reflect off
# Sets auto_reflect: false in state file
```

### Check Status

```bash
/reflect --status
# Shows: toggle state, metrics, KB stats
# KB stats: total learnings, entities, episodes, by scope/type
```

### Review Pending

```bash
/reflect --review
# Shows low-confidence learnings awaiting validation
# Allows promoting LOW -> MEDIUM/HIGH or discarding
```

## Project Memory (`/reflect --consolidate`)

Bulk-merge orphaned worktree memories into `.agents/MEMORY.md`.
See [consolidate_workflow.md](references/consolidate_workflow.md) for the full 9-step workflow.

## Ingest Project Memories (`/reflect --ingest-memories`)

Harvest Claude Code's auto-memory files from `{{HOME_TOOL_DIR}}/projects/*/memory/` into the
git-backed global knowledge base (`$LEARNINGS_HOME`) with QMD + GraphRAG indexing.

Memory files are ephemeral — Claude Code's dream cycles can prune them, and deleting a
project wipes its `{{HOME_TOOL_DIR}}/projects/<slug>/memory/` directory entirely. This command
archives the originals into `$LEARNINGS_HOME/documents/memories/{project}/` (git-tracked),
converts them to searchable knowledge learnings, and indexes into QMD + GraphRAG.

**When to use:** Periodically, or when you notice memory files accumulating across projects.
Run before deleting projects or cleaning up `{{HOME_TOOL_DIR}}/projects/`.

### Workflow

#### Step 1: DISCOVER all project memory files

```bash
MEMORY_BASE="$HOME/{{TOOL_DIR}}/projects"
echo "=== Scanning project memory directories ==="
TOTAL=0
for dir in "$MEMORY_BASE"/*/memory/; do
  [ -d "$dir" ] || continue
  count=$(find "$dir" -name "*.md" -not -name "MEMORY.md" -type f 2>/dev/null | wc -l)
  if [ "$count" -gt 0 ]; then
    project=$(basename "$(dirname "$dir")")
    echo "  $project: $count files"
    TOTAL=$((TOTAL + count))
  fi
done
echo "Total: $TOTAL memory files"
```

#### Step 2: READ and CLASSIFY each memory file

For each `.md` file (excluding `MEMORY.md` index files):

1. Read the file fully
2. Extract frontmatter fields: `name`, `description`, `type`
3. Classify by memory type → learning route:

| Memory Type | Learning Type | Scope | Route |
|-------------|---------------|-------|-------|
| `feedback` | `correction` or `pattern` | universal (if general) or project | Knowledge note |
| `user` | `preference` | universal | Knowledge note (user profile) |
| `project` | `decision` or `pattern` | project | Knowledge note |
| `reference` | `reference` | project or domain | Knowledge note |

4. Extract the project name from the directory path (decode the slug):
   ```
   -Users-stevengonsalvez-d-git-shotclubhouse → shotclubhouse
   ```

#### Step 3: DEDUP against existing knowledge base

For each memory file, check if it already exists in the knowledge base:

```bash
LEARNINGS_CLI="$LEARNINGS_HOME/cli/learnings"

# Check QMD first (fastest)
qmd query --collection learnings --json "{key_insight from memory}" 2>/dev/null

# Fall back to GraphRAG
if [[ -x "$LEARNINGS_CLI" ]]; then
    "$LEARNINGS_CLI" search "{key terms}" --mode local --format json
fi
```

**Skip** if high-similarity match exists. **Update** if partial match found and memory has newer info.

#### Step 4: CONVERT to knowledge learnings

For each non-duplicate memory, generate:

**A. Learning note** (using [assets/learning_template.md](assets/learning_template.md)):

- `id`: `lrn-mem-{project_slug}-{filename_slug}-{hash6}`
- `scope`: Derived from content (universal for user/feedback, project for project/reference)
- `confidence`: HIGH for feedback (user explicitly stated), MEDIUM for project/reference
- `learning_type`: Mapped from memory type (see table above)
- `key_insight`: The `description` field from frontmatter, or first substantive line
- `title`: The `name` field from frontmatter
- `tags`: Include project name, memory type, and content-derived tags
- `symptoms`: Extract "Why:" and "How to apply:" sections if present

Content sections:
- **Problem**: Context from the memory (what situation triggered this)
- **Solution**: The actionable guidance from the memory body
- **Context**: Project name, source path, original creation context

**B. Entity sidecar** (`.entities.yaml`):

Extract entities from the memory content:
- Tools, frameworks, libraries → `technology`
- People, roles → `concept`
- Projects, repos → `tool`
- Patterns, anti-patterns → `pattern`
- Errors, gotchas → `error`

Include relationships:
- feedback type: `solves` or `relates_to` relationships
- project type: `requires` or `relates_to` relationships
- reference type: `relates_to` relationships pointing to external resources

#### Step 5: PRESENT summary for approval

```markdown
## Memory Ingestion Plan

| # | Project | Memory File | Type | Action | Learning ID |
|---|---------|-------------|------|--------|-------------|
| 1 | shotclubhouse | feedback_tui_bulk_delete.md | feedback | NEW | lrn-mem-shot-tui-bulk-abc123 |
| 2 | github-io | user_stevie_tools.md | user | SKIP (exists) | — |
| 3 | github-io | feedback_no_emdashes.md | feedback | NEW | lrn-mem-ghio-emdash-def456 |
...

**New learnings**: N
**Skipped (duplicates)**: M
**Updated**: K

Proceed? [Y/n/select by number]
```

**NEVER auto-index without user approval.**

#### Step 6: ARCHIVE originals and INDEX approved learnings

For each approved memory file, first archive the original, then index the learning.

**A. Archive original memory files** (preserves raw content in git):

```bash
LEARNINGS_HOME="${LEARNINGS_HOME:-$HOME/.learnings}"

# Decode project slug: -Users-stevengonsalvez-d-git-shotclubhouse → shotclubhouse
# Take the last meaningful segment after -d-git- or last path component
decode_project() {
  local slug="$1"
  echo "$slug" | sed 's/.*-d-git-//' | sed 's/.*-d-//' | sed 's/^-//'
}

PROJECT_NAME=$(decode_project "$PROJECT_SLUG")
ARCHIVE_DIR="$LEARNINGS_HOME/documents/memories/$PROJECT_NAME"
mkdir -p "$ARCHIVE_DIR"

# Copy original memory file with metadata header prepended
for memory_file in $APPROVED_FILES; do
  BASENAME=$(basename "$memory_file")
  # Prepend archive metadata to the copy
  {
    echo "<!-- archived: $(date -Iseconds) -->"
    echo "<!-- source: $memory_file -->"
    echo ""
    cat "$memory_file"
  } > "$ARCHIVE_DIR/$BASENAME"
done
```

The `documents/memories/` directory structure:
```
$LEARNINGS_HOME/documents/memories/
├── shotclubhouse/
│   ├── feedback_tui_bulk_delete.md
│   ├── project_auth_migration.md
│   └── reference_supabase_config.md
├── stevengonsalvez-github-io/
│   ├── feedback_no_emdashes.md
│   ├── user_stevie_tools_and_opinions.md
│   └── project_blog_architecture.md
├── ai-coder-rules/
│   └── feedback_verify_before_claiming.md
└── _index.yaml          # tracks all ingested files
```

**B. Write structured learning notes** (for QMD + GraphRAG):

```bash
LEARNINGS_CLI="$LEARNINGS_HOME/cli/learnings"

# Derive category from memory type
# feedback → "preferences", user → "user-profile", project → "architecture", reference → "references"
CATEGORY="memories"
mkdir -p "$LEARNINGS_HOME/documents/learnings/$CATEGORY"

# Write {learning_id}.md to $LEARNINGS_HOME/documents/learnings/$CATEGORY/
# Write {learning_id}.entities.yaml alongside it
```

**C. Index into QMD + GraphRAG:**

```bash
if [[ -x "$LEARNINGS_CLI" ]]; then
    "$LEARNINGS_CLI" add "$LEARNINGS_HOME/documents/learnings/$CATEGORY/{learning_id}.md" \
        --entities "$LEARNINGS_HOME/documents/learnings/$CATEGORY/{learning_id}.entities.yaml"
fi
```

**D. Git commit the archive + learnings:**

```bash
cd "$LEARNINGS_HOME"
git add "documents/memories/" "documents/learnings/$CATEGORY/" ".memory-ingest-log.yaml"
git commit -m "reflect: ingest $COUNT memories from $PROJECT_COUNT projects

Archived: $COUNT original memory files
Indexed: $INDEXED learnings into QMD + GraphRAG
Projects: $(echo $PROJECTS | tr ' ' ', ')"
```

#### Step 7: MARK as ingested (prevent re-processing)

After successful indexing, create a tracking file so already-ingested memories aren't reprocessed:

```bash
INGEST_LOG="$LEARNINGS_HOME/.memory-ingest-log.yaml"
# Append entry:
# - file: {full path}
#   ingested_at: {ISO timestamp}
#   learning_id: {learning_id}
```

On subsequent runs, check this log before processing each memory file.

#### Step 8: REPORT

```markdown
## Memory Ingestion Complete

- **Scanned**: N projects, M memory files
- **Archived**: K files to $LEARNINGS_HOME/documents/memories/
- **Indexed**: K new learnings (QMD + GraphRAG)
- **Skipped**: J (already in KB or duplicate)
- **Updated**: L (merged newer info)
- **Git committed**: Yes (to $LEARNINGS_HOME local repo)

Archived originals:
- $LEARNINGS_HOME/documents/memories/{project}/*.md

Indexed learnings:
- QMD: K documents embedded
- GraphRAG: K documents + P entities + Q relationships
```

If `$LEARNINGS_HOME` has no git remote configured, warn:

```
⚠️  Your knowledge base ($LEARNINGS_HOME) has no git remote.
    Archives are git-tracked locally but NOT backed up.
    To add a remote: cd $LEARNINGS_HOME && git remote add origin <your-repo-url> && git push -u origin main
```

### Stale Memory Cleanup (Optional)

After ingestion, offer to flag memory files that have been successfully ingested:

```
The following memory files have been archived to $LEARNINGS_HOME
and indexed into QMD + GraphRAG. They are now searchable via /research
and will survive dream pruning or project deletion.

Would you like to keep the originals in {{HOME_TOOL_DIR}}/projects/? [Y/n]
```

**Default: keep originals** — Claude Code's auto-memory system still reads them for
context window injection. Only delete if the user explicitly requests cleanup.
Archived copies in `$LEARNINGS_HOME/documents/memories/` are the durable backup.

## Output Locations

| Scope | Location |
|-------|----------|
| Project memory | `.agents/MEMORY.md` (200 lines max) |
| Project knowledge | `docs/solutions/{category}/{name}.md` + `.entities.yaml` |
| Memory archive | `$LEARNINGS_HOME/documents/memories/{project}/` (git-tracked originals) |
| Global knowledge | `$LEARNINGS_HOME/documents/learnings/` (via `learnings add`) |
| Global episodes | `$LEARNINGS_HOME/documents/episodes/` (via `learnings add`) |
| Global metrics | `$LEARNINGS_HOME/metrics.yaml` |

## Memory Routing

| Signal | Target | Immediately searchable? |
|--------|--------|------------------------|
| Behavioral corrections | Agent file | N/A (loaded as rules) |
| Reusable knowledge (fix, pattern, technique) | Knowledge note → `docs/solutions/` + `learnings add` | YES |
| Session context & provenance | Episode → `$LEARNINGS_HOME/documents/episodes/` + `learnings add` | YES |
| Project-specific gotchas | `.agents/MEMORY.md` + knowledge note | Partial (memory: context window, note: full search) |
| Recurring bugs | New skill OR knowledge note | YES |
| LOW confidence + project-specific | `.agents/MEMORY.md` only | No (context window only) |

## Safety Guardrails

### Human-in-the-Loop
- NEVER apply changes without explicit user approval
- Always show full diff before applying
- Allow selective application
- Knowledge notes and entity sidecars shown in full before indexing

### Git Versioning
- All changes committed with descriptive messages
- Easy rollback via `git revert`
- Learning history preserved
- Episode notes provide full provenance trail

### Incremental Updates
- ONLY add to existing sections
- NEVER delete or rewrite existing rules
- Preserve original structure
- When de-duplication finds a match, propose UPDATE not REPLACE

### Conflict Detection
- Check if proposed rule contradicts existing
- Warn user if conflict detected
- Suggest resolution strategy
- De-duplication prevents knowledge base bloat

## Integration

### With /handover
If auto-reflection is enabled, PreCompact hook triggers reflection before handover.

### With Session Health
At 70%+ context (Yellow status), reminders to run `/reflect` are injected.

### With /research
Knowledge learnings captured here are automatically searchable via:
```bash
/research [keywords]
# Checks docs/solutions/ first, then $LEARNINGS_HOME via CLI
```

### With /workflow
At workflow completion, /workflow suggests running `/reflect` to capture learnings.

### Hook Integration (Claude Code)

PreCompact hook triggers auto-reflection. See [hooks/README.md](hooks/README.md) for setup.

## Portability

This skill works with any LLM tool that supports:
- File read/write operations
- Text pattern matching
- Git operations (optional, for commits)

### Configurable Paths

```bash
# Knowledge base home (default: ~/.learnings/)
export LEARNINGS_HOME=/path/to/learnings

# Reflect state directory
export REFLECT_STATE_DIR=/path/to/state
# Default: ~/.reflect/ (portable) or $HOME/{{TOOL_DIR}}/session/ (Claude Code)
```

### No Task Tool Dependency

Unlike the previous agent-based approach, this skill executes directly without spawning subagents. The LLM reads SKILL.md and follows the workflow.

### Git Operations Optional

Commits are wrapped with availability checks - if not in a git repo, changes are still saved but not committed.

## Troubleshooting

**No signals detected:**
- Session may not have had corrections or solved problems
- Try `/reflect --review` to check pending items
- Try `/reflect --knowledge` to specifically look for knowledge signals

**Conflict warning:**
- Review the existing rule cited
- Decide if new rule should override
- Can modify before applying

**Agent file not found:**
- Check agent name spelling
- Use `/reflect --status` to see available targets
- May need to create agent file first

**De-duplication false positive:**
- If a "duplicate" is actually a different learning, choose to create new
- Link to the similar learning via `links:` field

**Knowledge base CLI not found:**
- Check `$LEARNINGS_HOME/cli/learnings` exists and is executable
- Learning notes still saved locally to `docs/solutions/`
- Global indexing skipped gracefully

## File Structure

```
reflect/
├── SKILL.md                      # This file
├── scripts/
│   ├── state_manager.py          # State file CRUD
│   ├── signal_detector.py        # Pattern matching (behavioral + knowledge)
│   ├── metrics_updater.py        # Metrics aggregation
│   ├── output_generator.py       # Reflection file & index generation
│   └── memory_discovery.sh       # Project memory discovery & cleanup
├── hooks/
│   ├── precompact_reflect.py     # PreCompact hook integration
│   ├── settings-snippet.json     # Settings.json examples
│   └── README.md                 # Hook configuration guide
├── references/
│   ├── signal_patterns.md        # Detection rules (behavioral + knowledge)
│   ├── agent_mappings.md         # Target mappings for behavioral signals
│   ├── classification_rules.md   # Signal type, scope, learning type rules
│   ├── knowledge_format.md       # Entity types, relationships, sidecar format
│   ├── note_templates.md         # Learning, episode, and change templates
│   ├── consolidate_workflow.md   # /reflect --consolidate workflow
│   └── skill_template.md         # Skill generation guidelines
└── assets/
    ├── reflection_template.md    # Output template
    ├── learning_template.md      # Knowledge learning note template
    ├── episode_template.md       # Episode note template
    └── learnings_schema.yaml     # Schema definition
```
