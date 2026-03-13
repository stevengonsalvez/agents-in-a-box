# Knowledge System: /reflect & /research

> "Correct once, never again. Solve once, never re-research."

This document explains how the toolkit's knowledge capture (`/reflect`) and retrieval (`/research`) system works, including the two-tier storage architecture, GraphRAG, QMD, and how everything connects.

---

## System Overview

```
                         ┌─────────────────────┐
                         │   Agent Session      │
                         │  (conversation)      │
                         └─────────┬───────────┘
                                   │
                    ┌──────────────┴──────────────┐
                    │                             │
                    ▼                             ▼
            ┌──────────────┐             ┌──────────────┐
            │   /reflect   │             │  /research   │
            │   (capture)  │             │  (retrieve)  │
            └──────┬───────┘             └──────┬───────┘
                   │                            │
                   ▼                            ▼
        ┌─────────────────────────────────────────────┐
        │           Two-Tier Knowledge Store           │
        │                                             │
        │  ┌──────────────┐    ┌───────────────────┐  │
        │  │  HOT TIER    │    │    COLD TIER       │  │
        │  │  (project)   │    │    (global)        │  │
        │  │              │    │                    │  │
        │  │ docs/        │    │ ~/.learnings/      │  │
        │  │ solutions/   │    │ documents/         │  │
        │  │              │    │                    │  │
        │  │ Text search  │    │ QMD + GraphRAG     │  │
        │  └──────────────┘    └───────────────────┘  │
        └─────────────────────────────────────────────┘
```

---

## /reflect — Knowledge Capture

`/reflect` analyses your conversation to extract two types of signals:

- **Behavioral** — corrections to agent behaviour ("never do X", "always use Y")
- **Knowledge** — solved problems, patterns, debugging insights

### Capture Pipeline

```
  Conversation
       │
       ▼
┌──────────────────┐
│  Signal Detection │   Scans for linguistic patterns:
│                   │     HIGH:   "never", "always", "must", "don't"
│                   │     MEDIUM: "perfect", "exactly", "that's right"
│                   │     LOW:    "maybe", "consider", "perhaps"
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Classification   │   Routes each signal:
│  & Routing        │
└──┬─────┬─────┬──┘
   │     │     │
   │     │     └──► Project gotcha → .agents/MEMORY.md
   │     │
   │     └────────► Knowledge (fix, pattern, decision)
   │                  → Learning note (.md)
   │                  → Entity sidecar (.entities.yaml)
   │                  → Episode snapshot
   │
   └──────────────► Behavioral (correction, preference)
                      → Agent file diff
         │
         ▼
┌──────────────────┐
│  De-duplication   │   Checks QMD for similar existing learnings
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  User Approval    │   Shows diffs. NEVER auto-applies.
│  (human-in-loop)  │   Selective: y/n/modify/1,3/all-knowledge
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Dual Index       │   Writes to BOTH tiers:
│                   │     Hot:  docs/solutions/{category}/
│                   │     Cold: ~/.learnings/documents/
│                   │           → QMD embed
│                   │           → GraphRAG insert
└──────────────────┘
```

### Output Routing

| Signal Type | Target | Searchable? |
|-------------|--------|-------------|
| Behavioral correction | Agent config files | N/A (loaded as rules) |
| Reusable knowledge | `docs/solutions/` + global learnings | Yes (QMD + GraphRAG) |
| Session provenance | Episode note | Yes |
| Project gotcha | `.agents/MEMORY.md` | Partial (context window only) |

### Learning Note Format

```yaml
---
type: learning
id: lrn-{slug}-{hash6}
created: 2026-03-13T12:00:00Z
scope: universal | domain:{tech} | project:{name}
confidence: high | medium | low
learning_type: pattern | correction | bug-fix | decision | anti-pattern
title: "Descriptive title"
tags: [rust, async, tokio]
symptoms: ["nested runtime panic on block_on"]
key_insight: "THE ONE THING that fixes it"
---

## Problem
What went wrong and how it manifested.

## Solution
Step-by-step resolution.

## Anti-Pattern
What NOT to do (and why).

## Context
When this applies, version constraints, etc.
```

### Entity Sidecar Format

Pre-extracted entities that accompany each learning, so GraphRAG never needs to call an external LLM:

```yaml
document_id: tokio-runtime-panic-abc123
entities:
  - name: "tokio"
    type: technology       # technology | error | pattern | function | concept | tool
    description: "Async runtime for Rust"
relationships:
  - source: "block_on"
    target: "nested runtime panic"
    type: caused_by        # caused_by | solves | requires | relates_to
    description: "Calling block_on inside async context causes nested runtime panic"
    strength: 9            # 1-10
```

