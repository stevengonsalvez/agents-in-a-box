---
title: "Knowledge & Memory System"
---

> "Correct once, never again. Solve once, never re-research."

This document explains the complete knowledge capture, storage, indexing, and
retrieval system in agents-in-a-box — including all memory tiers, the reflection
pipeline, semantic search engines, micro-learnings, and session context loading.


> **Heads-up — reflect now lives in its own repo.** The reflect engine (`reflect-kb`) and its
> Claude Code plugin were extracted from this monorepo into
> **[github.com/stevengonsalvez/ainb-reflect-memory](https://github.com/stevengonsalvez/ainb-reflect-memory)**
> (flattened: engine at the repo root, plugin under `plugin/`). Install the CLI with
> `uv tool install --upgrade 'git+https://github.com/stevengonsalvez/ainb-reflect-memory.git[graph]'`
> and the plugin with `claude plugin install reflect@ainb-reflect-memory`. The concepts below are unchanged.

---

## Architecture at a Glance

<figure class="svg-wrap" style="margin:0 0 8px">
<svg style="width:100%;height:auto;max-width:1000px" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1200 880" font-family="ui-sans-serif,system-ui,Inter,sans-serif">
<defs>
<marker id="a-141413" markerWidth="11" markerHeight="8" refX="9" refY="4" orient="auto"><path d="M0,0 L10,4 L0,8 Z" fill="#141413"/></marker>
<marker id="a-D97757" markerWidth="11" markerHeight="8" refX="9" refY="4" orient="auto"><path d="M0,0 L10,4 L0,8 Z" fill="#D97757"/></marker>
<marker id="a-788C5D" markerWidth="11" markerHeight="8" refX="9" refY="4" orient="auto"><path d="M0,0 L10,4 L0,8 Z" fill="#788C5D"/></marker>
</defs>
<rect width="1200" height="880" fill="#FAF9F5"/>
<text x="600" y="44" text-anchor="middle" font-size="25" font-weight="700" fill="#141413">reflect &#8212; capture &#8594; index &#8594; recall</text>
<rect x="40" y="74" width="1120" height="118" rx="16" fill="#FFFFFF" stroke="#E3DACC" stroke-width="2"/>
<text x="58" y="98" font-size="12" font-weight="700" letter-spacing="1" fill="#87867F">AGENT SESSION</text>
<rect x="60" y="112" width="250" height="60" rx="12" fill="#FBF3EE" stroke="#E3DACC" stroke-width="1.5"/>
<text x="185.0" y="140.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">/prime</text>
<text x="185.0" y="155.0" text-anchor="middle" font-size="11" fill="#87867F">load context</text>
<rect x="340" y="112" width="250" height="60" rx="12" fill="#FBF3EE" stroke="#E3DACC" stroke-width="1.5"/>
<text x="465.0" y="140.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">Working session</text>
<text x="465.0" y="155.0" text-anchor="middle" font-size="11" fill="#87867F">generates signals</text>
<rect x="620" y="112" width="250" height="60" rx="12" fill="#FBF3EE" stroke="#E3DACC" stroke-width="1.5"/>
<text x="745.0" y="140.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">/reflect</text>
<text x="745.0" y="155.0" text-anchor="middle" font-size="11" fill="#87867F">capture corrections</text>
<rect x="900" y="112" width="250" height="60" rx="12" fill="#FBF3EE" stroke="#E3DACC" stroke-width="1.5"/>
<text x="1025.0" y="140.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">/recall &#183; /research</text>
<text x="1025.0" y="155.0" text-anchor="middle" font-size="11" fill="#87867F">retrieve</text>
<rect x="40" y="236" width="1120" height="350" rx="16" fill="#F6E7DF" stroke="#D97757" stroke-width="2"/>
<text x="58" y="262" font-size="12" font-weight="700" letter-spacing="1" fill="#D97757">reflect ENGINE</text>
<text x="1142" y="262" text-anchor="end" font-size="11" fill="#87867F">ships from github.com/stevengonsalvez/ainb-reflect-memory</text>
<rect x="62" y="300" width="150" height="64" rx="12" fill="#FFFFFF" stroke="#D97757" stroke-width="1.5"/>
<text x="137.0" y="330.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">/reflect</text>
<text x="137.0" y="345.0" text-anchor="middle" font-size="11" fill="#87867F">analyse convo</text>
<rect x="244" y="300" width="160" height="64" rx="12" fill="#FFFFFF" stroke="#D97757" stroke-width="1.5"/>
<text x="324.0" y="330.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">Extract</text>
<text x="324.0" y="345.0" text-anchor="middle" font-size="11" fill="#87867F">corrections + signals</text>
<rect x="436" y="300" width="170" height="64" rx="12" fill="#FFFFFF" stroke="#D97757" stroke-width="1.5"/>
<text x="521.0" y="330.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">Atomic notes</text>
<text x="521.0" y="345.0" text-anchor="middle" font-size="11" fill="#87867F">.md + .entities.yaml</text>
<rect x="638" y="300" width="120" height="64" rx="12" fill="#D97757" stroke="#141413" stroke-width="1.5"/>
<text x="698.0" y="337.0" text-anchor="middle" font-size="16" font-weight="600" fill="#FFFFFF">INDEX</text>
<rect x="800" y="256" width="200" height="52" rx="12" fill="#E7EADF" stroke="#788C5D" stroke-width="1.5"/>
<text x="900.0" y="280.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">GraphRAG</text>
<text x="900.0" y="295.0" text-anchor="middle" font-size="11" fill="#87867F">nano-graphrag: graph</text>
<rect x="800" y="356" width="200" height="52" rx="12" fill="#E7EADF" stroke="#788C5D" stroke-width="1.5"/>
<text x="900.0" y="380.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">QMD / BM25</text>
<text x="900.0" y="395.0" text-anchor="middle" font-size="11" fill="#87867F">fast lexical</text>
<rect x="1024" y="300" width="118" height="64" rx="12" fill="#FFFFFF" stroke="#D97757" stroke-width="1.5"/>
<text x="1083.0" y="330.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">Hybrid recall</text>
<text x="1083.0" y="345.0" text-anchor="middle" font-size="11" fill="#87867F">rerank</text>
<path d="M 212,332 L 244,332" stroke="#D97757" stroke-width="2" fill="none" marker-end="url(#a-D97757)"/>
<path d="M 404,332 L 436,332" stroke="#D97757" stroke-width="2" fill="none" marker-end="url(#a-D97757)"/>
<path d="M 606,332 L 638,332" stroke="#D97757" stroke-width="2" fill="none" marker-end="url(#a-D97757)"/>
<path d="M 758,322 L 800,288" stroke="#788C5D" stroke-width="2" fill="none" marker-end="url(#a-788C5D)"/>
<path d="M 758,342 L 800,372" stroke="#788C5D" stroke-width="2" fill="none" marker-end="url(#a-788C5D)"/>
<path d="M 1000,282 L 1024,322" stroke="#788C5D" stroke-width="2" fill="none" marker-end="url(#a-788C5D)"/>
<path d="M 1000,382 L 1024,342" stroke="#788C5D" stroke-width="2" fill="none" marker-end="url(#a-788C5D)"/>
<path d="M 1083,300 C 1083,210 1025,196 1025,176" stroke="#D97757" stroke-width="2.5" fill="none" marker-end="url(#a-D97757)"/>
<rect x="1006" y="216" width="170" height="17" rx="3" fill="#FAF9F5" opacity="0.95"/>
<text x="1091" y="229" text-anchor="middle" font-size="10.5" fill="#87867F">SessionStart injects top-3</text>
<path d="M 700,172 L 140,300" stroke="#141413" stroke-width="2" fill="none" stroke-dasharray="5,4" marker-end="url(#a-141413)"/>
<rect x="392.2" y="241" width="55.6" height="16" rx="3" fill="#FAF9F5" opacity="0.95"/>
<text x="420" y="253" text-anchor="middle" font-size="10.5" fill="#87867F">capture</text>
<rect x="40" y="630" width="1120" height="150" rx="16" fill="#E3DACC" stroke="#788C5D" stroke-width="1.5" opacity="0.55"/>
<text x="58" y="656" font-size="12" font-weight="700" letter-spacing="1" fill="#87867F">MEMORY TIERS</text>
<rect x="62" y="668" width="350" height="92" rx="12" fill="#FFFFFF" stroke="#E3DACC" stroke-width="1.5"/>
<text x="237.0" y="712.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">Tier 1 &#183; Context window</text>
<text x="237.0" y="727.0" text-anchor="middle" font-size="11" fill="#87867F">MEMORY.md, CLAUDE.md &#8212; always loaded</text>
<rect x="426" y="668" width="350" height="92" rx="12" fill="#FFFFFF" stroke="#E3DACC" stroke-width="1.5"/>
<text x="601.0" y="712.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">Tier 2 &#183; Project-local</text>
<text x="601.0" y="727.0" text-anchor="middle" font-size="11" fill="#87867F">docs/solutions, instincts.yaml &#8212; text search</text>
<rect x="790" y="668" width="350" height="92" rx="12" fill="#FFFFFF" stroke="#788C5D" stroke-width="1.5"/>
<text x="965.0" y="712.0" text-anchor="middle" font-size="15" font-weight="600" fill="#141413">Tier 3 &#183; Global KB</text>
<text x="965.0" y="727.0" text-anchor="middle" font-size="11" fill="#87867F">~/.claude/global-learnings/ &#8212; GraphRAG + QMD</text>
<path d="M 900,408 L 965,668" stroke="#788C5D" stroke-width="2" fill="none" stroke-dasharray="5,4" marker-end="url(#a-788C5D)"/>
<rect x="945.4" y="531" width="69.19999999999999" height="16" rx="3" fill="#FAF9F5" opacity="0.95"/>
<text x="980" y="543" text-anchor="middle" font-size="10.5" fill="#87867F">persisted</text>
</svg>
<figcaption style="font-size:12px;color:#87867F;text-align:center;margin-top:6px">reflect captures corrections, indexes them into GraphRAG + QMD, and recalls the most relevant at every session start. Engine + plugin ship from <a href="https://github.com/stevengonsalvez/ainb-reflect-memory">ainb-reflect-memory</a>.</figcaption>
</figure>

---

## Tier 1: Context Window Memory

Memory that is **always loaded** into every conversation turn — no search required.

| Source | Location | Scope |
|--------|----------|-------|
| Agent instructions | `CLAUDE.md` / `AGENTS.md` | Project rules, conventions, behaviours |
| Auto-memory | `~/.claude/projects/{hash}/memory/MEMORY.md` | Cross-session notes for this project |
| Project memory | `.agents/MEMORY.md` | Project-specific gotchas, architecture notes |

### Auto-Memory (`MEMORY.md`)

Persistent notes written by the agent across sessions. Stored per-project at
`~/.claude/projects/{project-hash}/memory/MEMORY.md`. Contents are injected
into every conversation automatically.

**What belongs here:**
- Shell aliases and environment quirks
- Dependency management gotchas (e.g. "nano-graphrag needs `--no-deps`")
- Architecture principles confirmed across sessions
- Key file paths and conventions

**What does NOT belong here:**
- Session-specific state or in-progress work
- Anything duplicating CLAUDE.md instructions
- Unverified assumptions from a single file read

### Project Memory (`.agents/MEMORY.md`)

Project-scoped memory committed to the repo. Shared across all team members
and agents. `/reflect` routes project-specific gotchas here.

**200-line limit** — if exceeded, verbose items should be moved to skills or docs/solutions/.

---

## Tier 2: Project-Local Knowledge

Structured learning notes stored in the project repo, searchable via text scoring.

```
project/
└── docs/
    └── solutions/
        ├── debugging-sessions/
        │   ├── tokio-runtime-panic.md
        │   └── tokio-runtime-panic.entities.yaml
        ├── patterns/
        │   ├── critical-patterns.md
        │   └── critical-patterns.entities.yaml
        └── decisions/
            └── chose-sqlx-over-diesel.md
```

### Search: `search-learnings.sh`

Fast grep-based search with weighted YAML frontmatter scoring:

```
Query: "tokio panic"
  ↓
┌─────────────────────────────┐
│  Weighted Frontmatter Scan  │
│                             │
│  title:       weight 100   │  ← "tokio runtime panic" → match!
│  symptoms[]:  weight  80   │  ← ["nested runtime panic"] → match!
│  key_insight: weight  60   │
│  tags[]:      weight  40   │  ← [rust, tokio] → match!
│  content:     weight  20   │
└──────────┬──────────────────┘
           ↓
  Ranked results (highest score first)
```

```bash
search-learnings.sh "query"
  -d, --dir <path>     # Directory to search (default: ./docs/solutions)
  -c, --category <cat> # Filter by category
  -t, --tag <tag>      # Filter by tag
  -l, --limit <n>      # Max results (default: 10)
  -f, --format <fmt>   # full | summary | json
```

---

## Tier 3: Global Knowledge Base

Cross-project knowledge indexed with **dual search engines** that run in parallel.

```
~/.learnings/
├── cli/
│   └── learnings              # CLI entry point (bash → Python)
├── documents/
│   ├── learnings/             # Knowledge notes + entity sidecars
│   │   ├── tokio-panic-a1b2c3.md
│   │   └── tokio-panic-a1b2c3.entities.yaml
│   └── episodes/              # Session snapshots (provenance)
│       └── 2026-03-16/
│           └── ep-20260316-b6b22d.md
├── nano_graphrag_cache/       # GraphRAG index (GraphML + vectors)
│   ├── graph_chunk_entity_relation.graphml
│   ├── vdb_entities.json
│   └── vdb_chunks.json
└── qmd/                       # QMD embeddings
```

### Search Engine 1: QMD — "What matches?"

Hybrid search combining three strategies for best-in-class retrieval:

```
Query ──┬──► BM25 keyword matching     (exact terms)
        ├──► Vector similarity          (semantic, all-mpnet-base-v2, 768d)
        └──► LLM reranking             (contextual relevance)
        │
        ▼
   Ranked documents by relevance score
```

- Runs fully locally (no API key)
- Embedding model: `all-mpnet-base-v2` on CPU/MPS
- Best for: finding documents that directly match a query

### Search Engine 2: GraphRAG — "What's connected?"

Entity graph traversal that discovers relationships invisible to keyword search:

```
  ┌─────────────────────────────────────────────────────┐
  │                   Entity Graph                       │
  │                                                      │
  │  [extended thinking] ──caused_by──► [model spiral]  │
  │         │                                │           │
  │         └──relates_to──►                 │           │
  │           [system prompt overload]       │           │
  │                   │              solves───┘           │
  │                   └──caused_by──► [hook density]     │
  │                                                      │
  │  [tmux send-keys] ──caused_by──► [prompt failure]   │
  │         │                               │            │
  │         └──requires──►          solves───┘            │
  │           [spawn_agent_tmux]  [capture-pane verify]  │
  └─────────────────────────────────────────────────────┘
```

Three search modes (with fallback: local → naive):

| Mode | Method | Best for |
|------|--------|----------|
| `naive` | Vector similarity only | Fast exact symptom matching |
| `local` | Entity neighbourhood graph | Related concepts via edges |
| `global` | Community-based reports | Broad patterns across all learnings |

### Why Both Engines?

| | QMD | GraphRAG |
|---|---|---|
| **Question** | "Which docs match this query?" | "What concepts are related?" |
| **Method** | Keyword + vector + reranking | Entity graph traversal |
| **Finds** | Direct matches | Connected concepts |
| **Example** | "tmux timing" → tmux timing doc | "tmux timing" → also surfaces spawn_agent, swarm-lib, REPL readiness |

They are **complementary, not fallback**. Both run in parallel and results merge.

### Key Architecture Decisions

- **Passthrough LLM**: Pre-extracted `.entities.yaml` sidecars feed GraphRAG directly — no external LLM API calls during indexing
- **Batch inserts only**: Never call `insert()` sequentially — use `insert_documents_batch()` or `learnings reindex`
- **File-based locks**: fcntl locks with 5-minute timeout for multi-process safety
- **Local embedding**: `all-mpnet-base-v2` runs on CPU/MPS — zero cloud dependency

---

## Tier 4: Instincts — Micro-Learnings

Lightweight YAML rules with confidence scoring (0.3–0.9). Too small for a full
knowledge note, but important enough to remember.

```yaml
# .agents/instincts.yaml
version: 1
instincts:
  - id: inst-20260310-a1b2c3
    rule: "Use pnpm instead of npm for this project"
    confidence: 0.7
    scope: project       # project | domain | universal
    category: tooling    # tooling | style | api | testing | architecture
    created: 2026-03-10
    last_reinforced: 2026-03-10
    reinforcement_count: 1
    source: "User corrected npm to pnpm"
```

### Confidence Lifecycle

```
  New instinct (0.5)
       │
       ├──► Reinforced (+0.1 per confirmation, max 0.9)
       │
       ├──► Contradicted (-0.2 per contradiction)
       │
       ├──► Stale (no reinforcement in 30 days → -0.1)
       │
       └──► Promoted to Tier 3 (when confidence ≥ 0.8 + universal scope)
              │
              ▼
        Full learning note in ~/.learnings/
```

```bash
/instincts              # Show active instincts
/instincts add          # Manually add
/instincts review       # Adjust confidence
/instincts promote      # Push high-confidence to global KB
/instincts prune        # Remove low-confidence or stale
```

---

## /reflect — Knowledge Capture Pipeline

`/reflect` analyses conversations to extract two signal types and route them
to the appropriate memory tier.

### Full Pipeline

```
  ┌───────────────────────────────────────────────────────────────────────┐
  │                                                                       │
  │  STEP 1: SCAN                                                         │
  │  ┌──────────────────────┐                                             │
  │  │  Signal Detection    │   Linguistic patterns:                      │
  │  │                      │     HIGH:   "never", "always", "must"       │
  │  │  Conversation ──────►│     MEDIUM: "perfect", "exactly right"      │
  │  │                      │     LOW:    "consider", "perhaps"           │
  │  └──────────┬───────────┘                                             │
  │             │                                                         │
  │  STEP 2: CLASSIFY                                                     │
  │  ┌──────────┴───────────┐                                             │
  │  │  Route Each Signal   │                                             │
  │  └──┬──────┬──────┬─────┘                                             │
  │     │      │      │                                                   │
  │     │      │      └──► Project gotcha ──► .agents/MEMORY.md (Tier 1)  │
  │     │      │                                                          │
  │     │      └─────────► Knowledge signal ──► Learning note + sidecar   │
  │     │                    (fix, pattern,      + Episode snapshot        │
  │     │                     decision)                                    │
  │     │                                                                  │
  │     └────────────────► Behavioral signal ──► Agent config file diff   │
  │                          (correction,                                  │
  │                           preference)                                  │
  │             │                                                         │
  │  STEP 3: DE-DUPLICATE                                                 │
  │  ┌──────────┴───────────┐                                             │
  │  │  QMD similarity check │   If match found:                          │
  │  │  against existing KB  │     → propose UPDATE, not new note         │
  │  └──────────┬───────────┘   If partial match:                         │
  │             │                 → propose LINK via links: field          │
  │             │                                                         │
  │  STEP 4: GENERATE                                                     │
  │  ┌──────────┴───────────┐                                             │
  │  │  Draft proposals      │   • Agent file diffs (behavioral)          │
  │  │                       │   • Learning notes (knowledge)             │
  │  │                       │   • Entity sidecars (.entities.yaml)       │
  │  │                       │   • Episode note (auto, no approval)       │
  │  └──────────┬───────────┘                                             │
  │             │                                                         │
  │  STEP 5: USER APPROVAL                                                │
  │  ┌──────────┴───────────┐                                             │
  │  │  Present full diffs   │   Options: Y (all), N, modify,            │
  │  │  NEVER auto-apply     │   1,3 (selective), all-knowledge,          │
  │  │                       │   all-behavioral                           │
  │  └──────────┬───────────┘                                             │
  │             │                                                         │
  │  STEP 6: INDEX                                                        │
  │  ┌──────────┴────────────────────────────────────────────────┐        │
  │  │                                                           │        │
  │  │  Behavioral ──► Apply agent file diffs                    │        │
  │  │                                                           │        │
  │  │  Knowledge  ──► Write to docs/solutions/ (Tier 2)         │        │
  │  │             ──► learnings add --entities (Tier 3)         │        │
  │  │                   ├──► QMD embed                          │        │
  │  │                   └──► GraphRAG insert                    │        │
  │  │                                                           │        │
  │  │  Episode    ──► Auto-write to ~/.learnings/episodes/      │        │
  │  │             ──► learnings add (index for search)          │        │
  │  │                                                           │        │
  │  │  Project    ──► Append to .agents/MEMORY.md (Tier 1)      │        │
  │  │                                                           │        │
  │  │  Git commit with descriptive message                      │        │
  │  └───────────────────────────────────────────────────────────┘        │
  │             │                                                         │
  │  STEP 7: UPDATE METRICS                                               │
  │  ┌──────────┴───────────┐                                             │
  │  │  ~/.learnings/        │   Tracks: signals detected/accepted,       │
  │  │  metrics.yaml         │   by type, by confidence, by scope         │
  │  └──────────────────────┘                                             │
  └───────────────────────────────────────────────────────────────────────┘
```

### Signal Routing Summary

| Signal Characteristic | Target Tier | Target File |
|-----------------------|-------------|-------------|
| Behavioral correction ("always do X") | Agent file | `~/.claude/agents/*.md` |
| Reusable fix, pattern, technique | Tier 2 + 3 | `docs/solutions/` + `~/.learnings/` |
| Project-specific gotcha | Tier 1 | `.agents/MEMORY.md` |
| Recurring bug with reusable fix | Tier 2 + 3 | `docs/solutions/` + new skill |
| Domain term / business rule | Tier 1 | `.agents/MEMORY.md` |
| Low confidence + project-specific | Tier 1 only | `.agents/MEMORY.md` |

### Learning Note Format

```yaml
---
title: "Descriptive title"
category: debugging-sessions    # or patterns, decisions, anti-patterns
tags: [rust, async, tokio]
symptoms:
  - "nested runtime panic on block_on"
root_cause: "Calling block_on inside an async context"
key_insight: "THE ONE THING that fixes it"
created: "2026-03-16"
confidence: high
language: rust
framework: tokio
---

## Problem
What went wrong and how it manifested.

## Solution
Step-by-step resolution with code.

## Context
When this applies, version constraints, alternatives considered.
```

### Entity Sidecar Format

Pre-extracted entities that feed GraphRAG without external LLM calls:

```yaml
document_id: tokio-runtime-panic-abc123
entities:
  - name: "tokio"
    type: technology        # technology | error | pattern | function | concept | tool
    description: "Async runtime for Rust"
  - name: "block_on"
    type: function
    description: "Blocking call to run an async future"
relationships:
  - source: "block_on"
    target: "nested runtime panic"
    type: caused_by         # caused_by | solves | requires | relates_to
    description: "Calling block_on inside async context triggers panic"
    strength: 9             # 1-10
```

### /reflect Subcommands

```bash
/reflect                    # Full analysis (behavioral + knowledge)
/reflect --behavioral       # Only agent file updates
/reflect --knowledge        # Only learning notes
/reflect --review           # Review pending LOW confidence learnings
/reflect --status           # Show metrics and KB stats
/reflect --consolidate      # Merge orphaned worktree memories
/reflect on                 # Enable auto-reflection (PreCompact hook)
/reflect off                # Disable auto-reflection
/reflect [agent-name]       # Focus on specific agent (behavioral only)
```

---

## /research — Knowledge Retrieval Pipeline

`/research` spawns parallel sub-agents to search all sources, then synthesises
findings into a single report.

### Full Pipeline

```
  User Query: "How to fix tmux timing issues with agent spawning?"
       │
       ▼
  ┌────────────────────────────────────────────────────────────────┐
  │  SPAWN PARALLEL SUB-AGENTS                                     │
  │                                                                │
  │  ┌──────────────────┐  ┌──────────────────┐                   │
  │  │  1. LEARNINGS    │  │  2. CODEBASE     │                   │
  │  │  RESEARCH        │  │  RESEARCH        │                   │
  │  │                  │  │                  │                   │
  │  │  Searches ALL    │  │  Grep, Glob,     │                   │
  │  │  backends in     │  │  AST search      │                   │
  │  │  parallel        │  │  across repo     │                   │
  │  └────────┬─────────┘  └────────┬─────────┘                   │
  │           │                     │                              │
  │  ┌──────────────────┐  ┌──────────────────┐                   │
  │  │  3. DOCS         │  │  4. WEB          │                   │
  │  │  RESEARCH        │  │  RESEARCH        │                   │
  │  │                  │  │  (optional)      │                   │
  │  │  README, inline  │  │                  │                   │
  │  │  docs, external  │  │  WebSearch +     │                   │
  │  │  references      │  │  WebFetch        │                   │
  │  └────────┬─────────┘  └────────┬─────────┘                   │
  │           │                     │                              │
  └───────────┼─────────────────────┼──────────────────────────────┘
              │                     │
              ▼                     │
  ┌──────────────────────────────────────────────┐
  │  LEARNINGS SEARCH (runs all backends)        │
  │                                              │
  │  ┌────────────────────────────────────────┐  │
  │  │  Hot Tier: search-learnings.sh         │  │  ◄── Text scoring
  │  │  (docs/solutions/ in project)          │  │      title > symptoms
  │  │                                        │  │      > insight > tags
  │  ├────────────────────────────────────────┤  │
  │  │  Cold Tier: QMD hybrid search          │  │  ◄── BM25 + vector
  │  │  (BM25 + vector + LLM reranking)       │  │      + reranking
  │  ├────────────────────────────────────────┤  │
  │  │  Cold Tier: GraphRAG search            │  │  ◄── Entity graph
  │  │  (entity graph + relationships)        │  │      traversal
  │  │  └─ fallback: local → naive            │  │
  │  └────────────────────────────────────────┘  │
  │                                              │
  │  Results merged across all three backends    │
  └──────────────────────┬───────────────────────┘
                         │
                         ▼
  ┌──────────────────────────────────────────────┐
  │  SYNTHESISE                                  │
  │                                              │
  │  • Merge findings from all sub-agents        │
  │  • Resolve conflicts between sources         │
  │  • Generate structured report                │
  │  • Save to research/YYYY-MM-DD_*.md          │
  └──────────────────────────────────────────────┘
```

---

## /prime — Session Context Loading

`/prime` runs at session start to load relevant knowledge into the conversation.

```
  New Session
       │
       ▼
  ┌──────────────────────┐
  │  1. Codebase scan    │  git ls-files → understand structure
  └──────────┬───────────┘
             │
  ┌──────────┴───────────┐
  │  2. README analysis  │  Project purpose, setup, conventions
  └──────────┬───────────┘
             │
  ┌──────────┴───────────┐
  │  3. Detect tech stack│  Languages, frameworks, databases
  └──────────┬───────────┘
             │
  ┌──────────┴───────────┐
  │  4. Load learnings   │
  │                      │
  │  QMD query for       │  → Top 3-5 most relevant learnings
  │  detected stack      │
  │                      │
  │  Critical patterns   │  → High-confidence patterns for
  │  for language/domain │    this tech stack
  └──────────┬───────────┘
             │
             ▼
  Session primed with context + relevant past knowledge
```

---

## End-to-End Lifecycle

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         SESSION LIFECYCLE                                    │
│                                                                             │
│                                                                             │
│  ┌───────┐                                                                  │
│  │ START │                                                                  │
│  └───┬───┘                                                                  │
│      │                                                                      │
│      ▼                                                                      │
│  ┌──────────┐     Tier 1 loaded automatically:                              │
│  │  /prime   │     • CLAUDE.md (rules)                                      │
│  │  (load)   │     • auto-memory (cross-session notes)                      │
│  └────┬─────┘     • .agents/MEMORY.md (project gotchas)                     │
│       │           • instincts.yaml (micro-rules)                            │
│       │           Plus: QMD/critical-patterns for tech stack                │
│       │                                                                     │
│       ▼                                                                     │
│  ┌──────────────────────────────────────────────────────────────────┐       │
│  │                    WORKING SESSION                                │       │
│  │                                                                   │       │
│  │  Developer works with agent ◄───── /research queries as needed   │       │
│  │  Corrections, fixes, decisions      (searches all 4 tiers)       │       │
│  │  accumulate as conversation         returns past learnings        │       │
│  │                                                                   │       │
│  └────────────────────────────┬──────────────────────────────────────┘       │
│                               │                                             │
│                               ▼                                             │
│  ┌──────────────────────────────────────────────────────────────────┐       │
│  │  /reflect                                                         │       │
│  │                                                                   │       │
│  │  Scan ──► Classify ──► De-dup ──► Generate ──► Approve ──► Index │       │
│  │                                                                   │       │
│  │  Writes to:                                                       │       │
│  │    • Agent files        (behavioral corrections)                  │       │
│  │    • docs/solutions/    (knowledge notes — Tier 2)                │       │
│  │    • ~/.learnings/      (knowledge notes — Tier 3, dual-indexed)  │       │
│  │    • .agents/MEMORY.md  (project gotchas — Tier 1)                │       │
│  │    • instincts.yaml     (micro-learnings — Tier 4)                │       │
│  └──────────────────────────────────────────────────────────────────┘       │
│                               │                                             │
│                               ▼                                             │
│  ┌───────┐                                                                  │
│  │  END  │  Knowledge persists. Next session starts from /prime.            │
│  └───────┘                                                                  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘

                   The loop: capture once, retrieve everywhere, forever.
```

---

## Cross-Tool Deployment

The knowledge system is **tool-agnostic**. Skills deploy to multiple AI coding
tools via the `ainb` skill manager (`ainb-tui/`):

```
┌──────────────────────────────────────────────────────────────────┐
│  ainb-toolkit: skills/                                           │
│  (github.com/stevengonsalvez/ainb-toolkit — canonical source)   │
│                                                                  │
│  ├── reflect/          ├── research/       ├── prime/            │
│  ├── instincts/        └── compound-docs/                        │
│                                                                  │
└─────────────────┬────────────────────────────────────────────────┘
                  │
    ainb skill install <unit-uri> --targets <tools>
    (or `ainb skill sync` to reconcile the whole manifest)
                  │
       ┌──────────┼──────────────┐
       │          │              │
       ▼          ▼              ▼
  ~/.claude/   ~/.codex/    ~/.copilot/
  skills/      skills/      skills/

  Template substitutions:
  {{TOOL_DIR}}      → .claude / .codex / .copilot
  {{HOME_TOOL_DIR}} → ~/.claude / ~/.codex / ~/.copilot
```

The global knowledge base (`~/.learnings/`) is shared across all tools — a
learning captured in Claude Code is searchable from Codex or Copilot.

---

## CLI Reference

### learnings CLI

```bash
# Search
learnings search "query" --mode naive|local|global  # Semantic search
learnings search "query" --format json               # JSON output

# Index
learnings add ./doc.md --entities ./doc.entities.yaml  # Index with sidecar
learnings reindex [--force]                            # Rebuild entire graph

# Inspect
learnings stats                                        # KB statistics
learnings critical-patterns --language rust             # High-confidence patterns
learnings visualize                                    # Interactive HTML graph

# Manage
learnings list [--category debugging]                  # List documents
```

### search-learnings.sh (hot tier)

```bash
search-learnings.sh "query"
  -d, --dir <path>     # Directory to search (default: ./docs/solutions)
  -c, --category <cat> # Filter by category
  -t, --tag <tag>      # Filter by tag
  -l, --limit <n>      # Max results (default: 10)
  -f, --format <fmt>   # full | summary | json
```

### Skill commands

```bash
/reflect                    # Full capture (behavioral + knowledge)
/reflect --knowledge        # Knowledge capture only
/reflect --behavioral       # Agent file updates only
/reflect --review           # Review pending low-confidence items
/reflect --status           # Metrics and KB stats
/reflect --consolidate      # Merge orphaned worktree memories
/reflect on | off           # Toggle auto-reflection

/research [query]           # Multi-source retrieval
/prime                      # Session context loading
/instincts                  # Show project instincts
/instincts add | review | promote | prune
```

---

## Data Flow Summary

```
                    CAPTURE                              RETRIEVAL
                    ───────                              ─────────

  Conversation ──► /reflect ──┬──► Agent files           /research ──┬──► search-learnings.sh
                              │                                      │    (Tier 2: text scoring)
                              ├──► .agents/MEMORY.md                 │
                              │    (Tier 1: always loaded)           ├──► QMD hybrid search
                              │                                      │    (Tier 3: BM25+vector)
                              ├──► docs/solutions/                   │
                              │    (Tier 2: project-local)           ├──► GraphRAG search
                              │                                      │    (Tier 3: entity graph)
                              ├──► ~/.learnings/                     │
                              │    (Tier 3: QMD + GraphRAG)          └──► Codebase + Web + Docs
                              │                                           (parallel sub-agents)
                              └──► .agents/instincts.yaml
                                   (Tier 4: micro-learnings)
                                        │
                                        └──► promotes to Tier 3
                                             when confidence ≥ 0.8

  /prime loads Tier 1 + relevant Tier 3 at session start.
  /research searches Tier 2 + Tier 3 on demand.
  Tier 1 is always in the context window — no search needed.
  Tier 4 instincts live in context but promote to Tier 3 over time.
```

---

## Safety Guardrails

| Guardrail | Mechanism |
|-----------|-----------|
| **Human-in-the-loop** | `/reflect` NEVER auto-applies — all changes require explicit approval |
| **Git versioning** | Every capture is committed with descriptive message; `git revert` for rollback |
| **De-duplication** | QMD similarity check prevents knowledge base bloat |
| **Conflict detection** | Warns if proposed rule contradicts existing rule |
| **File-based locks** | fcntl locks with 5-minute timeout prevent concurrent index corruption |
| **Incremental only** | Reflect only adds to sections — never deletes or rewrites existing rules |
| **Metrics tracking** | `~/.learnings/metrics.yaml` tracks signal counts, acceptance rates |
