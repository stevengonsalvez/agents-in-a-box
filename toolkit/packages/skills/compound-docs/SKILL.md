---
name: compound-docs
description: |
  Capture solved problems as searchable knowledge. When a problem is fixed,
  this skill extracts the learning, structures it with YAML frontmatter,
  and saves it to docs/solutions/ for future reference.

  Philosophy: "First fix = 30 min research. Document it = 5 min.
  Next occurrence = 2 min lookup."

  Triggers: "that worked", "it's fixed", problem solved, or /compound command.
version: 1.0.0
author: Claude Code Toolkit
allowed-tools:
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Bash
---

# Compound-Docs - Knowledge Capture Skill

## Quick Reference

| Command | Action |
|---------|--------|
| `/compound` | Capture current solution as a learning |
| `/compound --global` | Also promote to global knowledge base |
| `/compound --category <cat>` | Specify category (auto-detected by default) |

## Core Philosophy

**"Each solved problem makes future work easier."**

When you solve a problem worth remembering:
1. Capture the solution while context is fresh
2. Structure it for searchability
3. Store it where future sessions will find it

## Workflow

### Step 1: Detect Trigger

This skill activates when:
- User says "that worked", "it's fixed", "problem solved"
- User explicitly runs `/compound`
- After a successful debugging session
- When an error is resolved

**Trigger Phrases:**
- "that worked"
- "it's fixed"
- "finally got it working"
- "the issue was"
- "the fix was"
- "problem solved"

### Step 2: Gather Context

Collect information about the problem and solution:

```markdown
## Context to Gather

1. **Problem Description**
   - What error message appeared?
   - What was the observed behavior?
   - What was expected?

2. **Root Cause**
   - What actually caused the issue?
   - Why did it happen?

3. **Solution**
   - What fixed it?
   - What's the key insight?
   - What are the steps?

4. **Files Involved**
   - Which files were modified?
   - Which files had the bug?

5. **Tags**
   - What technologies are involved?
   - What keywords would help find this?
```

### Step 3: Extract Entities & Relationships

Extract structured entities for GraphRAG indexing. These power graph-based
search in the global knowledge base.

**Entity Types:**

| Type | Description | Examples |
|------|-------------|----------|
| `technology` | Languages, frameworks, runtimes | tokio, react, postgresql |
| `error` | Error types, messages, exceptions | nested runtime panic, n+1 query |
| `pattern` | Design patterns, anti-patterns | eager loading, spawn_blocking |
| `function` | Specific functions, methods, APIs | block_on, prefetch_related |
| `concept` | Abstract concepts, principles | async context, connection pooling |
| `tool` | CLI tools, dev tools, services | cargo, webpack, docker |

**Relationship Types:**

| Type | Description | Example |
|------|-------------|---------|
| `caused_by` | What caused the error | block_on -> nested runtime panic |
| `solves` | What fixes the error | spawn_blocking -> nested runtime panic |
| `requires` | Prerequisites | spawn_blocking -> tokio runtime |
| `relates_to` | Related concepts | tokio -> async context |

**Extraction Guidelines:**
- Extract 3-8 entities per learning (focused, not exhaustive)
- Always include at least one `solves` relationship
- Strength: 9-10 direct/causal, 5-7 moderate, 1-4 weak
- Entity names normalized to lowercase canonical form
- Use the most specific entity type available

**Output Format (YAML block in your response):**

```yaml
entities:
  - name: "tokio"
    type: technology
    description: "Async runtime for Rust"
  - name: "nested runtime panic"
    type: error
    description: "Cannot start a runtime from within a runtime"
  - name: "spawn_blocking"
    type: function
    description: "Tokio function to run sync code within async context"
relationships:
  - source: "block_on"
    target: "nested runtime panic"
    type: caused_by
    description: "Calling block_on inside async context causes nested runtime panic"
    strength: 9
  - source: "spawn_blocking"
    target: "nested runtime panic"
    type: solves
    description: "Use spawn_blocking instead of block_on for sync code in async context"
    strength: 10
```

### Step 4: Determine Category

Auto-detect or use specified category:

| Category | Indicators |
|----------|------------|
| `build-errors` | Compile errors, CI failures, bundling issues |
| `performance-issues` | Slowdowns, memory leaks, optimization |
| `security-fixes` | Vulnerabilities, auth issues, secrets |
| `testing-patterns` | Test strategies, flaky tests |
| `debugging-sessions` | Complex investigations |
| `architecture-decisions` | Design choices, patterns |
| `api-integrations` | Third-party APIs, SDKs |
| `dependency-issues` | Package conflicts, upgrades |
| `deployment-fixes` | Production incidents |
| `database-migrations` | Schema changes, data fixes |
| `ui-patterns` | Frontend patterns, CSS |
| `tooling-setup` | Dev environment, configs |

### Step 5: Generate Learning Document

Create the YAML frontmatter and content:

