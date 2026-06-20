---
title: "reflect-memory — recall reference (all 57 ports)"
description: "Every reflect 4.1.0 recall feature as a scannable table — flag, what it does, an example, and what you'd get without it — plus the full 57-port catalogue."
---

> "Correct once, never again." This is the reference half of reflect-memory: first one learning's
> whole journey (the newcomer's mental model), then **every** ported feature as a table — flag, what
> it does, an example, and the counterfactual: what you'd get if it didn't exist.

For the architecture see [Construct](/knowledge/reflect-memory/construct/); for the problem and the
bare-harness comparison see [Problem & fit](/knowledge/reflect-memory/problem-and-fit/).

:::note[Examples are illustrative]
The `reflect search …` examples and their results below are representative, hand-authored to show
the behaviour — not pasted from a specific live KB run. Every feature is backed by a behavioural
proof under [`tests/eval/behavioral/proofs/`](https://github.com/stevengonsalvez/ainb-reflect-memory/tree/main/tests/eval/behavioral/proofs)
that exercises it with the knob **on and off**.
:::

## Memory end to end — one learning's journey

Follow one correction from the moment it happens to the moment it saves you weeks later.

**1 · Capture.** Mid-session you tell the agent: *"no — don't bump the shared `payments.proto`
without regenerating the clients, it broke staging last time."* A `PostToolUse`/`Stop` hook detects
the correction signal (the *"no — don't…"* shape), slices just the relevant dialogue window (not the
whole 100k-token transcript), and the drain writes a structured learning:

```yaml
---
title: "Regenerate gRPC clients after editing payments.proto"
category: reliability
tags: [grpc, proto, payments, codegen]
confidence: 0.8
project_id: billing-svc
problem: "Bumped payments.proto without regenerating clients"
fix: "Run `make proto-gen` after any .proto edit; CI now gates on it"
rule: "Never ship a .proto change without regenerated clients"
---
```
Alongside it, an entity sidecar records `payments.proto —[prevents]→ staging outage`.

**2 · Index.** `reflect reindex` embeds the note (vector arm), adds its entities + edges to the
GraphRAG graph (graph arm), and registers it in the BM25 index (QMD arm). It's now reachable three
different ways.

**3 · Recall — three weeks later, a different session.** A new teammate's agent opens `billing-svc`
and is about to edit `payments.proto`. **SessionStart** fires, builds a query from the project +
branch context, and runs hybrid recall:

- the **vector arm** matches on *proto / payments / codegen* meaning,
- the **BM25 arm** matches the literal `payments.proto`,
- the **graph arm** hops the `prevents` edge to the staging-outage context,
- **RRF** fuses the three rankings, the **cross-encoder** reranks, the **OOD gate** confirms it's
  genuinely relevant, and the **token budget** packs it into the inject block.

Before the agent writes a single line, it sees: *"Regenerate gRPC clients after editing
payments.proto — broke staging last time; run `make proto-gen`."* The mistake never happens twice.

That whole chain — capture → index → fuse → rerank → gate → inject — is what the 57 ports tune.

## The 57 ports at a glance

```
            ┌──────────────────────── recall time ───────────────────────┐
 query ─▶ [arms] ─▶ RRF fuse ─▶ [rerank] ─▶ [gate] ─▶ [boosts] ─▶ budget ─▶ inject
            R1·R5·R6           R2·R3        R7·R12      R8·R16
            ─────────────────── scope: R15·A6 · modes: M1·R10·R11 ────────────────
```

| Group | Ports | Below |
|---|---|---|
| Retrieval arms | R1 R5 R6 R2 R3 R4 | [↓](#retrieval-arms) |
| Relevance gates | R7 R12 | [↓](#relevance-gates) |
| Ranking & affinity boosts | R8 R16 S3 S4 | [↓](#ranking--affinity-boosts) |
| Scope: sharding & isolation | R15 A6 | [↓](#scope-sharding--isolation) |
| Recall modes & inject | M1 R10 R11 M7 M4 O3 A1 R20 | [↓](#recall-modes--inject) |
| Caching, dedup & negative-recall | R9 S7 C1 A3 SG6 S1 | [↓](#caching-dedup--negative-recall) |
| Capture · storage · consolidation · team (the other 29) | S/M/A/C/O/SG/R series | [↓](#the-other-29--capture-storage-consolidation-team) |

All 57: **R1–R16 + R20** (17 retrieval/recall), **M1–M8** (8 modes), **S1–S10** (10
capture/storage), **A1–A6** (6 advanced), **C1–C5** (5 consolidation), **O1–O3** (3 observations),
**SG1–SG8** (8 signals). (There is no R17–R19 — the R-series jumps R16 → R20.)

---

## Retrieval arms

The parallel signals that find candidates, plus the post-fusion shapers that order them. Marked ⭑
features have a [worked example](#worked-examples) below.

| Feature | Flag (default) | What it does · example | Without it |
|---|---|---|---|
| **R1 ⭑ Graph-expansion arm** | `RECALL_GRAPH_ARM` (on) | A 3rd arm walks the entity graph and fuses notes you never matched lexically. *"why does checkout call `recalcTax` twice?"* → hops `caused_by` to the EU-VAT rounding note. | Only the lexically-matching note; you "fix" the perf and re-introduce the rounding bug. |
| **R5 ⭑ Temporal arm** | `RECALL_TEMPORAL_ARM` (on) | When a date phrase is found (R6), a 4th arm fuses notes whose timestamp falls in the window. *"what did we decide last week about auth"*. | In-window notes get crowded out by topical arms; recent decisions don't surface. |
| **R6 Query-time date parsing** | `RECALL_TEMPORAL` (on) | Regex-extracts "last week" / "in march" / "since 2026-01-01" into a real date range that feeds R5. | "last week" is just two more keywords; temporal intent silently dropped. |
| **R2 ⭑ Cross-encoder rerank** | `RECALL_CROSS_ENCODER` (on); `REFLECT_CE_MODEL`, `RECALL_CE_TIMEOUT` | Re-reads the top-20 jointly with the query (local MiniLM CE) and re-sorts by *meaning*. *"flaky test in the auth suite"* lifts the real answer over a keyword-similar "auth token format". | The keyword-similar-but-wrong note wins rank 1; right prior art misses a tight budget. |
| **R3 ⭑ MMR diversity** | `RECALL_MMR` (on) / `--no-mmr`; `RECALL_MMR_LAMBDA` (0.7) | Final top-k via Maximal Marginal Relevance — de-clusters near-duplicate notes. *"nginx 502 under load"* surfaces the lone "enable keepalive" note past 4 "raise worker_connections" twins. | Top-k is 4 copies of one idea; the second, correct idea sits at rank 6, never injected. |
| **R4 Token-budget retrieval** | `--max-tokens N` (0=off); `REFLECT_RECALL_MAX_TOKENS` | Packs ranked notes by estimated tokens until the budget is hit, instead of a fixed top-k. *`reflect search "deploy steps" --max-tokens 1500`*. | A fixed top-k blows the window on a verbose corpus; long notes evict the user's own files. |

## Relevance gates

| Feature | Flag (default) | What it does · example | Without it |
|---|---|---|---|
| **R7 ⭑ OOD relevance gate** | `--min-overlap` / `REFLECT_RECALL_MIN_OVERLAP` | Measures query-term coverage of the top hit; below threshold it injects **nothing** (`ood_gated`). *`--min-overlap 0.3`* in a fresh repo. | Every session gets the least-bad junk; the agent learns to ignore the inject block. |
| **R12 Per-arm calibrated thresholds** | `RECALL_ARM_<NAME>_MIN_SCORE` (0=off); seed via `reflect calibrate-thresholds` | A per-arm floor applied *before* RRF (arm scores aren't comparable). *`RECALL_ARM_BM25_MIN_SCORE=0.15`* drops weak BM25 hits without nuking strong graph hits. | One global cutoff either lets BM25 noise through or starves the graph arm. |

## Ranking & affinity boosts

Secondary signals that break ties — each bounded so it can nudge, never hijack.

| Feature | Flag (default) | What it does · example | Without it |
|---|---|---|---|
| **R8 Bounded multiplicative boosts** | `RECALL_CONFIDENCE_ALPHA` `RECALL_RECENCY_ALPHA` `RECALL_TAG_ALPHA` `RECALL_PROOF_ALPHA` (0.2/0.2/0.2/0.1) | Each signal applied as `1+α·(norm−0.5)`, clamped to ±α/2 — recency/confidence/tags/proof break ties only. | Unbounded boosts let "most recent" bury a 2-year-old note that perfectly answers the query. |
| **R16 Project-affinity boost** | `RECALL_PROJECT_ALPHA` (0.2) | Under `--global`, current-project notes get a capped +10% lift over equally-relevant foreign ones. | Cross-project recall treats every project equally; a foreign note outranks your own. |
| **S3 Numeric confidence ranking** | `RECALL_CONFIDENCE_ALPHA`; `--field confidence_num` | Stores continuous 0–1 confidence as the canonical ranking value; HIGH/MED/LOW are display buckets. | Coarse tiers can't separate two "HIGH" notes; ranking loses a real signal. |
| **S4 Provenance / proof-count** | `RECALL_PROOF_ALPHA` (0.1); `--field proof_count` | First-class `proof_count` provenance nudges ranking ±5% and is projectable. | A note proven 12 times ranks identically to an unverified one. |

## Scope: sharding & isolation

| Feature | Flag (default) | What it does · example | Without it |
|---|---|---|---|
| **R15 Per-project sharding** | `--global` / `RECALL_GLOBAL`; `RECALL_LEARNINGS_ROOT` | Each project has its own nano-graphrag shard; recall defaults to the current project's. `--global` unions across all. | Every project's recall is polluted by every other's; the relevant local note drowns. |
| **A6 Branch-aware isolation** | `RECALL_BRANCH` / `--all-branches` / `RECALL_ALL_BRANCHES` | Within a project, each git branch/worktree gets a sub-shard; recall pins to the current branch. | A speculative note from an abandoned `feat/y` surfaces as fact while you work `feat/x`. |

## Recall modes & inject

| Feature | Flag (default) | What it does · example | Without it |
|---|---|---|---|
| **M1 ⭑ Staged 3-layer recall** | `recall_stages`: `reflect index …` → `reflect hydrate <id…>` | Index-then-hydrate: returns token-capped ID-only rows; the agent hydrates only the ids it wants. | Every recall pays full-body cost for every candidate; deep digs become token-prohibitive. |
| **R10 3-tier hierarchical inject** | `REFLECT_TIERED_INJECT` (opt-in); `REFLECT_SKILL_TIER_MIN_SCORE` (2.0) | SessionStart consults curated skills first; a strong skill hit is injected and raw recall skipped. | Every session runs full raw recall even when a promoted skill already has the answer. |
| **R11 Forced-grounding short-circuit** | (R10 freshness gate) | If the tier-1 skill hit is fresh **and** high-confidence, SessionStart emits just that and never spawns the recall subprocess. | Warm-project boots needlessly spawn the full pipeline — slow and noisy. |
| **M7 Knowledge-corpus Q&A** | `reflect corpus build <name> --tag …` | Snapshots a filtered KB subset into `corpora/<name>.json` for a primed, deterministic Q&A scope. | No way to pin recall to a curated subset; every query hits the whole corpus. |
| **M4 Pluggable-mode inheritance** | `REFLECT_MODE` (`engineering`); `REFLECT_MODES_DIR` | Loads taxonomy + prompt templates from a mode JSON (deep-merge inheritance); drives learning types + economics glyphs. | One hard-coded taxonomy; research/writing workflows can't retune what's captured/surfaced. |
| **O3 Persona / preference answer** | always-on (`project_persona`) | A high-confidence distilled field (e.g. `testing_style='TDD'`) answers an open-domain query directly. *"what testing style does this project use?"* | The question falls through to generic recall; a known team fact isn't answered crisply. |
| **A1 Pinned editable memory slots** | `REFLECT_SLOTS` (opt-in) | A pinned scratchpad slot per (project, name) injected at Tier-0, ahead of skills/recall, regardless of ranking. | No way to force an always-present note; critical context depends on it ranking well. |
| **R20 Skills-index query** | always-on (built in `reflect.db`) | A queryable sqlite index of installed skills (name/tags/summary) replaces per-query SKILL.md scanning; feeds R10/R11. | Tiered inject rescans every SKILL.md per query; slower, and never matches an unindexed skill. |

## Caching, dedup & negative-recall

| Feature | Flag (default) | What it does · example | Without it |
|---|---|---|---|
| **R9 Fuzzy cache tier** | `RECALL_FUZZY_CACHE` (on); `RECALL_FUZZY_THRESHOLD` (0.85) | A reworded repeat within the Jaccard threshold is served from cache — skips embed+graph+rerank. | Every rephrasing pays the full retrieval cost; a debugging back-and-forth re-runs it dozens of times. |
| **S7 Chunk-hash dedup** | always-on (`chunk_hashes`) | Slice-chunk hashing at drain so re-draining the same transcript doesn't duplicate the learning. | Recall returns two copies of the same lesson; duplicates crowd the top-k. |
| **C1 Semantic-dedup adjudication** | `REFLECT_DEDUP_THRESHOLD` (0.97; ≥1.0 disables) | Before a CREATE lands, an embedding-cosine twin ≥ threshold is held as a "merge?" adjudication. | Near-identical phrasings accumulate; the KB bloats with restatements of one idea. |
| **A3 `forget_after` TTL prune** | per-row `forget_after` (hourly sweep) | Expired learnings are archived and moved to `.forgotten/`; permanent/future ones survive. | Stale, time-boxed notes linger forever and keep surfacing past their relevance. |
| **SG6 Knowledge-gap signal** | `RECALL_GAP_LOG` (on) / `--no-gap-log` | A 0-result recall logs `{query, normalized, session_id}` to `knowledge-gaps.jsonl` as a curation backlog. | Misses vanish silently; you never learn what the KB *should* have known. |
| **S1 Structured field extraction** | `--field NAME` | Projects a single typed field (`rule`/`fix`/`problem`/…) instead of the whole note. *`--field rule`*. | Every hit returns the full note body; context-expensive when you only need the rule. |

---

## Worked examples

Illustrative command → output for the marquee arms (see the note at the top — representative, not a
live run).

### R1 · graph-expansion
```
$ reflect search "why does checkout call recalcTax twice"
✓ recalcTax is idempotent but expensive            (vector + bm25)
✓ double-call fixes an EU-VAT rounding bug (a1b2c3) (graph: caused_by hop)   ← never matched lexically
```

### R2 · cross-encoder rerank
```
$ reflect search "flaky test in the auth suite"
  RRF rank 1:  auth token format …            ← keyword-heavy, wrong
→ CE rank 1:  auth integration test flaky under parallel xdist   ← answers the question
```

### R5 · temporal arm
```
$ reflect search "what's our current API auth"
✓ migrated to server-side sessions (Jun)   ← temporal arm lifts recent
  we use JWT (Apr)                          ← older, more-cited, demoted
```

### R7 · OOD gate
```
$ reflect search "totally unrelated topic" --min-overlap 0.3
∅ ood_gated — top hit overlap 0.08 < 0.3 → injected nothing (no least-bad junk)
```

### M1 · staged recall
```
$ reflect index "tokio panic on shutdown"        # token-capped id+title rows
  [a17] Graceful tokio shutdown ordering   score 0.82
  [c44] Abort vs cancel on JoinHandle      score 0.71
$ reflect hydrate a17 c44                          # full bodies only for what you picked
```

---

## The other 29 — capture, storage, consolidation, team

Not query-time features, but the plumbing that fills and maintains the KB the recall arms read.
Listed for completeness so the full 57 are accounted for.

| Feature | Category | Flag (default) | What it does |
|---|---|---|---|
| **M2 Writer-output classifier + breaker** | capture | `REFLECT_DRAIN_INVALID_THRESHOLD` (3) | Kills + archives a drifting/poisoned writer after N bad outputs. |
| **M3 Quota-aware writer abort** | capture | `REFLECT_DRAIN_DAILY_MAX` / `REFLECT_QUOTA_GATE` | Defers the whole drain queue when the daily LLM gate is closed, instead of burning the cap. |
| **M5 Commit-reference verification** | capture | always-on | Checks every cited commit hash against the repo; rejects all-fabricated notes, flags partials. |
| **M6 Private-tag strip** | capture | always-on | Strips `<private>` spans at the LLM-prompt boundary so they never reach the writer/index. |
| **M8 Token-economics surfacing** | dashboard | `RECALL_ECONOMICS` (on) | Annotates each result with discovery/read tokens + savings % and a mode glyph. |
| **S2 Typed causal-link enum** | capture | always-on | Closed enum for sidecar relations (`caused_by/causes/enables/prevents/contradicts/supersedes/part_of/uses`). |
| **S5 Belief-revision on ingest** | capture | always-on | Runs CREATE/UPDATE/DELETE against `reflect.db` so new learnings revise prior beliefs. |
| **S6 History snapshot on update** | capture | always-on | Snapshots the prior form into `learning_history` before mutating a live row. |
| **S8 Doc→chunk→learning grouping** | capture | always-on | Persists each learning's lineage back to its source transcript + chunk. |
| **S9 Volatile-signals sidecar** | capture | always-on | Moves churning signals (recall_count, helpful_count…) out of note markdown into a DB sidecar — clean git diffs. |
| **S10 Write-validate-retry loop** | capture | always-on (3 attempts) | Validates structure + sidecar after write; re-prompts; flags unfixable notes `validated: false`. |
| **R13 Auto skill-refresh trigger** | capture | always-on | Flags an existing skill for refresh when a learning it covers lands. |
| **R14 Per-skill staleness signal** | signal | `REFLECT_STALENESS_DAYS` (30) | Marks a skill `is_stale` when an in-scope learning changed after its last refresh. |
| **SG1 Cross-turn contradiction** | capture | always-on | Detects + reconciles contradicting learnings at capture (sets `is_latest`). |
| **SG2 Git-event capture** | capture | always-on | Links commits↔sessions; demotes a reverted commit's learnings on `git revert`. |
| **SG3 Idle-sweep trigger** | signal | `REFLECT_IDLE_THRESHOLD_SEC`; `RECALL_SPECULATIVE_ALPHA` (0.2) | Idle timer sweeps quiet transcripts into speculative learnings (down-ranked at recall). |
| **SG4 Test-outcome parsing** | signal | always-on | Parses pass/fail from Bash output in PostToolUse into a capture signal. |
| **SG5 Tool-loop detection** | signal | always-on | Detects repeated/oscillating tool-call loops as a signal. |
| **SG7 TodoWrite completion signal** | signal | always-on | Emits a "how I did X" candidate when a todo flips to `completed`. |
| **SG8 Permission-reply capture** | signal | always-on | Captures permission-prompt allow/deny replies as policy learnings. |
| **A2 Bitemporal graph edges** | infra | always-on | Edges carry `tcommit` / `tvalid` / `tvalid_end` — "what was true" vs "what we knew" stay separable. |
| **A4 Followup-rate diagnostic** | dashboard | `RECALL_FOLLOWUP` (on) | Logs a recall-quality verdict (did the user immediately re-query differently?) to metrics. |
| **A5 Synthetic compression fallback** | capture | drain `--no-llm` | Builds a structured learning from heuristics alone when the drain LLM is unavailable. |
| **C2 Auto-consolidation threshold** | consolidation | `REFLECT_SYNTHESIS_AUTO_THRESHOLD` (30) | Fires the synthesis pass early once learnings-since-last-consolidation cross the threshold. |
| **C3 Graph maintenance sweep** | consolidation | always-on (every N drains) | Structural rewrite of the local graphml to repair orphan edges after deletes. |
| **C4 Lifecycle events fan-out** | infra | `REFLECT_EVENTS_ON_<EVENT>` | Appends lifecycle events to `events.jsonl` + runs per-event shell hooks (local webhooks). |
| **C5 KB export/import round-trip** | team | `kb_export.py` / `kb_import.py` | Snapshots `documents/` + `reflect.db` into one git-friendly tarball; byte-identical restore elsewhere. |
| **O1 Consolidated observations** | consolidation | always-on | A 2nd drain stream of persona/convention statements that accumulate evidence over time. |
| **O2 Auto-refreshing conventions doc** | consolidation | always-on | Re-renders a conventions markdown doc from accumulated observations each consolidation. |

---

Every feature above is verified by a behavioural proof under
[`tests/eval/behavioral/proofs/`](https://github.com/stevengonsalvez/ainb-reflect-memory/tree/main/tests/eval/behavioral/proofs)
(57 `proof_*.py`, one per port) that demonstrates the behaviour with the knob on **and** off. See the
[`reflect` CLI reference](/knowledge/reflect-cli/) for how to drive recall directly.
