---
name: compound-docs
description: |
  Internal module for knowledge capture. Called by /reflect for generating
  structured learning documents with YAML frontmatter and entity sidecars.
  Not user-invocable — use /reflect or /reflect --knowledge instead.
version: 1.1.0
author: Claude Code Toolkit
user-invocable: false
allowed-tools:
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Bash
---

> **Internal Module**: Called by `/reflect` for knowledge capture.
> Use `/reflect` directly, or `/reflect --knowledge` for knowledge-only capture.

# Compound-Docs — Knowledge Note Generator

## Purpose

Generate structured learning documents from solved problems. This module handles:
1. Context gathering (problem, root cause, solution, files, tags)
2. Category auto-detection
3. Learning document generation (YAML frontmatter + markdown)
4. Entity sidecar generation for GraphRAG indexing
5. Saving to `docs/solutions/` and promoting to global KB

## Context to Gather

1. **Problem**: Error message, observed vs expected behavior
2. **Root Cause**: What actually caused it, why
3. **Solution**: What fixed it, key insight, steps
4. **Files**: Which files were modified or had the bug
5. **Tags**: Technologies involved, searchable keywords

## Category Auto-Detection

| Category | Indicators |
|----------|------------|
| `build-errors` | Compile errors, CI failures, bundling |
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

## Learning Document Format

```yaml
---
title: "[Brief descriptive title]"
category: [auto-detected or specified]
tags: [extracted tags]
symptoms:
  - "[Error message or behavior]"
root_cause: "[What actually caused it]"
key_insight: "[THE ONE THING that fixes it]"
created: [today's date]
confidence: [high|medium|low]
language: [if applicable]
framework: [if applicable]
---

## Problem
[Description]

## Solution
[Steps with code examples]

## Context
[Why it happened, how to prevent]
```

## Entity Extraction

For GraphRAG indexing, extract entities and relationships.
See `reflect/references/knowledge_format.md` for entity types,
relationship types, extraction guidelines, and sidecar format.

## Saving

```bash
# Project-local
mkdir -p docs/solutions/[category]
# Save: docs/solutions/[category]/[filename].md

# Global promotion (via the reflect CLI from reflect-kb).
# `--force` skips the interactive y/N prompt; content-hash doc_id makes the
# call idempotent so re-runs no-op cleanly.
if command -v reflect >/dev/null 2>&1; then
    reflect add docs/solutions/[category]/[filename].md \
        --entities docs/solutions/[category]/[filename].entities.yaml \
        --force
fi
```

## Quality Checklist

- [ ] Title is descriptive (searchable)
- [ ] Key insight is THE ONE THING (not a summary)
- [ ] Symptoms include actual error messages
- [ ] Tags cover relevant technologies
- [ ] Category is correct
- [ ] Confidence reflects certainty
