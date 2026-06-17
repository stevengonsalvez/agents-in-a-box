---
name: reflect:ingest
description: |
  The global knowledge indexer. Harvests ALL memory sources across all tools
  (Claude, Codex, Copilot, Gemini) and all project types into the unified
  GraphRAG + QMD knowledge base. Archives originals, generates entity sidecars,
  and dual-indexes for future retrieval. This is THE command that makes the
  knowledge base comprehensive.
version: "3.0.0"
user-invocable: true
triggers:
  - reflect:ingest
  - ingest memories
  - index learnings
  - harvest memories
  - build knowledge base
allowed-tools:
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Bash
---

# Reflect: Ingest — Global Knowledge Indexer

The single command that makes the knowledge base comprehensive. Sweeps ALL memory
sources across all tools and projects, archives originals, generates entity
sidecars, and dual-indexes into GraphRAG + QMD.

**Philosophy**: Every piece of knowledge from every tool should be searchable in
one place. If it exists in a memory file somewhere, it should be in the global index.

## When to Use

- Periodically (weekly recommended) to keep the knowledge base current
- Before deleting projects or cleaning up `~/.claude/projects/`
- After heavy multi-project work across different tools
- When `reflect-status` shows low sidecar coverage or stale sources
- After running `reflect:consolidate` (to index what it produced)

## What Gets Indexed

`reflect:ingest` sweeps ALL of these sources:

| # | Source | Path | Provider |
|---|--------|------|----------|
| 1 | Claude auto-memories (individual files) | `~/.claude/projects/*/memory/*.md` | claude.py |
| 2 | Codex memories | `~/.codex/memories/*.md` | codex.py |
| 3 | Codex global AGENTS.md | `~/.codex/AGENTS.md` | codex.py |
| 4 | Copilot AGENTS.md | `~/.copilot/AGENTS.md` | copilot.py |
| 5 | Gemini global config | `~/.gemini/GEMINI.md` | gemini.py |
| 6 | Gemini project configs | `<project>/GEMINI.md` | gemini.py |
| 7 | Project AGENTS.md files | `<repo>/AGENTS.md` | codex.py / copilot.py |
| 8 | Consolidated project memory | `<repo>/.agents/MEMORY.md` | Glob scan |
| 9 | Unindexed knowledge notes | `docs/solutions/**/*.md` without sidecars | Glob scan |
| 10 | Episode files | `~/.reflect/episodes/ep-*.md` | Glob scan |
| 11 | Skill-embedded learnings | `~/.claude/skills/*/references/*.md` | Glob scan |

## Output Destinations

Everything flows to:

```
~/.learnings/                (KB content + index; managed by the `reflect` CLI)
├── documents/
│   ├── *.md / *.entities.yaml  Learning notes + entity sidecars (indexed
│   │                            into GraphRAG + QMD)
│   ├── memories/               Archived originals (by project)
│   │   ├── shotclubhouse/
│   │   ├── ai-coder-rules/
│   │   └── ...
│   └── episodes/               Session episode notes
├── nano_graphrag_cache/        GraphRAG index (nodes, edges, communities)
├── shards/                     R15: per-project shard KBs (each its own
│   ├── <project-a>/            documents/ + nano_graphrag_cache/). Recall
│   │   ├── documents/          defaults to the CURRENT project's shard for
│   │   ├── nano_graphrag_cache/ faster, cross-project-noise-free injection;
│   │   └── branches/           A6: per-branch / per-worktree sub-shards.
│   │       ├── feat__auth/     Worktrees of one repo (agents-in-a-box runs
│   │       │   ├── documents/  /.agents-in-a-box/worktrees/...) get SEPARATE
│   │       │   └── nano_graphrag_cache/ sub-shards so feat/auth and
│   │       └── feat__payment/  feat/payment don't pollute each other's recall.
│   └── <project-b>/            Recall defaults to the CURRENT branch's sub-
│                                shard; trunk (main/master) uses the project
│                                shard. `reflect:recall --all-branches` pools
│                                every branch of the project; `--global` pools
│                                every project at the top-level KB.
└── .memory-ingest-log.yaml     Tracks what's been ingested (prevents reprocessing)

# The `reflect` CLI itself is installed separately via:
#   uv tool install --upgrade 'git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]'
# and lands on $PATH at ~/.local/bin/reflect. The legacy ~/.learnings/cli/
# install path is deprecated; the `reflect` binary fully supersedes it.

~/.cache/qmd/
├── index.sqlite            QMD search index
└── (collections: learnings, obsidian, blog, writing)
```

## Pipeline

### Step 1: Discover ALL sources

```bash
# Use multi-tool provider discovery
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/memory_discovery.py discover --json

# Also scan non-provider sources
# Episode files:
find ~/.reflect/episodes -name "ep-*.md" 2>/dev/null

# Unindexed knowledge notes (missing sidecars):
find docs/solutions -name "*.md" -not -name "*.entities.yaml" 2>/dev/null | while read f; do
  [ ! -f "${f%.md}.entities.yaml" ] && echo "$f"
done

# .agents/MEMORY.md in current repo:
[ -f .agents/MEMORY.md ] && echo ".agents/MEMORY.md"
```

