---
title: "reflect-memory — the problem & where it fits"
description: "Context engineering, not a bigger instructions file: why front-loading CLAUDE.md/AGENTS.md hits a wall, how reflect captures and recalls the right knowledge at the right time, and how it compares to Mem0, Hindsight, ByteRover, claude-mem, agentmemory, Honcho and OpenViking."
---

> **reflect-memory** captures what you teach a coding agent — corrections, decisions, the architecture
> reasons you keep re-explaining — and recalls the *right* piece into the *right* moment of the next
> session. Across machines and harnesses, reusing the agent's own model, with **no extra API key**.

![Timeline of one coding session and where reflect operates — recall injects prior learnings at SessionStart and each prompt; signals + capture write out at tool calls, Stop and PreCompact; the index closes the loop to the next session](../../assets/reflect-session-timeline.svg)

## The real problem: context engineering, not a bigger file

Every harness gives you one persistent memory primitive: a **static instructions file you maintain by
hand** — `CLAUDE.md`, `AGENTS.md`, `MEMORY.md`, `copilot-instructions.md`. It is front-loaded into the
context window at the start of every session.

That has a hard ceiling. The context window is finite and **shared with the actual task**. Every line
you front-load "just in case" is bloat the moment this particular session doesn't need it — and it
crowds out the files, diffs, and reasoning the task *does* need.

```
   context window  =  [ task: files · diffs · plan · tool output ]  +  [ memory ]
                                                          ▲                    ▲
                            this is the work ─────────────┘                    │
                            front-loaded "just in case" — bloat when ──────────┘
                            irrelevant, and you can only fit so much
```

So a bigger instructions file is the wrong axis. The questions that actually matter:

- A correction you made three weeks ago ("don't bump the shared proto without regenerating clients")
  — how does it reach the agent *only* in the session that's about to touch that proto?
