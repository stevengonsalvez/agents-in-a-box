---
title: "reflect-memory — the problem & where it fits"
description: "What Claude Code, Codex, and Copilot give you out of the box, the memory gap they all share, and how reflect-memory fills it without an extra API key."
---

> **reflect-memory** is a cross-harness memory layer for AI coding agents: it captures what you
> teach the agent, indexes it (semantic + lexical + graph), and recalls the right prior art into
> the next session — across machines, with **no extra LLM or embedding key**.

This page is the newcomer's entry point: the problem, what your harness already does, and the gap
reflect fills. For the mental model see [Construct](/knowledge/reflect-memory/construct/); for the
full per-feature reference see [Recall reference](/knowledge/reflect-memory/recall/).

## The problem: every session starts amnesiac

```
Day 1   you: "regen the gRPC clients after editing payments.proto — broke staging"
        agent: fixes it ✓
   ...new session, 3 weeks later, different machine, maybe a teammate...
Day 22  agent: edits payments.proto, skips regen ──▶ staging breaks again ✗
```

Out of the box, an agent's only persistent memory is a **static instructions file you maintain by
hand** (`CLAUDE.md`, `AGENTS.md`, `copilot-instructions.md`). It does not *learn* from a session,
it cannot *search* what happened in past sessions by meaning, and it never *connects* one fact to
another. So you re-fix the same bug, re-explain the same convention, and re-research the same
problem — the cost the agent was supposed to remove.

reflect's promise is the inverse: **correct once, never again.**

## What each harness gives out of the box

```
┌──────────────┐   ┌──────────────┐   ┌──────────────┐
│ Claude Code  │   │  Codex CLI   │   │   Copilot    │   ◀── static file
│  CLAUDE.md   │   │  AGENTS.md   │   │ *-instr.md   │      you hand-edit
└──────┬───────┘   └──────┬───────┘   └──────┬───────┘
       └──────────────────┼──────────────────┘
                          ▼
              ┌───────────────────────┐
              │     reflect-memory     │  ◀── auto-capture + semantic/
              │  capture·index·recall  │      lexical/graph recall,
              └───────────────────────┘      shared across all three
```

| Capability | Claude Code | Codex CLI | Copilot | **reflect-memory** |
|---|:---:|:---:|:---:|:---:|
| Persistent static instructions | `CLAUDE.md` | `AGENTS.md` | `*-instructions.md` | uses them **+** a live KB |
| Maintained by | you, by hand | you, by hand | you, by hand | **auto-captured** from sessions |
| Auto-capture corrections → structured learnings | ✗ | ✗ | ✗ | ✓ |
| Semantic (vector) recall of past learnings | ✗ | ✗ | ✗ | ✓ |
| Lexical / BM25 exact-term recall | ✗ | ✗ | ✗ | ✓ |
| Graph (multi-hop) recall — "what caused / depends on X" | ✗ | ✗ | ✗ | ✓ |
| Recency / temporal awareness | ✗ | ✗ | ✗ | ✓ |
| Relevance gate (inject nothing when nothing fits) | ✗ | ✗ | ✗ | ✓ |
| Shared across **harnesses** (Codex writes, Claude reads) | ✗ | ✗ | ✗ | ✓ |
| Shared across **machines** | ✗ | ✗ | ✗ | ✓ (Postgres backend) |
| Extra API / embedding key for memory | — | — | — | **none** (local model) |
| Hook integration | native | 0.129+ hooks | native hooks¹ | wires all three |

¹ Copilot drops `userPromptSubmitted` hook output, so per-prompt recall there is manual `/recall`;
SessionStart auto-recall works. See [Hooks & platform](/knowledge/hooks-and-platform/).

The static file isn't useless — it's the right place for *rules you already know you want every
time*. The gap is everything you **discover mid-session**: that knowledge has nowhere to go and no
way back. reflect is that path.

## Where reflect fits

```
   harness hooks ──▶ reflect (capture)  ──▶ markdown KB (source of truth)
        ▲                                         │
        │                                   index (QMD + nano-graphrag)
   harness LLM  ◀── reflect (recall) ◀────────────┘
   (capture reuses your harness's own model — no extra key)
```

| It plugs into | How |
|---|---|
| Your harness's hooks | SessionStart recall, Stop/PostToolUse capture, PreCompact flush |
| Your harness's own LLM | capture summarisation runs via `claude -p` — no new provider, no new key |
| A local embedding model | `all-mpnet-base-v2` on CPU/MPS for vector recall — no cloud call |
| Your existing files | learnings are plain markdown; `CLAUDE.md`/`AGENTS.md` stay as-is |

The deliberate design line: **the brain is client-side, the store is dumb.** reflect never adds an
LLM provider you have to configure — it borrows the one your harness already authenticated.

## Next

| Read this | For |
|---|---|
| [Construct](/knowledge/reflect-memory/construct/) | the capture → index → recall mental model, and local vs shared (Postgres) backend |
| [Recall reference](/knowledge/reflect-memory/recall/) | every recall feature with an example and what breaks without it |
| [Hooks & platform](/knowledge/hooks-and-platform/) | exactly which hook fires what, per harness |
