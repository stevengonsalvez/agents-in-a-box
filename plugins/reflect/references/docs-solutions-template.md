# docs/solutions/ - Knowledge Capture System

This directory captures institutional knowledge from solved problems, making future similar issues faster to resolve.

## Philosophy

> "Each unit of engineering work should make subsequent units of work easier---not harder."

First time solving a problem = research (30 min). Document it = 5 min. Next occurrence = 2 min lookup.

## Structure

```
docs/solutions/
├── README.md                 # This file
├── patterns/
│   └── critical-patterns.md  # Required reading for all agents
├── build-errors/             # Compilation, bundling, CI failures
├── performance-issues/       # Slowdowns, memory leaks, scaling
├── security-fixes/           # Vulnerabilities, auth issues
├── testing-patterns/         # Test strategies, flaky tests
├── debugging-sessions/       # Complex bug investigations
├── architecture-decisions/   # ADRs, design choices
├── api-integrations/         # Third-party API gotchas
├── dependency-issues/        # Package conflicts, upgrades
├── deployment-fixes/         # Production incidents, rollbacks
├── database-migrations/      # Schema changes, data fixes
├── ui-patterns/              # Frontend patterns, CSS fixes
└── tooling-setup/            # Dev environment, editor configs
```

## Document Format

Each learning document uses YAML frontmatter:

```yaml
---
title: "Brief descriptive title"
category: build-errors
tags: [rust, async, tokio]
symptoms: ["Cannot start a runtime from within a runtime"]
root_cause: "Calling block_on() inside an async context"
key_insight: "Use tokio::task::spawn_blocking or restructure to avoid nesting"
related: [./another-doc.md]
created: 2026-02-11
confidence: high
---

## Problem

[Detailed description of the problem]

## Solution

[Step-by-step solution]

## Context

[Additional context, code examples, links]
```

## Key Fields

| Field | Purpose | Required |
|-------|---------|----------|
| `title` | Brief description | Yes |
| `category` | Directory category (enum) | Yes |
| `tags` | Searchable keywords | Yes |
| `symptoms` | Error messages, behaviors | Yes |
| `root_cause` | What actually caused it | Yes |
| `key_insight` | **THE ONE THING** that fixes it | Yes |
| `related` | Links to related docs | No |
| `created` | Date created | Yes |
| `confidence` | high/medium/low | Yes |

## Categories

| Category | Use For |
|----------|---------|
| `build-errors` | Compilation, bundling, CI failures |
| `performance-issues` | Slowdowns, memory leaks, scaling problems |
| `security-fixes` | Vulnerabilities, auth issues, data exposure |
| `testing-patterns` | Test strategies, flaky test fixes |
| `debugging-sessions` | Complex bug investigations |
| `architecture-decisions` | Design choices, pattern selection |
| `api-integrations` | Third-party API gotchas, SDK issues |
| `dependency-issues` | Package conflicts, version upgrades |
| `deployment-fixes` | Production incidents, rollback procedures |
| `database-migrations` | Schema changes, data migration scripts |
| `ui-patterns` | Frontend patterns, CSS fixes, responsive design |
| `tooling-setup` | Dev environment, editor configs |

## Usage

### Adding a Learning

When you solve a problem worth documenting:

1. Create a file in the appropriate category: `docs/solutions/{category}/{descriptive-name}.md`
2. Fill in the YAML frontmatter (all required fields)
3. Add detailed problem description and solution
4. **ALWAYS generate the `.entities.yaml` sidecar** for GraphRAG indexing
5. Commit to the repo

Or use the `/reflect` command to have Claude capture it automatically.

### Searching Learnings

Learnings are automatically searched during `/research` and `/plan` commands.

Manual search:
```bash
# Search by keyword
rg "tokio" docs/solutions/

# Search by symptom
rg "Cannot start a runtime" docs/solutions/

# Search by tag
rg "tags:.*async" docs/solutions/

# Search global knowledge base
learnings search "tokio runtime panic"
```

### Promoting to Global

If a learning is universally useful (not repo-specific):

```bash
learnings promote docs/solutions/build-errors/my-fix.md
```

This adds it to the global knowledge base accessible from any session.

## Best Practices

1. **Be specific** - Include exact error messages in symptoms
2. **Focus on key_insight** - This is what prevents repeating mistakes
3. **Add code examples** - Show the fix, not just describe it
4. **Link related docs** - Help readers find connected knowledge
5. **Update when wrong** - Remove or correct outdated learnings
6. **Always generate sidecars** - Entity sidecars are required for GraphRAG indexing