- The architecture *reason* behind a non-obvious choice ("the double `recalcTax` call fixes an EU-VAT
  rounding bug") — how is it recalled when someone questions that code, and stays invisible otherwise?
- A hard-won veering ("we migrated off JWT to server-side sessions in June") — how does it override the
  stale fact, instead of sitting in a file nobody re-reads?

The goal isn't to store *more* — it's to store the signal-bearing moments and **query the right one at
the right time**, spending context only when it pays. That is a retrieval problem, not a file-size
problem. It's why a class of "agent memory" products exists.

## The memory-product landscape — and where each one pinches

Several tools attack this. They differ on four axes that decide whether they actually fit a
coding-agent workflow:

1. **Do they store everything, or selectively?** Whole-session / fire-hose capture is useful but
   accretes noise and unbounded data over time.
2. **Do they need a separate LLM/embedding key?** Your coding agent already has a model+subscription.
   Most memory tools make you configure and *pay for a second provider* just for memory.
3. **What infrastructure do they run?** A always-on server (Postgres, Redis, a Rust/Bun daemon, a
   vector DB) is operational weight — and often cloud-dependent.
4. **Do they capture coding signals as first-class events?** Corrections, test outcomes, git events,
   skill upgrades — captured *structurally*, or only as prose an LLM might (or might not) extract?

```
        store selectively    no extra key     local, no server    first-class signals
reflect       ✓                  ✓                  ✓                    ✓
others        varies          mostly ✗           mostly ✗           ✗ (probabilistic)
```

| Tool | What it stores | Extra LLM/embed key? | Runs as | First-class coding signals | License |
|---|---|---|---|---|---|
| **reflect** | **selective** learnings; markdown = source of truth | **No** — reuses the agent's own model + local embeddings | **local files** (sqlite + graphml); optional shared Postgres | **Yes** — corrections, tests, tool-loops, git, todos, permissions, contradictions, skill-refresh | MIT |
| Hindsight | LLM-extracted facts + mental models | Yes for writes¹ | local daemon → Docker (FastAPI+Postgres) → cloud | No — extracted from prose (skill-capture is a logged bug) | MIT |
| Mem0 | LLM-extracted facts | **Yes** — OpenAI by default at ingest | library → Docker (Postgres+Neo4j); graph = Pro $249/mo | No — hooks, but no correction/git/test capture | Apache-2.0 + SaaS |
| ByteRover | curated markdown tree | curation makes its own LLM calls | local files + node daemon; optional cloud sync | No — curation is agent-*directed*, not passive | Elastic 2.0 (not OSS) |
| claude-mem | every tool call → compressed observations | **No** — reuses Claude auth + local embeddings | always-on Bun daemon + optional ChromaDB | No — probabilistic Haiku extraction | Apache-2.0 |
| agentmemory | **fire-hose** — every tool call, verbatim | optional (value degrades without) | always-on Rust daemon (4 ports) | No — raw events; little structure without LLM | Apache-2.0 |
| Honcho | user/peer models (theory-of-mind) | Yes (self-host); cloud is per-token | Postgres + Redis + deriver worker; or cloud | No — built for end-user personalization, not coding | AGPL-3.0 |
| OpenViking | LLM-extracted memories/skills (tiered) | **Yes** — OpenAI/Volcengine by default | Rust+Go+C++ server, always-on | No — git/test/skill not first-class | AGPL-3.0 |

¹ Hindsight's `retain` needs an LLM; its "reuse your Claude subscription" loopback is documented as
**personal-use-only per Anthropic's terms** — not shippable. See the
[adopt-vs-build critique](https://github.com/stevengonsalvez/ainb-reflect-memory#why-build-not-adopt).

The pattern: the tools that *don't* need a second key (claude-mem) tend to **store everything and grow
noisy**; the tools that *do* curate well (Hindsight, Mem0, OpenViking) make you **stand up a server and
pay a second provider**; and **none** of them capture corrections, test outcomes, git events, or skill
upgrades as typed signals — they hope an LLM extracts them from the transcript.

## Where reflect fits

reflect is the row that holds all four columns at once. Its design line: **the brain is client-side,
the store is dumb, and the signals are typed.**

```
   harness hooks ──▶ reflect (capture)  ──▶ markdown KB (source of truth)
        ▲              ▲ typed signals              │
        │              │ (corrections, tests,  index (QMD + nano-graphrag)
   harness LLM  ◀── reflect (recall) ◀──── git, skills, contradictions)
   (capture reuses the agent's OWN model — no extra key, no separate sub)
```

| reflect's answer | to the problem |
|---|---|
| **Selective capture** — only signal-bearing moments become learnings; TTL, dedup and contradiction-handling built in | whole-session noise + unbounded growth |
| **Reuses the agent's own model** (`claude -p`) + a local embedding model | no second API key, no separate subscription, no ToS loophole |
| **Local files** (QMD sqlite + nano-graphrag graphml) — optional shared Postgres only if you *want* cross-machine | no mandatory server / cloud dependency |
| **First-class typed signals** — corrections, test pass/fail, tool-loops, git commit/revert, todo completions, permission replies, cross-turn contradictions, idle sweeps, plus auto **skill-refresh** | the gap every other tool leaves to probabilistic extraction |
| **Cross-harness by design** — Codex writes a learning, a later Claude session reads it | single-harness / MCP-bolted memory |
| **Markdown source of truth**, volatile signals in a DB sidecar → clean git diffs, reviewable in a PR | opaque DB/vector blobs |

The honest line: on the *no-extra-key* axis alone, claude-mem ties reflect (it also reuses Claude's
auth). reflect's separation is the **combination** — selective + no-key + local + typed-signals +
cross-harness + MIT — which no single competitor matches.

## Next

| Read this | For |
|---|---|
| [Construct](/knowledge/reflect-memory/construct/) | the capture → index → recall mental model, and local vs shared (Postgres) backend |
| [Recall reference](/knowledge/reflect-memory/recall/) | every recall feature with an example and what breaks without it |
| [Hooks & platform](/knowledge/hooks-and-platform/) | exactly which hook fires what, per harness |
