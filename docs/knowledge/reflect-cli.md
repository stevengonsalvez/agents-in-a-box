---
title: "`reflect` CLI reference"
---

The `reflect` command is the command-line interface to the agents-in-a-box knowledge base. It is shipped by the Python package **`reflect-kb`** (source: [`reflect-kb/`](https://github.com/stevengonsalvez/agents-in-a-box/tree/main/reflect-kb)) and provides the **capture → index → recall** loop: `reflect add` captures a learning, `reflect reindex` rebuilds the GraphRAG + vector index, and `reflect search` recalls the most relevant prior learnings.

## Two version streams

There are **two** independent versions and they are easy to confuse:

| Component | Package / manifest | Version | Source of truth |
|---|---|---|---|
| `reflect` **CLI** | Python package `reflect-kb` | `0.1.1` | [`reflect-kb/pyproject.toml`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/reflect-kb/pyproject.toml) |
| `reflect` **plugin** (Claude Code wiring) | [`plugins/reflect/`](https://github.com/stevengonsalvez/agents-in-a-box/tree/main/plugins/reflect) | `3.6.0` | [`plugins/reflect/.claude-plugin/plugin.json`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/plugins/reflect/.claude-plugin/plugin.json) |

`reflect --version` reports the CLI version (`0.1.x`). The plugin version describes the harness wiring (hooks, skills, adapters) and is documented separately in the plugin architecture docs.

## Install

Recommended — `uv tool install` with the `[graph]` extra (pulls the full GraphRAG + vector stack):

```bash
uv tool install --upgrade 'git+https://github.com/stevengonsalvez/agents-in-a-box.git#subdirectory=reflect-kb[graph]'
```

Verify the install:

```bash
reflect --version   # prints 0.1.x
```

> The `[graph]` extra will not resolve cleanly with plain `pip` on Python >= 3.11 because `nano-graphrag` pulls `graspologic -> hyppo -> numba -> llvmlite` (Python < 3.10 only). Use the `uv`/`pipx` `--no-deps` flow documented in [`reflect-kb/README.md`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/reflect-kb/README.md).

## Subcommands

| Command | What it does |
|---|---|
| `reflect init` | Initialise the KB at `~/.claude/global-learnings/`. |
| `reflect add <file>` | Add a learning document. `--entities <file>` attaches an entity sidecar; `--force` overwrites non-interactively (required in subprocess / non-TTY contexts). |
| `reflect search <query>` | Hybrid GraphRAG + vector search over the KB. Flags: `--mode`, `--tags/-t`, `--category/-c`, `--limit/-l`. |
| `reflect reindex` | Rebuild the full graph index from all documents. `--force` clears the cache and rebuilds from scratch. |
| `reflect stats` | Show KB metrics (document count, entities, relationships, confidence). |
| `reflect critical-patterns` | Surface high-confidence, widely-applicable patterns. Filter with `--language/-l`, `--domain/-d`. |
| `reflect generate-sidecars` | Backfill missing `.entities.yaml` sidecars heuristically (no LLM). `--force` regenerates all. |
| `reflect metrics stats` | Aggregate the recall-metrics JSONL log (total events, hit rate, p50/p95 latency, top tags). Supports `--format json` and `--window-days`. |
| `reflect timeline --explain <ROW>` | Drill down on a statusline dashboard row (`REC`, `MEM`, `ING`, `DRN`, `TOK`, `ERR`, `COM`, `AGT`, or `all`). Shells out to the reflect plugin's helper. |

The Python `console_scripts` entry point is `reflect = reflect_kb.cli.main:main` ([`pyproject.toml`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/reflect-kb/pyproject.toml)).

## Quick start

```bash
reflect init                                              # one time per machine
reflect add ./my-solution.md --entities ./my-solution.entities.yaml
reflect search "how did we fix the tokio runtime panic"
reflect stats
reflect timeline --explain TOK
```

## Content directory

The CLI is the data layer; it knows nothing about Claude Code. Knowledge content lives in a separate directory at `~/.claude/global-learnings/` (override with `$GLOBAL_LEARNINGS_PATH`), holding `documents/*.md`, `documents/*.entities.yaml` sidecars, the gitignored `nano_graphrag_cache/` index, and the rotated `metrics.jsonl` telemetry log.

## Plugin: skills, adapters and hooks

The `reflect` plugin (version `3.6.0`) wires the CLI into the agent harness. It ships:

**Six colon-namespaced skills** ([`plugins/reflect/skills/`](https://github.com/stevengonsalvez/agents-in-a-box/tree/main/plugins/reflect/skills)):

| Skill | Purpose |
|---|---|
| `reflect:reflect` | Full conversation scan for self-improvement; classifies corrections + knowledge signals. |
| `reflect:recall` | Retrieve relevant prior learnings (hybrid vector + graph search). |
| `reflect:ingest` | Global knowledge indexer; harvests memory sources across all tools into the KB. |
| `reflect:consolidate` | Project-level memory consolidation; merges orphaned worktree memory dirs. |
| `reflect:reflect-status` | Read-only views into reflect system state; can approve/reject pending items. |
| `reflect:errors-ack` | Acknowledge captured error signals. |

**Cross-harness adapters** ([`plugins/reflect/adapters/`](https://github.com/stevengonsalvez/agents-in-a-box/tree/main/plugins/reflect/adapters)): a shared `base.py` plus per-harness adapters under `claude/`, `codex/`, and `copilot/`.

**Five lifecycle hooks** (declared in [`plugin.json`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/plugins/reflect/.claude-plugin/plugin.json)):

| Hook | Action |
|---|---|
| `SessionStart` | Runs `recall` and kicks off the background ingest drainer. |
| `UserPromptSubmit` | Runs `recall` against the prompt. |
| `PostToolUse` | Arms low-cost mini-learning capture. |
| `Stop` | Enqueues short-session reflection. |
| `PreCompact` | Runs `precompact_reflect.py --auto --verbose` — **auto-installed**, so reflection fires before context compaction without manual setup. |

## See also

- [Knowledge base overview](./overview.md)
- [`reflect-kb/README.md`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/reflect-kb/README.md)
- [Docs hub](../README.md)