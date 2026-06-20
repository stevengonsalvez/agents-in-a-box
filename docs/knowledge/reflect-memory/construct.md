---
title: "reflect-memory — the construct"
description: "The capture → index → recall mental model, the two indices (QMD lexical + nano-graphrag vector/graph), and the local vs shared (Postgres) backend."
---

> One loop: **capture** what you teach the agent, **index** it three ways, **recall** the right
> piece into the next session. Everything in the [Recall reference](/knowledge/reflect-memory/recall/)
> is a tuning knob on this loop.

For *why* this exists and how it compares to bare-harness memory, see
[Problem & fit](/knowledge/reflect-memory/problem-and-fit/).

## The loop

```
   ┌─────────┐  signal   ┌──────────┐  markdown   ┌──────────────┐
   │ session │ ───────▶  │ capture  │ ─────────▶  │  KB (notes)  │ ◀── source of truth
   └─────────┘ "no—don't"└──────────┘  + sidecar  └──────┬───────┘
                                                          │ index
                                ┌─────────────────────────┼─────────────────────────┐
                                ▼                          ▼                          ▼
                         ┌────────────┐            ┌──────────────┐           ┌──────────────┐
                         │ QMD (BM25) │            │ vector (ANN) │           │ graph (edges)│
                         │ index.sqlite│           │ nano-graphrag│           │ nano-graphrag│
                         └──────┬─────┘            └──────┬───────┘           └──────┬───────┘
                                └──────────── recall ─────┴──── RRF fuse ────────────┘
                                                          │ rerank · gate · budget
                                                          ▼
                                                   inject into next session
```

| Stage | What happens | Where it lives |
|---|---|---|
| **Capture** | A hook detects a correction/decision signal, slices just the relevant dialogue window, and the drain writes a structured learning (title/category/tags/confidence/problem/fix/rule) plus an entity **sidecar** (`A —[relation]→ B`). Capture summarisation reuses the harness LLM (`claude -p`) — no extra key. | `~/.learnings/` markdown + sidecars |
| **Index** | `reflect reindex` registers each note in **three** arms: BM25 lexical (QMD), vector embedding (nano-graphrag, `all-mpnet-base-v2`), and the entity graph (nano-graphrag). | QMD `index.sqlite` + nano-graphrag store |
| **Recall** | Build a query from project/branch/prompt context, run the arms, **RRF-fuse** the rankings, **cross-encoder rerank**, **OOD-gate** (inject nothing if nothing fits), then **token-budget** pack the survivors into the inject block. | client-side, every session |

The split that makes it cheap and private: **the brain is client-side** (embedding + clustering +
capture LLM all run on your machine / your harness's model), and **the store is dumb** (it indexes
and returns rows — it never calls an LLM).

## The two indices

reflect runs two complementary search engines, fused at recall time:

| Engine | Answers | Backed by | Strength |
|---|---|---|---|
| **QMD** | "what *matches* these words?" | BM25 over `~/.cache/qmd/index.sqlite` | exact terms, file names, error strings |
| **nano-graphrag** | "what's *connected* / what *means* this?" | vector ANN (hnswlib) + entity graph (`.graphml`) | semantics + multi-hop ("what caused X?") |

Neither alone is enough — BM25 misses paraphrase, vectors miss exact tokens, and only the graph
hops to the note you never lexically matched. RRF fusion is what lets all three vote.

## Physical topology

![reflect component topology — harness hooks feed the engine; the markdown KB is the source of truth; the derived store runs either local (QMD sqlite + nano-graphrag hnswlib/graphml) or shared (Supabase Postgres + pgvector)](../../assets/reflect-topology.svg)

## Backend: local or shared (Postgres)

The markdown notes are **always** the local source of truth, and **all LLM / embedding / clustering
always stays client-side — no extra API key**. Only the *derived* vector + graph store has two modes:

| | **Mode 1 — Local** (default) | **Mode 2 — Shared** (Postgres) |
|---|---|---|
| Derived store | per-machine: QMD `index.sqlite` (BM25) + nano-graphrag (hnswlib + `.graphml`) | one **Supabase Postgres** (pgvector) for everyone |
| Setup | nothing | `REFLECT_PG_DSN` + `REFLECT_WORKSPACE_ID` + 2 migrations |
| Share across machines | git-sync the notes, then `reflect reindex` on each machine | automatic — every machine queries the same store |

In Mode 2, nano-graphrag runs **unchanged** — it's handed Postgres-backed storage classes (the same
way it ships `Neo4jStorage`). The DB stays dumb (no LLM/embeddings); tenant isolation is RLS
(fail-closed) + explicit `workspace_id` scoping; writes need a `service_role` DSN.

➡️ **Full backend reference** (schema, setup, threat model, per-harness install):
[`stevengonsalvez/ainb-reflect-memory`](https://github.com/stevengonsalvez/ainb-reflect-memory#readme).

## Next

Each stage above has knobs. The [Recall reference](/knowledge/reflect-memory/recall/) documents every
recall-time feature with a concrete example and the counterfactual — what you'd get without it.
