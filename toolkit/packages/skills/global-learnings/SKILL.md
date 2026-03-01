---
name: global-learnings
description: |
  Manage the global learnings knowledge base - a GraphRAG-powered system for
  storing, searching, and retrieving cross-project learnings. Supports vector
  search, entity graph traversal, and critical pattern management.
user-invocable: false
---

# Global Learnings System

This skill provides the global learnings knowledge base infrastructure.

## CLI Usage

The learnings CLI is located at `~/.claude/global-learnings/cli/learnings`.

### Common Commands

```bash
# Search learnings
learnings search "error message" --mode naive --format json

# Add a new learning
learnings add --title "Fix for X" --category build-errors

# Get critical patterns
learnings critical-patterns --language rust --domain backend

# Rebuild the graph index
learnings rebuild-graph
```

## Search Modes

- `naive`: Vector similarity only (fast, good for exact error messages)
- `local`: Entity neighborhood search (finds related concepts via graph)
- `global`: Community-based search (broad patterns across all learnings)

## Setup

See `references/setup.md` for installation and configuration guide.

## Scripts

- `scripts/learnings` - CLI entry point
- `scripts/learnings_cli.py` - Main CLI implementation
- `scripts/graph_engine.py` - GraphRAG engine
- `scripts/entity_store.py` - Entity extraction and storage
- `scripts/graspologic_shim.py` - Pure networkx shim for graspologic
