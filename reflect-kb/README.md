# reflect-kb

> Universal cross-harness retrieval + learning knowledge base for AI coding agents.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](./LICENSE)
[![Python](https://img.shields.io/badge/python-%3E%3D3.11-blue.svg)](https://www.python.org/)
[![Version](https://img.shields.io/badge/version-0.1.1-green.svg)](./pyproject.toml)

> **Two version streams — don't confuse them.** This directory hosts the `reflect`
> **CLI** (Python package `reflect-kb`, semver `0.1.x`, version field in
> [`pyproject.toml`](./pyproject.toml)). The Claude Code **plugin** that wires the
> CLI into the agent harness lives at [`plugins/reflect/`](../plugins/reflect/)
> and follows its **own** semver `3.x.x` (see
> [`plugins/reflect/.claude-plugin/plugin.json`](../plugins/reflect/.claude-plugin/plugin.json)).
> When asked "what version of reflect is installed?" you usually want **both**:
> `reflect --version` for the CLI and the plugin manifest for the harness wiring.

## What it does

reflect-kb implements the **capture → index → recall** loop for agent knowledge. After every session,
the agents-in-a-box reflect plugin drains the ingest queue (`~/.learnings/ingest/`), calls `reflect add`
for each pending document, and rebuilds the graph index. At the start of the next session, `reflect search`
recalls the most relevant prior learnings and surfaces them before the agent touches the first file.
The result is a compounding knowledge base that gets smarter the more it is used — without per-session
context-window blowup. Works across Claude Code, Codex CLI, and GitHub Copilot via cross-harness adapters
baked into the plugin.

## Install

Recommended: `uv tool install` with the `[graph]` extra (pulls the full GraphRAG + vector stack):

```bash
uv tool install --upgrade 'git+https://github.com/stevengonsalvez/reflect-kb.git[graph]'
```

Verify: `reflect --version` should print `0.1.x`.

**Post-consolidation install (after Phase 7 of the monorepo plan):**

```bash
uv tool install --upgrade 'git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]'
```

Both URLs will resolve during the transition window.

## Quick start

```bash
# 1. Initialise the KB (one time per machine — creates ~/.claude/global-learnings/)
reflect init

# 2. Add a learning document (with optional entity sidecar)
reflect add ./my-solution.md --entities ./my-solution.entities.yaml

# 3. Search the knowledge base
reflect search "how did we fix the tokio runtime panic"

# 4. Show KB statistics
reflect stats

# 5. Drill into the statusline dashboard
reflect timeline --explain TOK
```

## Subcommands

| Command | What it does |
|---|---|
| [`reflect init`](docs/usage.md#reflect-init) | Initialise the KB at `~/.claude/global-learnings/` |
| [`reflect add`](docs/usage.md#reflect-add) | Add a learning doc; `--force` for non-interactive overwrite |
| [`reflect search`](docs/usage.md#reflect-search) | Hybrid GraphRAG + vector search over the KB |
| [`reflect reindex`](docs/usage.md#reflect-reindex) | Rebuild the full graph index from all documents |
| [`reflect stats`](docs/usage.md#reflect-stats) | Show KB metrics (doc count, entities, relationships, confidence) |
| [`reflect critical-patterns`](docs/usage.md#reflect-critical-patterns) | Surface high-confidence, widely-applicable patterns |
| [`reflect generate-sidecars`](docs/usage.md#reflect-generate-sidecars) | Backfill missing `.entities.yaml` sidecars (heuristic, no LLM) |
| [`reflect metrics`](docs/usage.md#reflect-metrics) | Command group for recall-metrics aggregation (subcommands below) |
| &nbsp;&nbsp;↳ [`reflect metrics stats`](docs/usage.md#reflect-metrics-stats) | Aggregate the recall-metrics JSONL log: total events, hit rate, p50/p95 latency, top tags |
| [`reflect timeline`](docs/usage.md#reflect-timeline) | Drill down on statusline dashboard rows (REC/MEM/ING/DRN/TOK/ERR/COM/AGT) |

See [docs/usage.md](docs/usage.md) for per-subcommand synopsis, all flags, examples, and common errors.

## Architecture

```
┌────────────────────────────────────────────────────────────────┐
│  agents-in-a-box reflect plugin  (orchestrator)                │
│  ┌─────────────────┐  ┌──────────────────┐                     │
│  │ PreCompact hook │  │ SessionStart hook │                     │
│  │  drains ingest  │  │  calls `reflect   │                     │
│  │  queue; calls   │  │   search` + injects│                    │
│  │ `reflect add`   │  │   context         │                     │
│  └────────┬────────┘  └────────┬──────────┘                    │
│           │                    │                                │
└───────────┼────────────────────┼────────────────────────────────┘
            │                    │
            ▼                    ▼
┌───────────────────────────────────────────────────────────────┐
│  reflect-kb  (this library — Python CLI + retrieval engine)   │
│  • GraphRAG index (nano-graphrag)                             │
│  • Vector search (nano-vectordb + sentence-transformers)      │
│  • Entity sidecar store (.entities.yaml)                      │
│  • Metrics JSONL writer                                       │
└───────────────────────────────┬───────────────────────────────┘
                                │  reads/writes
                                ▼
┌───────────────────────────────────────────────────────────────┐
│  learnings-kb  (~/.claude/global-learnings/ or               │
│                 $GLOBAL_LEARNINGS_PATH)                       │
│  • documents/*.md          knowledge documents                │
│  • documents/*.entities.yaml   entity sidecars                │
│  • nano_graphrag_cache/    graph index (gitignored)           │
│  • metrics.jsonl           recall telemetry (rotated 10 MB)   │
└───────────────────────────────────────────────────────────────┘
```

**Key split:** reflect-kb is the data layer — it knows nothing about Claude Code. The plugin is the
orchestrator — it knows when to drain, recall, and surface. The learnings-kb content directory is a
separate git repo (private; content not code).

## Companion tooling

- **Claude Code plugin:** `claude plugin install reflect@agents-in-a-box`
  — ships the PreCompact + SessionStart hooks, drain script, recall skill, and statusline timeline.
- **Content directory:** `~/.claude/global-learnings/` (override with `$GLOBAL_LEARNINGS_PATH`).

## What's new in 0.1.1

- **`--force` flag on `reflect add`** — non-interactive overwrite for ingest pipelines and subprocess
  contexts where `click.confirm` cannot read a TTY. Without `--force`, non-TTY stdin now fails loudly
  rather than silently dropping the file.
- **Content-hash `doc_id`** — `doc_id` is now `slug(title) + sha256(title + body)[:6]`. Previously
  hashing title-only caused silent collisions when two documents shared a slug-able title. Same title +
  same body = same id (idempotent re-ingest). Same title + different body = distinct ids.
- **`reflect timeline --explain ROW`** — drill-down on a single statusline dashboard row (REC, MEM,
  ING, DRN, TOK, ERR, COM, AGT, or `all`). Delegates to the reflect plugin's `reflect_timeline.sh`
  helper, auto-discovered from `$CLAUDE_PLUGIN_ROOT` or the plugin cache.
- **`reflect metrics stats`** — aggregate the recall-metrics JSONL log: total events, hit rate,
  p50/p95 latency, top tags. Supports `--format json` for machine consumption and `--window-days` for
  custom time windows.

## License

MIT. See [LICENSE](./LICENSE).