### Step 2: Read and Classify Each Memory File

For each discovered file:

1. Read the file fully
2. Extract frontmatter fields: `name`, `description`, `type`
3. Classify by memory type → learning route:

| Memory Type | Learning Type | Scope | Confidence |
|-------------|---------------|-------|------------|
| `feedback` | correction / pattern | universal (if general) or project | HIGH (user stated) |
| `user` | preference | universal | HIGH |
| `project` | decision / pattern | project | MEDIUM |
| `reference` | reference | project or domain | MEDIUM |
| Episode | session summary | project | MEDIUM |
| Knowledge note | (already structured) | from frontmatter | from frontmatter |
| AGENTS.md section | preference / process | universal | HIGH |

4. Extract project name from directory path:
   ```
   -Users-stevengonsalvez-d-git-shotclubhouse → shotclubhouse
   ```

### Step 3: Dedup Against Existing Knowledge Base

Check each entry against what's already indexed:

```bash
# Check QMD first (fastest)
qmd query --collection learnings --json "{key_insight}" 2>/dev/null

# Check GraphRAG via the reflect CLI (from reflect-kb — installed via
# `uv tool install reflect-kb`). Older versions of this skill referenced
# a legacy $HOME/.learnings/cli/learnings wrapper — that path is gone
# after the global-learnings skill purge; use `reflect` instead.
reflect search "{key terms}" 2>/dev/null

# Check ingest log (prevents reprocessing)
grep -q "{content_hash}" "$HOME/.learnings/.memory-ingest-log.yaml" 2>/dev/null
```

**Dedup rules:**
- Same `content_hash` in ingest log → SKIP (already processed)
- High-similarity match in QMD (>85%) → SKIP (duplicate)
- Same topic but different detail level → KEEP (more detail supersedes)
- Same error + same fix → SKIP
- Different angle on same topic → KEEP both, note relationship

### Step 4: Convert to Knowledge Learnings

For each non-duplicate memory, generate:

**A. Learning note** (`~/.learnings/documents/{id}.md` — flat top-level; the
CLI chooses this destination):

Use the template at `assets/learning_template.md` for the note structure.
Fields preserved: `id`, `scope`, `confidence`, `learning_type`, `source_episodes`,
`superseded_by` (for revisions), `provenance` (source_tool/path/hash), plus
Problem / Solution / Anti-Pattern / Context sections.


```yaml
---
title: "{descriptive title}"
category: "{auto-detected category}"
type: LEARNING
scope: "{universal|project|domain}"
confidence: {0.0-1.0}
key_insight: "{the one-line takeaway}"
tags: ["{project}", "{tech}", "{topic}"]
provenance:
  source_tool: "{claude|codex|copilot|gemini}"
  source_path: "{original file path}"
  content_hash: "{sha256 prefix}"
  ingested_at: "{ISO timestamp}"
---

## Problem
{context from the memory — what situation triggered this}

## Solution
{the actionable guidance from the memory body}

## Context
{project name, source path, related files}
```

**B. Entity sidecar** (`~/.learnings/documents/{id}.entities.yaml` — sibling
of the learning note):

Required schema — every field below is mandatory except `strength` (defaults to 5).
The `reflect add --entities` CLI enforces this via `entity_store.Entity` and will
raise `KeyError` if any required key is missing.

```yaml
document_id: lrn-{slug}-{hash6}
extracted_at: "{ISO-8601 timestamp}"
entities:
  - name: "{canonical lowercase name}"
    type: technology | error | pattern | function | concept | tool
    description: "{brief one-line description}"      # REQUIRED
relationships:
  - source: "{entity A name}"                        # NOT 'from'
    target: "{entity B name}"                        # NOT 'to'
    type: caused_by | causes | enables | prevents | contradicts | supersedes | part_of | uses | solves | requires | relates_to | implements | configures | triggers
    description: "{how they relate}"                 # REQUIRED
    strength: 1-10                                   # optional, default 5
```

Entity type hints by memory kind:
- Tools, frameworks, libraries → `technology`
- Patterns, anti-patterns → `pattern`
- Errors, gotchas → `error`
- Functions, APIs → `function`
- Concepts → `concept`

Relationship hints by memory kind:
- feedback: `solves` or `relates_to`
- project: `requires` or `relates_to`
- reference: `relates_to` to external resources
- when the direction of effect is known, prefer a typed causal link (S2) —
  `caused_by` / `causes` / `enables` / `prevents` / `contradicts` /
  `supersedes` / `part_of` / `uses` — over flat `relates_to`

Validate before writing:
```bash
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/validate_sidecar.py "$SIDECAR_PATH"
```

See `references/knowledge_format.md` for the full entity/relationship type reference.

### Step 5: Present Summary for Approval