---

## /research — Knowledge Retrieval

`/research` spawns parallel sub-agents to search across multiple sources, then synthesises findings into a single report.

### Retrieval Pipeline

```
  User Query
       │
       ▼
┌──────────────────────────────────────────┐
│  Spawn parallel sub-agents               │
│                                          │
│  ┌─────────────┐  ┌──────────────────┐   │
│  │ Learnings   │  │ Codebase         │   │
│  │ Research    │  │ Research         │   │
│  └──────┬──────┘  └──────────────────┘   │
│         │          ┌──────────────────┐   │
│         │          │ Documentation    │   │
│         │          │ Research         │   │
│         │          └──────────────────┘   │
│         │          ┌──────────────────┐   │
│         │          │ Web Research     │   │
│         │          │ (optional)       │   │
│         │          └──────────────────┘   │
└─────────┼────────────────────────────────┘
          │
          ▼
  Learnings search runs ALL backends in parallel and merges results:

  ┌────────────────────────────────────────┐
  │  Local grep search                     │  ◄── Text scoring
  │  (docs/solutions/ in project)          │      (title > symptoms
  │                                        │       > insight > tags)
  ├────────────────────────────────────────┤
  │  QMD hybrid search                     │  ◄── Best for matching
  │  (BM25 + vector + LLM reranking)       │      "what matches
  │                                        │       the query"
  ├────────────────────────────────────────┤
  │  GraphRAG search                       │  ◄── Best for discovering
  │  (entity graph + relationships)        │      "what's connected
  │                                        │       to the query"
  │  └─ internal fallback: local → naive   │
  └────────────────┬───────────────────────┘
                   │
                   ▼  all results merged
  ┌────────────────────────────────────────┐
  │  Synthesise all findings               │
  │  → Save to research/YYYY-MM-DD_*.md   │
  └────────────────────────────────────────┘

  QMD and GraphRAG are COMPLEMENTARY, not fallback:
  • QMD finds documents by keyword/semantic similarity
  • GraphRAG traverses entity relationships (nodes, edges, communities)
  • Together they provide comprehensive coverage
  • Only fallback: within GraphRAG itself (local mode → naive mode)
```

---

## Two-Tier Storage Architecture

### Hot Tier — Project-Local

| | |
|---|---|
| **Location** | `./docs/solutions/{category}/` |
| **Scope** | Current project only |
| **Search** | Text scoring via `search-learnings.sh` |
| **Speed** | Fastest — no embedding needed |
| **Scoring** | title (100) > symptoms (80) > key_insight (60) > tags (40) > content (20) |

```
project/
└── docs/
    └── solutions/
        ├── patterns/
        │   ├── critical-patterns.md
        │   └── critical-patterns.entities.yaml
        ├── debugging/
        │   ├── tokio-runtime-panic.md
        │   └── tokio-runtime-panic.entities.yaml
        └── decisions/
            └── chose-sqlx-over-diesel.md
```

### Cold Tier — Global Cross-Project

| | |
|---|---|
| **Location** | `~/.learnings/documents/` |
| **Scope** | Universal, all projects |
| **Search** | QMD (hybrid) + GraphRAG (graph) |
| **Speed** | Slower but semantically richer |
| **Indexed by** | `learnings add` → dual QMD + GraphRAG |

```
~/.learnings/
├── cli/
│   └── learnings              # CLI entry point
├── documents/
│   ├── learnings/             # Knowledge notes + sidecars
│   │   ├── tokio-panic.md
│   │   └── tokio-panic.entities.yaml
│   └── episodes/             # Session snapshots (provenance)
│       └── ep-20260313-a1b2c3.md
├── nano_graphrag_cache/       # GraphRAG index
└── qmd/                       # QMD embeddings
```

---

## Search Engines

Both search engines run **in parallel** during `/research` and their results are **merged**. They answer different questions about the same query.

### QMD (Query Markdown) — "What matches?"

Fast hybrid search combining three strategies to find the most relevant documents:

```
Query
  │
  ├──► BM25 keyword matching     (exact terms)
  ├──► Vector similarity          (semantic meaning, all-mpnet-base-v2)
  └──► LLM reranking             (contextual relevance)
  │
  ▼
Ranked documents by relevance
```

- **Embedding model**: `all-mpnet-base-v2` (768 dimensions, runs locally on CPU)
- **No API key required** — fully local
- **Collection**: `learnings` with context hierarchy: clusters > episodes > learnings
- **Strength**: Finding the best direct matches for a query

### GraphRAG (nano-graphrag) — "What's connected?"

