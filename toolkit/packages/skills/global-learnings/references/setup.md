# Global Learnings - Cross-Project Knowledge Base

An agent-agnostic knowledge base for storing and searching cross-project learnings. Uses GraphRAG (nano-graphrag) and QMD for dual semantic search with local embeddings (no API key required). Works with any AI coding agent.

## Philosophy

> "Each solved problem makes future work easier - across ALL projects."

Local learnings stay in each project's `docs/solutions/`. Universal patterns and insights get promoted here for access from any session, any agent.

## Storage Location

Default: `~/.learnings/`

Override with the `LEARNINGS_HOME` environment variable:
```bash
export LEARNINGS_HOME="$HOME/.learnings"
```

## Structure

```
~/.learnings/
├── documents/
│   ├── learnings/              # Individual learning documents
│   │   ├── tokio-runtime-panic-abc123.md
│   │   ├── tokio-runtime-panic-abc123.entities.yaml
│   │   └── ...
│   ├── episodes/               # Session episode summaries
│   └── clusters/               # Clustered pattern documents
├── nano_graphrag_cache/        # GraphRAG index (auto-generated)
├── cli/
│   ├── learnings               # Bash wrapper (manages uv venv)
│   ├── learnings_cli.py        # Python CLI
│   ├── graph_engine.py         # nano-graphrag wrapper
│   ├── entity_store.py         # Entity/relationship sidecar management
│   ├── requirements.txt
│   └── setup.py
├── .venv/                      # uv-managed virtualenv (auto-created)
├── .gitignore
└── README.md
```

## Setup

### 1. Migration (from ~/.claude/global-learnings/)

If you have an existing installation at `~/.claude/global-learnings/`, run the migration script:

```bash
# From the toolkit source
./toolkit/packages/skills/global-learnings/scripts/setup-learnings-home.sh
```

This script:
- Creates `~/.learnings/` directory structure
- Migrates existing documents to `documents/learnings/`
- Creates a backward-compat symlink (`~/.claude/global-learnings -> ~/.learnings`)
- Copies CLI scripts to `~/.learnings/cli/`
- Adds `LEARNINGS_HOME` export to your shell profile
- Initializes a git repository
- Is idempotent (safe to run multiple times)

### 2. Fresh Install

For a fresh install, the migration script works the same way -- it creates the structure without migrating:

```bash
./toolkit/packages/skills/global-learnings/scripts/setup-learnings-home.sh

# Or just run the CLI (auto-initializes on first run):
~/.learnings/cli/learnings --help
```

### 3. QMD Setup (Dual Search Backend)

Install and configure QMD for fast local semantic search alongside GraphRAG:

```bash
./toolkit/packages/skills/global-learnings/scripts/setup-qmd.sh
```

This script:
- Installs `qmd` CLI via npm (if not already installed)
- On macOS, ensures brew sqlite for extension support
- Adds the learnings collection to QMD
- Sets context hierarchy for the collection
- Runs initial embedding
- Is idempotent

### Prerequisites

- `uv` (install via `curl -LsSf https://astral.sh/uv/install.sh | sh` or `brew install uv`)
- `npm` (for QMD installation)

## Multi-Agent Usage

The knowledge base is agent-agnostic. Configure any AI coding agent to use it:

### Claude Code
```bash
# Already configured via skills system
# CLI at: ~/.learnings/cli/learnings
```

### Cursor
```json
// .cursor/settings.json
{
  "ai.customInstructions": "Search learnings before solving: ~/.learnings/cli/learnings search \"<query>\" --format json"
}
```

### GitHub Copilot
```markdown
<!-- .github/copilot-instructions.md -->
Before solving errors, search the knowledge base:
~/.learnings/cli/learnings search "<error>" --mode naive --format json
```

### OpenAI Codex
```bash
# In codex instructions
export LEARNINGS_HOME="$HOME/.learnings"
# CLI: $LEARNINGS_HOME/cli/learnings search "<query>" --format json
```

## Usage

### Search Learnings

```bash
# Vector-only search (fast, good for exact symptom matching)
learnings search "tokio runtime panic" --mode naive

# Graph-based search (finds related concepts via entity graph)
learnings search "async timeout" --mode local

# Community-based search (broad patterns across all learnings)
learnings search "error handling" --mode global

# JSON output (for integration with other tools)
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

## QMD Collection Configuration

After setup, the QMD collection is configured to search across all document subdirectories:

```bash
# List collections
qmd collection list

# Search via QMD directly
qmd search "tokio runtime panic" --collection learnings

# Re-embed after adding documents (automatic via CLI)
qmd embed
```

The context hierarchy (`clusters > episodes > learnings`) ensures that higher-level pattern documents provide broader context while individual learnings provide specific details.

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

1. **Documents** are stored as Markdown files with YAML frontmatter in `documents/` subdirectories
2. **Entity sidecars** contain pre-extracted entities and relationships
3. **GraphRAG** (nano-graphrag) builds a knowledge graph from entities and embeds documents using `all-mpnet-base-v2` (768 dims, local CPU)
4. **QMD** provides fast local semantic search as a complementary backend
5. **Passthrough LLM** feeds pre-extracted entities directly to nano-graphrag, bypassing the need for an external LLM API
6. **Search** uses vector similarity (naive), entity graph traversal (local), or community reports (global) via GraphRAG, plus QMD for broad coverage

## Integration

- `/research` searches global learnings via GraphRAG + QMD
- `/reflect` captures and promotes learnings with entity extraction
- `learnings` CLI provides direct interaction
- Git-backed (documents are source of truth, cache is derived)
- Agent-agnostic (works with any AI coding agent via `LEARNINGS_HOME`)
