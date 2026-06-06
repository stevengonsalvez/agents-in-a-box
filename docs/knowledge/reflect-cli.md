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
| `reflect cost` | Report background-drain spend from `~/.reflect/drain-cost.jsonl` (plugin 4.0.0+). Groups by `--by day\|transcript\|model\|outcome`, shows the cached-vs-uncached token split with outlier flagging and a per-model `$` estimate. Flags: `--since 30d`, `--top N`, `--json`, `--state-dir`. Backed by `plugins/reflect/scripts/reflect_cost.py`. |
| `reflect errors count\|ack\|append` | Triage the error sink at `~/.reflect/errors.json`. `count` prints un-acked errors (drives the statusline ⚠ badge); `ack [ids…]` acknowledges them; `append --severity --source --kind --message --context` records one. Exposed on the installed binary so callers (statusline, drain hook, `/reflect:errors-ack`) avoid bare `python3 -m reflect_kb`, which needs reflect_kb importable by *system* python3 (it lives in the uv tool venv). The `python -m reflect_kb.errors` entrypoint still works for back-compat. |
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

> **Note:** the plugin-side recall/reflect/drain code (`recall.py`, `graphml_repair.py`, `reflect_synthesis.py`) canonically reads from `~/.learnings/` and treats `~/.claude/global-learnings/` as a legacy/deprecated fallback (the self-heal and synthesis scripts glob **both** paths). Both resolve in practice; `~/.learnings/` is the current canonical root in the plugin's architecture docs. The `reflect init` default above describes the `reflect-kb` CLI's own behaviour — verify with `reflect --help` for your installed version.

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
| `SessionStart` | Runs `recall` and kicks off the background drain (`reflect-drain-bg.sh`) — the sole consumer of the pending-reflections queue as of plugin 4.0.0. |
| `UserPromptSubmit` | Runs `recall` against the prompt. |
| `PostToolUse` | Arms low-cost mini-learning capture. |
| `Stop` | Gates ($0) and **queues** a short-session transcript for the bg-drain cascade — does not reflect synchronously. |
| `PreCompact` | Runs `precompact_reflect.py --auto --verbose` — **auto-installed**. Hook scripts can't run an LLM, so it gates ($0) and **queues** the transcript for the bg-drain cascade rather than reflecting inline. |

The producer hooks (`Stop`, `PreCompact`) only enqueue; reflection itself runs later in the background drain. As of plugin 4.0.0 the drain gates + slices each transcript and invokes `/reflect` on **Sonnet** by default under hard caps. Cost-control env vars consumed by the drain:

| Env var | Default | Effect |
|---|---|---|
| `REFLECT_DRAIN_MODEL` | `sonnet` | Drain model (Opus reserved for escalation + weekly synthesis). |
| `REFLECT_DRAIN_MAX_TURNS` | `8` | Per-`/reflect` turn cap (was 25 pre-4.0.0). |
| `REFLECT_DRAIN_TIMEOUT` | `180` | Per-entry `claude -p` wall-clock cap, seconds (was 600 pre-4.0.0). |
| `REFLECT_DRAIN_TOKEN_MAX` | `2000000` | Post-hoc poison: archive a run reporting more total tokens than this. |
| `REFLECT_DRAIN_CASCADE` | `1` | Gate + slice the transcript before `/reflect` (set `0` to disable). |
| `REFLECT_DRAIN_DEBOUNCE_SEC` | `600` | Min seconds between drain runs. |
| `REFLECT_DRAIN_CWD` | `$HOME` | Neutral cwd for the `claude -p` call. |
| `REFLECT_DISABLED` | unset | Set `1` for a hard kill switch (drain is a no-op). |
| `REFLECT_DRAIN_SKIP_REINDEX` | unset | Set `1` to skip the post-drain reindex (tests). |

Inspect drain spend with `reflect cost`. See the plugin [`CHANGELOG.md`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/plugins/reflect/CHANGELOG.md) for the 4.0.0 cost rearchitecture.

## See also

- [Knowledge base overview](./overview.md)
- [`reflect-kb/README.md`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/reflect-kb/README.md)
- [Docs hub](../README.md)