Graph-based semantic search that discovers relationships between concepts:

```
Query
  │
  ▼
┌─────────────────────────────────────────────────┐
│  Entity Graph                                    │
│                                                  │
│  [tokio] ──caused_by──► [nested runtime panic]  │
│     │                          │                 │
│     └──requires──► [async]     └──solves──►      │
│                       │        [spawn_blocking]  │
│                       │                          │
│              ──relates_to──► [futures]            │
└─────────────────────────────────────────────────┘
```

Three search modes (with internal fallback: local → naive):

| Mode | Method | Best for |
|------|--------|----------|
| `naive` | Vector similarity only | Fast exact symptom matching |
| `local` | Entity neighbourhood graph | Finding related concepts via relationships |
| `global` | Community-based reports | Broad patterns across all learnings |

- **Strength**: Discovering learnings that are conceptually related but wouldn't match keyword search (e.g., searching for "panic" also surfaces learnings about `spawn_blocking` via the entity graph)

**Key architectural decisions**:
- **Passthrough LLM**: Uses pre-extracted `.entities.yaml` sidecars instead of calling an external API
- **Batch inserts only**: Never call `insert()` sequentially — always batch via `insert_documents_batch()` or `learnings reindex`
- **File-based locks**: fcntl locks with 5-minute timeout for multi-process safety

### Why Both?

| | QMD | GraphRAG |
|---|---|---|
| **Question answered** | "Which documents match this query?" | "What concepts are related to this query?" |
| **Search method** | Keyword + vector + reranking | Entity graph traversal |
| **Finds** | Direct matches | Connected concepts |
| **Example** | "tokio panic" → finds doc about tokio panics | "tokio panic" → also surfaces spawn_blocking, async runtimes, futures |

---

## Supporting Components

### compound-docs

Pre-extracts entities from learning documents into `.entities.yaml` sidecars **before** `/reflect` indexes them. This prevents expensive external LLM calls during GraphRAG indexing.

### instincts

Lightweight YAML micro-learnings with confidence scoring (0.3-0.9). Project-scoped rules that feed into the global learnings knowledge base when confidence is high enough. Think of these as "muscle memory" for agents.

### episodes

Raw session snapshots stored in the cold tier for provenance. Each episode records what happened in a session and links to the learnings that were extracted from it.

---

## CLI Reference

### /reflect commands

```bash
/reflect                    # Full analysis (behavioral + knowledge)
/reflect --behavioral       # Only agent file updates
/reflect --knowledge        # Only learning notes
/reflect --review           # Review pending LOW confidence learnings
/reflect --status           # Show metrics and KB stats
/reflect --consolidate      # Merge orphaned worktree memories
/reflect on                 # Enable auto-reflection
/reflect off                # Disable auto-reflection
```

### /research commands

```bash
/research [query]           # Comprehensive multi-source research
```

### learnings CLI

```bash
learnings search "query" --mode naive|local|global  # Semantic search
learnings add ./doc.md --entities ./doc.entities.yaml  # Index document
learnings reindex [--force]                            # Rebuild graph
learnings stats                                        # KB statistics
learnings critical-patterns [--language rust]           # High-confidence patterns
learnings visualize                                    # Interactive HTML graph
```

### search-learnings.sh (hot tier)

```bash
search-learnings.sh "query"
  -d, --dir <path>         # Directory to search (default: ./docs/solutions)
  -c, --category <cat>     # Filter by category
  -t, --tag <tag>          # Filter by tag
  -l, --limit <n>          # Max results (default: 10)
  -f, --format <fmt>       # full | summary | json
```

---

## How It All Connects

```
┌────────────────────────────────────────────────────────────┐
│                    SESSION LIFECYCLE                         │
│                                                             │
│  Start ──► Work ──► /reflect ──► Learnings captured         │
│                                      │                      │
│                              ┌───────┴────────┐             │
│                              ▼                ▼             │
│                         Hot Tier          Cold Tier          │
│                      (project-local)     (global)           │
│                              │                │             │
│                              │    ┌───────────┤             │
│                              │    ▼           ▼             │
│                              │   QMD      GraphRAG          │
│                              │  (hybrid)  (entity graph)    │
│                              │    │           │             │
│                              └────┼───────────┘             │
│                                   │                         │
│  Next session ──► /research ──────┘                         │
│                       │                                     │
│                       ▼                                     │
│                  Finds past learnings                       │
│                  Prevents re-research                       │
│                  Avoids repeated mistakes                   │
└────────────────────────────────────────────────────────────┘
```

The core loop: **capture knowledge once, retrieve it everywhere, forever.**