```yaml
---
title: "[Brief descriptive title]"
category: [auto-detected or specified]
tags: [extracted tags]
symptoms:
  - "[Error message or behavior 1]"
  - "[Error message or behavior 2]"
root_cause: "[What actually caused it]"
key_insight: "[THE ONE THING that fixes it - most important field]"
related: []
created: [today's date]
confidence: [high|medium|low based on certainty]
language: [if applicable]
framework: [if applicable]
---

## Problem

[Detailed description of the problem]

## Solution

[Step-by-step solution with code examples]

## Context

[Additional context, why this happened, how to prevent]

## Related

[Links to related docs, external resources]
```

### Step 6: Save to docs/solutions/

```bash
# Ensure directory exists
mkdir -p docs/solutions/[category]

# Generate filename
FILENAME="[descriptive-name].md"

# Save file
# Location: docs/solutions/[category]/[filename].md
```

### Step 6.5: Generate Entity Sidecar (for --global)

When `--global` is specified, save the entities from Step 3 as a sidecar YAML file
alongside the learning document:

```bash
# Sidecar file: same name as doc, with .entities.yaml extension
# docs/solutions/build-errors/tokio-runtime-panic.entities.yaml
```

**Sidecar format:**

```yaml
document_id: tokio-runtime-panic-abc123
extracted_at: "2026-02-16T10:00:00"
entities:
  - name: "tokio"
    type: technology
    description: "Async runtime for Rust"
  # ... entities from Step 3
relationships:
  - source: "block_on"
    target: "nested runtime panic"
    type: caused_by
    description: "Calling block_on inside async context causes nested runtime panic"
    strength: 9
  # ... relationships from Step 3
```

The `document_id` is generated from the learning document filename, and `extracted_at`
is the current ISO timestamp.

### Step 7: Promote to Global Knowledge Base (Optional)

If the learning is universally useful (or `--global` was specified):

```markdown
This learning seems universally useful. Would you like to promote it to the global knowledge base?

Global learnings are accessible from any session, across all projects.

Options:
1. Save locally only (default)
2. Promote to global
```

If promoting:
```bash
# Check global learnings CLI exists
LEARNINGS_CLI="$HOME/.claude/global-learnings/cli/learnings"

if [[ -x "$LEARNINGS_CLI" ]]; then
    # Add document with pre-extracted entities
    "$LEARNINGS_CLI" add docs/solutions/[category]/[filename].md \
        --entities docs/solutions/[category]/[filename].entities.yaml
fi
```

The `--entities` flag passes the pre-extracted entity sidecar so the graph
engine can index entities without calling an external LLM.

## Output Format

After capturing, confirm with:

```markdown
## Learning Captured

**Title**: [title]
**Category**: [category]
**File**: docs/solutions/[category]/[filename].md

### Key Insight
> [key_insight]

### Tags
[tag1], [tag2], [tag3]

### What This Enables
Future research for similar problems will surface this solution automatically.

---
Run `/research [related keywords]` to verify it's searchable.
```

## Examples

### Example 1: Build Error

**Conversation:**
```
User: I keep getting "Cannot start a runtime from within a runtime"
[... debugging ...]
User: Oh! I needed to use spawn_blocking. That worked!
```

**Generated Learning:**
```yaml
---
title: "Tokio runtime panic on nested block_on"
category: build-errors
tags: [rust, tokio, async, runtime]
symptoms:
  - "Cannot start a runtime from within a runtime"
root_cause: "Calling block_on() inside an async context"
key_insight: "Use tokio::task::spawn_blocking for sync code in async context"
created: 2026-02-11
confidence: high
language: rust
framework: tokio
---
```

### Example 2: Performance Issue

**Conversation:**
```
User: The API is really slow, takes 5 seconds per request
[... investigation ...]
User: Found it! N+1 query. Adding eager loading fixed it.
```

**Generated Learning:**
```yaml
---
title: "N+1 query causing API slowdown"
category: performance-issues
tags: [sql, orm, n-plus-one, eager-loading]
symptoms:
  - "API response time > 5 seconds"
  - "Multiple queries per request in logs"
root_cause: "ORM lazy loading causing N+1 queries"
key_insight: "Use eager loading (includes/prefetch_related) for related records"
created: 2026-02-11
confidence: high
---
```

## Integration

### With /research

Learnings captured here are automatically searchable via:
```bash
/research [keywords]
```

The research command checks docs/solutions/ first.

### With /workflow

At workflow completion, /workflow suggests running /compound to capture learnings.

### With Global Knowledge

If global-learnings repo is configured:
```bash
learnings search "[query]" --mode local    # Graph-based search (related concepts)
learnings search "[query]" --mode naive    # Vector-only search (exact symptom matching)
learnings add [file] --entities [sidecar]  # Add with pre-extracted entities
```

## Checklist

Before saving a learning, verify:

- [ ] Title is descriptive (searchable)
- [ ] Key insight is THE ONE THING (not a summary)
- [ ] Symptoms include actual error messages
- [ ] Tags cover relevant technologies
- [ ] Category is correct
- [ ] Confidence reflects certainty

## Notes

- Focus on `key_insight` - this is what prevents repeating mistakes
- Include exact error messages in symptoms (for grep matching)
- Don't over-document - focus on what's needed to solve it again
- Update existing learnings rather than creating duplicates
