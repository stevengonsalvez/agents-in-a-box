---
name: global-learnings
description: |
  Manage the global learnings knowledge base - an agent-agnostic system for
  storing, searching, and retrieving cross-project learnings. Uses GraphRAG
  for vector + graph-based retrieval and QMD for fast local semantic search.
  Works with any AI coding agent (Claude, Cursor, Copilot, Codex, etc.).
user-invocable: false
---

# Global Learnings System

This skill provides the global learnings knowledge base infrastructure. It is
agent-agnostic -- knowledge is stored at `~/.learnings/` (configurable via
`LEARNINGS_HOME` env var) and accessible from any AI coding agent or CLI.

## Storage Location

Default: `~/.learnings/`

Override with the `LEARNINGS_HOME` environment variable:
```bash
export LEARNINGS_HOME="$HOME/.learnings"
```

### Directory Structure

```
~/.learnings/
├── documents/
│   ├── learnings/          # Individual learning documents
│   ├── episodes/           # Session episode summaries
│   └── clusters/           # Clustered pattern documents
├── nano_graphrag_cache/    # GraphRAG index (auto-generated)
├── cli/                    # CLI scripts (copied by setup)
├── .venv/                  # uv-managed virtualenv (auto-created)
└── .gitignore
```

## Search Backends

The system uses two complementary search backends:

- **GraphRAG** (nano-graphrag): Vector similarity + entity graph traversal + community reports. Best for deep semantic search and discovering connections between learnings.
- **QMD**: Fast local semantic search over document collections. Best for quick lookups and broad context retrieval.

Both backends are queried and results are merged for comprehensive coverage.

## CLI Usage (CLI-only, no MCP)

The learnings CLI is located at `~/.learnings/cli/learnings`.

### Common Commands

```bash
# Search learnings (GraphRAG)
learnings search "error message" --mode naive --format json

# Add a new learning
learnings add ./my-solution.md

# Add with pre-extracted entities
learnings add ./my-solution.md --entities ./my-solution.entities.yaml

# Get critical patterns
learnings critical-patterns --language rust --domain backend

# Rebuild the graph index
learnings reindex
learnings reindex --force

# Show statistics
learnings stats

# Initialize repository structure
learnings init

# Visualize the knowledge graph (interactive HTML)
learnings visualize
learnings visualize -o my-graph.html
learnings visualize --no-open
```

## Search Modes (GraphRAG)

- `naive`: Vector similarity only (fast, good for exact error messages)
- `local`: Entity neighborhood search (finds related concepts via graph)
- `global`: Community-based search (broad patterns across all learnings)

## Setup

See `references/setup.md` for installation and configuration guide.

## Scripts

- `scripts/setup-learnings-home.sh` - Migration to ~/.learnings/ and directory setup
- `scripts/setup-qmd.sh` - QMD installation and collection configuration
- `scripts/learnings` - CLI entry point (bash wrapper)
- `scripts/learnings_cli.py` - Main CLI implementation
- `scripts/graph_engine.py` - GraphRAG engine
- `scripts/entity_store.py` - Entity extraction and storage
- `scripts/graspologic_shim.py` - Pure networkx shim for graspologic