```markdown
## Ingest Plan

### Sources Scanned
| Provider | Files Found | New | Duplicate | Stale |
|----------|------------|-----|-----------|-------|
| Claude   | 45         | 12  | 30        | 3     |
| Codex    | 8          | 3   | 5         | 0     |
| Copilot  | 2          | 0   | 2         | 0     |
| Gemini   | 4          | 2   | 2         | 0     |
| Episodes | 9          | 2   | 7         | 0     |
| docs/    | 3          | 3   | 0         | 0     |

### New Learnings to Index
| # | Source | Project | Title | Type | Category |
|---|--------|---------|-------|------|----------|
| 1 | claude | shotclubhouse | "RLS bypass with SECURITY DEFINER" | pattern | database |
| 2 | codex  | ai-coder-rules | "Codex memories dir structure" | reference | tooling |
| 3 | episode | — | "Sprint bug blitz session" | session | debugging |
...

**New learnings**: 22
**Skipped (duplicates)**: 46
**Sidecars to generate**: 3 (for existing unindexed notes)

Proceed? [Y/n/select by number]
```

**NEVER auto-index without user approval.**

### Step 6: Archive Originals and Index

For each approved entry:

**A. Archive original** (preserves raw content in git):

```bash
LEARNINGS_HOME="${LEARNINGS_HOME:-$HOME/.learnings}"
PROJECT_NAME="$(decode_project "$PROJECT_SLUG")"
ARCHIVE_DIR="$LEARNINGS_HOME/documents/memories/$PROJECT_NAME"
mkdir -p "$ARCHIVE_DIR"

# Copy with archive metadata
{
  echo "<!-- archived: $(date -Iseconds) -->"
  echo "<!-- source: $ORIGINAL_PATH -->"
  echo ""
  cat "$ORIGINAL_PATH"
} > "$ARCHIVE_DIR/$(basename "$ORIGINAL_PATH")"
```

**B. Write learning note + entity sidecar**

**C. Index into GraphRAG + QMD:**

```bash
# Use the canonical reflect CLI (from reflect-kb, installed via
# `uv tool install reflect-kb`). --force is REQUIRED here because this
# step runs under a non-interactive subprocess; without it, an existing
# doc with a colliding id would silently abort the add.
reflect add "$NOTE_PATH" --entities "$SIDECAR_PATH" --force
```

**D. Update QMD:**

```bash
qmd update --collection learnings 2>/dev/null
qmd embed 2>/dev/null
```

### Step 7: Mark as Ingested

Append to tracking file so already-ingested memories aren't reprocessed:

```bash
INGEST_LOG="$HOME/.learnings/.memory-ingest-log.yaml"
# Append entry:
# - file: {original path}
#   content_hash: {sha256}
#   ingested_at: {ISO timestamp}
#   learning_id: {generated id}
#   source_tool: {provider name}
```

### Step 8: Git Commit Archive + Learnings

```bash
cd "$LEARNINGS_HOME"
git add documents/memories/ documents/learnings/ .memory-ingest-log.yaml
git commit -m "reflect:ingest — $COUNT learnings from $PROVIDER_COUNT providers

Archived: $ARCHIVED original memory files
Indexed: $INDEXED into GraphRAG + QMD
Sources: $SOURCES"
```

### Step 9: Update Metrics

```bash
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/metrics_updater.py \
    --knowledge-notes $COUNT --sidecars $COUNT
```

Also rebuild the skills index (R20) so retrieval can match queries against
installed skills without a filesystem scan:

```bash
python3 {{HOME_TOOL_DIR}}/skills/reflect/scripts/skill_index.py rebuild
```

### Step 10: Report

```markdown
## Ingest Complete

- **Sources scanned**: 4 providers + episodes + docs
- **New learnings indexed**: 22 (with entity sidecars)
- **Duplicates skipped**: 46
- **Sidecars backfilled**: 3
- **Archives written**: 22 → ~/.learnings/documents/memories/
- **GraphRAG**: {node_count} nodes, {edge_count} edges
- **QMD**: {file_count} files, {vector_count} vectors
```

## Difference from reflect:consolidate

| | reflect:consolidate | reflect:ingest |
|---|---|---|
| **Scope** | Current project only | ALL projects, ALL tools |
| **Purpose** | Tidy worktree orphans → `.agents/MEMORY.md` | Build comprehensive global knowledge base |
| **Output** | `.agents/MEMORY.md` (project, git-tracked) | `~/.learnings/documents/` (global GraphRAG + QMD) |
| **Cleanup** | Deletes orphaned worktree dirs | Marks as ingested (preserves originals as archives) |
| **When** | After deleting worktrees | Periodically, or before project cleanup |

**Typical workflow**: Run `reflect:consolidate` first (tidies project), then `reflect:ingest` (indexes everything globally).

## Safety

- NEVER auto-index without user approval
- NEVER delete original memory files (archive them instead)
- Always check ingest log to prevent reprocessing
- Show full summary before any writes
- Preserve provenance metadata on every learning
