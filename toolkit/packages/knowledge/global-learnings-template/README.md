# Global Learnings - Cross-Project Knowledge Base

This repository stores learnings that are useful across multiple projects. It uses nano-graphrag for semantic search and graph-based retrieval with local embeddings (no API key required).

## Source-of-truth split

- **This toolkit** (ai-coder-rules) is the canonical source for the **CLI** (`cli/*.py`, `cli/learnings`). `bootstrap.js` deploys it to `~/.learnings/cli/` on every tool install and prunes any stale orphan files.
- **learnings-kb repo** is the canonical source for **content only** — `documents/`, entity sidecars, GraphRAG cache, QMD indexes. It no longer carries a CLI copy.

Fixes to the CLI land here; content changes land in learnings-kb. Never edit `~/.learnings/cli/` directly — it will be overwritten on next bootstrap.

### Migrating from old install paths

Pre-v3.1 installs placed the CLI at `~/.claude/global-learnings/cli/` (Claude-specific). That path is deprecated — the canonical install is now `~/.learnings/cli/` for all tools. If you have the old path, it's safe to delete:

```bash
rm -rf ~/.claude/global-learnings
```

New installs (via `bootstrap.js` for any tool) write only to `~/.learnings/`.

## Philosophy

> "Each solved problem makes future work easier - across ALL projects."

Local learnings stay in each project's `docs/solutions/`. Universal patterns and insights get promoted here for access from any session.

## Structure

```
global-learnings/
├── documents/               # Source learning documents (Markdown + YAML frontmatter)
│   ├── tokio-runtime-panic-abc123.md
│   ├── tokio-runtime-panic-abc123.entities.yaml  # Entity sidecar
│   └── ...
├── nano_graphrag_cache/     # GraphRAG index (auto-generated)
├── cli/
│   ├── learnings            # Bash wrapper (manages uv venv)
│   ├── learnings_cli.py     # Python CLI
│   ├── graph_engine.py      # nano-graphrag wrapper
│   ├── entity_store.py      # Entity/relationship sidecar management
│   ├── requirements.txt
│   └── setup.py
├── .venv/                   # uv-managed virtualenv (auto-created)
├── .gitignore
└── README.md
```

## Setup

```bash
# The CLI auto-initializes on first run via the bash wrapper.
# It uses uv to manage a virtualenv and install dependencies.

# First run:
~/.claude/global-learnings/cli/learnings --help
# → Creates .venv/, installs deps, shows help

# Or initialize explicitly:
~/.claude/global-learnings/cli/learnings init
```

Prerequisites: `uv` (install via `curl -LsSf https://astral.sh/uv/install.sh | sh` or `brew install uv`)

## Usage

### Search Learnings

```bash
# Vector-only search (fast, good for exact symptom matching)
learnings search "tokio runtime panic" --mode naive

# Graph-based search (finds related concepts via entity graph)
learnings search "async timeout" --mode local

# Community-based search (broad patterns across all learnings)
learnings search "error handling" --mode global

# JSON output (for integration with /research)
learnings search "n+1 query" --mode local --format json
```

### Add Learning

```bash
# Add a document to the knowledge base
learnings add ./my-solution.md

# Add with pre-extracted entities (from /compound --global)
learnings add ./my-solution.md --entities ./my-solution.entities.yaml
```

### Rebuild Index

```bash
# Incremental reindex (adds new docs to existing graph)
learnings reindex

# Full rebuild from scratch
learnings reindex --force
```

### Other Commands

```bash
# Show knowledge base statistics (docs, entities, relationships)
learnings stats

# Show high-confidence critical patterns
learnings critical-patterns
learnings critical-patterns --language rust --domain backend

# Initialize repository structure
learnings init
```

## Document Format

Learning documents use YAML frontmatter:

```yaml
---
title: "Brief descriptive title"
category: build-errors
tags: [rust, tokio, async]
symptoms:
  - "Cannot start a runtime from within a runtime"
root_cause: "Calling block_on() inside an async context"
key_insight: "Use tokio::task::spawn_blocking for sync code in async context"
created: 2026-02-16
confidence: high
language: rust
framework: tokio
---

## Problem
[Description]

## Solution
[Steps to fix]
```

## Entity Sidecar Format

Each document can have a `.entities.yaml` sidecar with pre-extracted entities:

```yaml
document_id: tokio-runtime-panic-abc123
extracted_at: "2026-02-16T10:00:00"
entities:
  - name: "tokio"
    type: technology
    description: "Async runtime for Rust"
  - name: "spawn_blocking"
    type: function
    description: "Tokio function to run sync code within async context"
relationships:
  - source: "block_on"
    target: "nested runtime panic"
    type: caused_by
    description: "Calling block_on inside async context causes nested runtime panic"
    strength: 9
```

Entity types: `technology`, `error`, `pattern`, `function`, `concept`, `tool`
Relationship types: `caused_by`, `solves`, `requires`, `relates_to`

## How It Works

1. **Documents** are stored as Markdown files with YAML frontmatter in `documents/`
2. **Entity sidecars** contain pre-extracted entities and relationships (from Claude during `/compound --global`)
3. **nano-graphrag** builds a knowledge graph from entities and embeds documents using `all-mpnet-base-v2` (768 dims, local CPU)
4. **Passthrough LLM** feeds pre-extracted entities directly to nano-graphrag, bypassing the need for an external LLM API
5. **Search** uses vector similarity (naive), entity graph traversal (local), or community reports (global)

## Integration

- `/research` searches global learnings first via `search-learnings.sh`
- `/compound --global` promotes local learnings with entity extraction
- `learnings` CLI provides direct interaction
- Graph is git-backed (documents are source of truth, cache is derived)
