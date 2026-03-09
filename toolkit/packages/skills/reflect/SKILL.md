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
# Default: ~/.reflect/ (portable) or {{HOME_TOOL_DIR}}/session/ (Claude Code)
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

Scan for solved problems, discovered patterns, and decisions (trigger phrases like
"fixed by", "root cause was", "that worked", "the issue was", "chose X over Y").

### Step 2: CLASSIFY Each Signal

Classify each signal by type (behavioral vs knowledge), scope, and learning type.
See [classification_rules.md](references/classification_rules.md) for full tables covering:
- Signal type mapping (behavioral → agent file, knowledge → learning note)
- Behavioral classification → agent targets
- Knowledge learning types (pattern, correction, bug-fix, decision, anti-pattern)
- Scope auto-detection (project/domain/universal)
- Skill-worthy check criteria

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
These are raw session snapshots providing provenance — no user approval needed.

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

## Output Locations

| Scope | Location |
|-------|----------|
| Project memory | `.agents/MEMORY.md` (200 lines max) |
| Project knowledge | `docs/solutions/{category}/{name}.md` + `.entities.yaml` |
| Project reflections | `{{TOOL_DIR}}/reflections/` |
| Global knowledge | `$LEARNINGS_HOME/documents/learnings/` |
| Global episodes | `$LEARNINGS_HOME/documents/episodes/` |
| Global metrics | `$LEARNINGS_HOME/metrics.yaml` |

## Memory Routing

- **Behavioral corrections** → Agent file
- **Solved problems** → Knowledge note (`docs/solutions/`) + global KB
- **Project-specific gotchas** → `.agents/MEMORY.md` + knowledge note
- **Recurring bugs** → New skill OR knowledge note
- **LOW confidence + project-specific** → prefer MEMORY.md over agent files

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
# Default: ~/.reflect/ (portable) or {{HOME_TOOL_DIR}}/session/ (Claude Code)
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
