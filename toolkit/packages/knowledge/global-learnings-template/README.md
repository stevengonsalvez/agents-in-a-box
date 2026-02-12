# Global Learnings - Cross-Project Knowledge Base

This repository stores learnings that are useful across multiple projects. It uses Nano-GraphRAG for semantic search and graph-based retrieval.

## Philosophy

> "Each solved problem makes future work easier - across ALL projects."

Local learnings stay in each project's `docs/solutions/`. Universal patterns and insights get promoted here for access from any session.

## Structure

```
global-learnings/
├── nano_graphrag_cache/     # GraphRAG index (Parquet files)
│   ├── entities.parquet
│   ├── relationships.parquet
│   ├── communities.parquet
│   └── embeddings.parquet
├── documents/               # Source learning documents
│   ├── build-001.yaml
│   ├── perf-002.yaml
│   └── ...
├── cli/
│   ├── learnings           # Python CLI tool
│   ├── requirements.txt
│   └── setup.py
└── README.md
```

## Setup

```bash
# Clone this repo
git clone <repo-url> ~/.claude/global-learnings

# Install CLI dependencies
cd ~/.claude/global-learnings/cli
pip install -e .

# Verify installation
learnings --help
```

## Usage

### Search Learnings

```bash
# Semantic search (vector + graph)
learnings search "tokio runtime panic"

# Search with filters
learnings search "async timeout" --tags rust,tokio

# Get critical patterns for a language
learnings critical-patterns --language rust
```

### Add Learning

```bash
# Add a new learning document
learnings add ./my-solution.md

# Add with automatic entity extraction (uses Claude session)
learnings add ./my-solution.md --extract
```

### Promote from Local

```bash
# Promote a local learning to global
learnings promote ~/project/docs/solutions/build-errors/my-fix.md
```

### Rebuild Index

```bash
# Rebuild the GraphRAG index
learnings reindex
```

## Document Format

Learning documents use YAML frontmatter:

```yaml
---
title: "Brief descriptive title"
category: build-errors
tags: [rust, tokio, async]
symptoms:
  - "Error message pattern"
root_cause: "What caused it"
key_insight: "THE ONE THING that fixes it"
created: 2026-02-11
confidence: high
---

## Problem
[Description]

## Solution
[Steps to fix]
```

## How It Works

1. **Documents** are stored as YAML/Markdown files in `documents/`
2. **Nano-GraphRAG** indexes them, extracting entities and relationships
3. **Embeddings** are generated using local sentence-transformers
4. **Search** uses vector similarity + graph traversal
5. **Claude session** is used for entity extraction (no separate API)

## Integration

This repo is accessed by:
- `/research` command (searches global learnings)
- `/compound --global` (promotes local learnings here)
- `learnings` CLI (direct interaction)

## Contributing

To add a learning:
1. Create a properly formatted document
2. Run `learnings add ./document.md`
3. Commit and push

Learnings are auto-approved (no review process).
