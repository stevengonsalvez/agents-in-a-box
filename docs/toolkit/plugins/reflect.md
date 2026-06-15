---
title: "reflect"
description: "Claude Code plugin for agent self-improvement: captures corrections as learnings, indexes them, and auto-recalls the relevant ones into every new session."
---

`reflect` is a **Claude Code plugin** — it is loaded by Claude Code (Anthropic's plugin system: skills + hooks + commands), not by the ainb TUI. It captures the corrections and design decisions you make in a session as structured learnings, indexes them into a hybrid GraphRAG + BM25 knowledge base, and auto-injects the most relevant prior learnings into every new session. Philosophy: *correct once, never again; recall everything next time.* Current version: **3.6.0**.

## How it works

![reflect — how it works](../../assets/diagrams/reflect.svg)

The plugin manifest registers five lifecycle hooks. **`SessionStart`** runs two commands: `session_start_recall.py` builds a query from the session's cwd, branch, and recent commits and injects the top-3 reranked learnings as `additionalContext`; and `reflect-drain-bg.sh` launches a detached background drainer that processes any reflections queued from prior sessions. **`UserPromptSubmit`** re-runs recall using the actual prompt as a sharper query (with per-session dedupe), and also completes the mini-learning capture — if a tool just failed and the prompt looks like a correction, it writes a low-confidence learning straight to disk without spending an LLM call.

**`PostToolUse`** is the first half of that cheap capture path: when a tool call fails, it arms a watcher so the next `UserPromptSubmit` can detect a correction. **`Stop`** enqueues short sessions (that ended before compaction) for asynchronous reflection, and **`PreCompact`** enqueues long sessions when Claude Code compacts the conversation — both feed the same `~/.reflect/pending_reflections.jsonl` queue, deduped by `session_id`.

Capture and indexing run through the sub-skills. `/reflect` scans a conversation, classifies corrections vs. successes, and writes a Markdown learning note plus a YAML entity sidecar (people, files, libraries, decisions). The background drainer replays queued transcripts through the same `/reflect` skill headlessly. Everything is dual-indexed into **reflect-kb** — nano-graphrag for semantic + entity-graph search and qmd for fast BM25 lexical search — both running locally.

Retrieval closes the loop: `/reflect:recall` (the same engine the SessionStart and UserPromptSubmit hooks call automatically) runs hybrid search, reranks by confidence × recency × tag overlap, and surfaces the strongest learnings back into the agent's context before the first token is generated.

## What it provides

### Skills

| Skill | Purpose |
|-------|---------|
| `reflect` | Full conversation scan — detect behavioral corrections and knowledge signals, classify them, write a learning note with an entity sidecar for GraphRAG indexing |
| `reflect:recall` | Retrieve relevant prior learnings from the global KB via hybrid vector + graph search, reranked by confidence, recency, and tag overlap (also runs automatically at SessionStart / UserPromptSubmit) |
| `reflect:ingest` | The global knowledge indexer — harvest ALL memory sources across tools (Claude, Codex, Copilot, Gemini) into the unified GraphRAG + qmd KB, archiving originals and generating entity sidecars |
| `reflect:consolidate` | Project-level memory consolidation — merge orphaned worktree memory dirs into a single `.agents/MEMORY.md` (deduplicates and sections; does not index into the global KB) |
| `reflect:errors-ack` | Triage and acknowledge entries in the reflect errors sink (`~/.reflect/errors.json`) — drain poison, parser crashes, ingest failures, hook timeouts; invoked from the statusline ⚠N badge |
| `reflect-status` | Read-only metrics — pending reviews, sidecar coverage, GraphRAG health; can approve/reject pending low-confidence items |

### Hooks (registered in `plugin.json`)

| Event | What fires |
|-------|-----------|
| `SessionStart` | `session_start_recall.py` injects top-3 learnings from cwd/branch/commits, plus a detached `reflect-drain-bg.sh` that drains the pending-reflections queue in the background |
| `UserPromptSubmit` | `user_prompt_submit_recall.py` re-runs recall against the actual prompt (deduped) and writes a mini-learning if a prior tool failure is being corrected |
| `PostToolUse` | `posttooluse_minilearning.py` arms a mini-learning watcher when a tool call fails (cheap, no LLM call) |
| `Stop` | `stop_reflect.py` enqueues short sessions for asynchronous reflection, deduped against PreCompact |
| `PreCompact` | `precompact_reflect.py --auto --verbose` enqueues the transcript for reflection when Claude Code compacts the conversation |

## Install

Reflect is two layers: the plugin (hooks + skills) and the `reflect-kb` CLI (recall/search + qmd + nano-graphrag). `claude plugin install` only does the first, so install both — the one-step path uses `ainb`:

```bash
# 1. plugin: hooks + skills — install NATIVELY per harness.
#    All three read the same plugins/reflect/ dir (distinct manifests, no conflict).
# Claude Code:
claude plugin marketplace add stevengonsalvez/agents-in-a-box
claude plugin install reflect@agents-in-a-box
# GitHub Copilot CLI (subdirectory install — auto-wires hooks + skills):
copilot plugin install stevengonsalvez/agents-in-a-box:plugins/reflect
# OpenAI Codex CLI:
codex plugin marketplace add stevengonsalvez/agents-in-a-box
codex plugin add reflect@agents-in-a-box

# 2. everything else in one shot — auto-installs reflect-kb[graph] via uv,
#    and prints any missing system tools (bash>=4, coreutils, jq) for you to run
ainb reflect bootstrap

# 3. verify — dependency check classified by what needs each tool
ainb doctor
```

`ainb reflect bootstrap` is **hybrid**: it auto-installs the reflect-owned layer (`reflect-kb[graph]`) and only *prints* the `brew`/`apt` commands for system tools, so it never mutates your OS or PATH. The manual equivalent (with annotated commands + the nano-graphrag `--no-deps` caveat) lives in [`plugins/reflect/README.md`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/plugins/reflect/README.md#install).

### One plugin dir, three native installs

All three harnesses now have native plugin runtimes, and `plugins/reflect/` is a valid plugin for each — the manifests live at distinct paths and reference distinct hooks files, so they coexist without conflict:

| Harness | Manifest | Hooks file | Hook shape | Install |
|---|---|---|---|---|
| Claude Code | `.claude-plugin/plugin.json` | inline in manifest | PascalCase, `${CLAUDE_PLUGIN_ROOT}` | `claude plugin install reflect@agents-in-a-box` |
| GitHub Copilot | `plugin.json` (root) | `copilot-hooks.json` | camelCase + `version:1`, `${PLUGIN_ROOT}` | `copilot plugin install stevengonsalvez/agents-in-a-box:plugins/reflect` |
| OpenAI Codex | `.codex-plugin/plugin.json` | `codex-hooks.json` | PascalCase, `${PLUGIN_ROOT}` | `codex plugin add reflect@agents-in-a-box` |

Shared (read-only) across all three: `skills/`, `hooks/` scripts, `scripts/`. No default-discovery hooks file (`hooks.json` / `hooks/hooks.json`) exists, so no harness auto-loads the wrong-format file. The Python adapters under `plugins/reflect/adapters/<codex|copilot>/` remain as a non-marketplace fallback (the role bootstrap used to play). Copilot fires the hooks but does not auto-inject `sessionStart` `additionalContext` into the model, so on Copilot recall is surfaced via manual `/recall`.

## Using it

- **Mostly automatic.** Once installed, the SessionStart hook fires `recall` at the start of every new session and surfaces relevant prior learnings before you type. UserPromptSubmit sharpens that with the actual prompt on each turn.
- **Capture happens for free.** Failed tool calls + correction-shaped follow-ups become low-cost mini-learnings, and short / compacted sessions are queued and drained in the background — no manual step required.
- **`/reflect`** — run it explicitly to scan the current conversation and write a learning note when you want to capture something deliberately.
- **`/reflect:recall "<anything>"`** — query the knowledge base on demand.
- **`/reflect:ingest`** — periodically bulk-index existing memories from any tool into the global KB.
- **`/reflect:consolidate`** — merge scattered worktree memory dirs into one project `.agents/MEMORY.md`.
- **`/reflect-status`** — view pipeline metrics, pending reviews, and KB health; approve/reject low-confidence items.
- **`/reflect:errors-ack`** — triage the errors sink when the statusline shows a ⚠N badge.

## Source

`plugins/reflect/` — Claude Code plugin (skills + lifecycle hooks) backed by the `reflect-kb` GraphRAG + qmd library. Diagram generated via /fireworks-tech-graph.
