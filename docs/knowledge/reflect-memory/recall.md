---
title: "reflect-memory — recall reference (all 57 ports)"
description: "Every reflect 4.1.0 recall feature as a scannable callout — flag, what it does, an example, and what you'd get without it — plus the full 57-port catalogue."
---

> "Correct once, never again." This is the reference half of reflect-memory: first one learning's
> whole journey (the newcomer's mental model), then **every** ported feature as a callout — flag, what
> it does, an example, and the counterfactual: what you'd get if it didn't exist.

For the architecture see [Construct](/knowledge/reflect-memory/construct/); for the problem and the
landscape comparison see [Problem & fit](/knowledge/reflect-memory/problem-and-fit/) and
[Why build, not adopt](/knowledge/reflect-memory/comparison/).

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
the correction signal, slices just the relevant dialogue window (not the whole 100k-token transcript),
and the drain writes a structured learning:

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
and is about to edit `payments.proto`. **SessionStart** fires, builds a query from project + branch
context, and runs hybrid recall: the **vector arm** matches on meaning, the **BM25 arm** matches the
literal `payments.proto`, the **graph arm** hops the `prevents` edge — **RRF** fuses the rankings,
the **cross-encoder** reranks, the **OOD gate** confirms relevance, the **token budget** packs it in.

Before the agent writes a line, it sees: *"Regenerate gRPC clients after editing payments.proto —
broke staging last time; run `make proto-gen`."* The mistake never happens twice. That whole chain —
capture → index → fuse → rerank → gate → inject — is what the 57 ports tune.

## The 57 ports at a glance

```
            ┌──────────────────────── recall time ───────────────────────┐
 query ─▶ [arms] ─▶ RRF fuse ─▶ [rerank] ─▶ [gate] ─▶ [boosts] ─▶ budget ─▶ inject
            R1·R5·R6           R2·R3        R7·R12      R8·R16
            ─────────────────── scope: R15·A6 · modes: M1·R10·R11 ────────────────
```

- **Retrieval arms** — R1 R5 R6 R2 R3 R4 → [§ below](#retrieval-arms)
- **Relevance gates** — R7 R12 → [§ below](#relevance-gates)
- **Ranking & affinity boosts** — R8 R16 S3 S4 → [§ below](#ranking--affinity-boosts)
- **Scope: sharding & isolation** — R15 A6 → [§ below](#scope-sharding--isolation)
- **Recall modes & inject** — M1 R10 R11 M7 M4 O3 A1 R20 → [§ below](#recall-modes--inject)
- **Caching, dedup & negative-recall** — R9 S7 C1 A3 SG6 S1 → [§ below](#caching-dedup--negative-recall)
- **Capture · storage · consolidation · team** (the other 29) → [§ below](#the-other-29--capture-storage-consolidation-team)

All 57: **R1–R16 + R20** (17 retrieval/recall), **M1–M8** (8 modes), **S1–S10** (10
capture/storage), **A1–A6** (6 advanced), **C1–C5** (5 consolidation), **O1–O3** (3 observations),
**SG1–SG8** (8 signals). (No R17–R19 — the R-series jumps R16 → R20.) ★ = has a
[worked example](#worked-examples) below.

## Retrieval arms

The parallel signals that find candidates, plus the post-fusion shapers that order them.

<div class="rm-cards two">
  <div class="rm-card">
    <div class="h"><span class="id">R1 ★</span> Graph-expansion arm · <code>RECALL_GRAPH_ARM</code> (on)</div>
    <p><span class="lbl">Does</span> A 3rd arm walks the entity graph and fuses notes you never matched lexically. <em>e.g.</em> “why does checkout call <code>recalcTax</code> twice?” → hops <code>caused_by</code> to the EU-VAT rounding note.</p>
    <p class="without"><span class="lbl">Without it</span> Only the lexically-matching note; you “fix” the perf and re-introduce the rounding bug the double-call fixed.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R5 ★</span> Temporal arm · <code>RECALL_TEMPORAL_ARM</code> (on)</div>
    <p><span class="lbl">Does</span> When a date phrase is found (R6), a 4th arm fuses notes whose timestamp falls in the window. <em>e.g.</em> “what did we decide last week about auth”.</p>
    <p class="without"><span class="lbl">Without it</span> In-window notes get crowded out by topical arms; recent decisions don't surface.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R6</span> Query-time date parsing · <code>RECALL_TEMPORAL</code> (on)</div>
    <p><span class="lbl">Does</span> Regex-extracts “last week” / “in march” / “since 2026-01-01” into a real date range that feeds R5.</p>
    <p class="without"><span class="lbl">Without it</span> “last week” is just two more keywords; temporal intent silently dropped.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R2 ★</span> Cross-encoder rerank · <code>RECALL_CROSS_ENCODER</code> (on)</div>
    <p><span class="lbl">Does</span> Re-reads the top-20 jointly with the query (local MiniLM CE) and re-sorts by <em>meaning</em>. “flaky test in the auth suite” lifts the real answer over a keyword-similar “auth token format”.</p>
    <p class="without"><span class="lbl">Without it</span> The keyword-similar-but-wrong note wins rank 1; right prior art misses a tight budget.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R3 ★</span> MMR diversity · <code>RECALL_MMR</code> (on) / <code>--no-mmr</code></div>
    <p><span class="lbl">Does</span> Final top-k via Maximal Marginal Relevance — de-clusters near-duplicates. “nginx 502 under load” surfaces the lone “enable keepalive” note past 4 “raise worker_connections” twins.</p>
    <p class="without"><span class="lbl">Without it</span> Top-k is 4 copies of one idea; the second, correct idea sits at rank 6, never injected.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R4</span> Token-budget retrieval · <code>--max-tokens N</code></div>
    <p><span class="lbl">Does</span> Packs ranked notes by estimated tokens until the budget is hit, instead of a fixed top-k. <code>reflect search "deploy steps" --max-tokens 1500</code>.</p>
    <p class="without"><span class="lbl">Without it</span> A fixed top-k blows the window on a verbose corpus; long notes evict the user's own files.</p>
  </div>
</div>

## Relevance gates

<div class="rm-cards two">
  <div class="rm-card">
    <div class="h"><span class="id">R7 ★</span> OOD relevance gate · <code>--min-overlap</code></div>
    <p><span class="lbl">Does</span> Measures query-term coverage of the top hit; below threshold it injects <strong>nothing</strong> (<code>ood_gated</code>). <code>--min-overlap 0.3</code> in a fresh repo.</p>
    <p class="without"><span class="lbl">Without it</span> Every session gets the least-bad junk; the agent learns to ignore the inject block.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R12</span> Per-arm calibrated thresholds · <code>RECALL_ARM_&lt;NAME&gt;_MIN_SCORE</code></div>
    <p><span class="lbl">Does</span> A per-arm floor applied <em>before</em> RRF (arm scores aren't comparable). <code>RECALL_ARM_BM25_MIN_SCORE=0.15</code> drops weak BM25 hits without nuking strong graph hits. Seed via <code>reflect calibrate-thresholds</code>.</p>
    <p class="without"><span class="lbl">Without it</span> One global cutoff either lets BM25 noise through or starves the graph arm.</p>
  </div>
</div>

## Ranking & affinity boosts

Secondary signals that break ties — each bounded so it can nudge, never hijack.

<div class="rm-cards two">
  <div class="rm-card">
    <div class="h"><span class="id">R8</span> Bounded multiplicative boosts · <code>RECALL_*_ALPHA</code></div>
    <p><span class="lbl">Does</span> Each signal applied as <code>1+α·(norm−0.5)</code>, clamped to ±α/2 — recency / confidence / tags / proof break ties only (defaults 0.2/0.2/0.2/0.1).</p>
    <p class="without"><span class="lbl">Without it</span> Unbounded boosts let “most recent” bury a 2-year-old note that perfectly answers the query.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R16</span> Project-affinity boost · <code>RECALL_PROJECT_ALPHA</code> (0.2)</div>
    <p><span class="lbl">Does</span> Under <code>--global</code>, current-project notes get a capped +10% lift over equally-relevant foreign ones.</p>
    <p class="without"><span class="lbl">Without it</span> Cross-project recall treats every project equally; a foreign note outranks your own.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">S3</span> Numeric confidence ranking · <code>--field confidence_num</code></div>
    <p><span class="lbl">Does</span> Stores continuous 0–1 confidence as the canonical ranking value; HIGH/MED/LOW are display buckets.</p>
    <p class="without"><span class="lbl">Without it</span> Coarse tiers can't separate two “HIGH” notes; ranking loses a real signal.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">S4</span> Provenance / proof-count · <code>RECALL_PROOF_ALPHA</code> (0.1)</div>
    <p><span class="lbl">Does</span> First-class <code>proof_count</code> provenance nudges ranking ±5% and is projectable via <code>--field proof_count</code>.</p>
    <p class="without"><span class="lbl">Without it</span> A note proven 12 times ranks identically to an unverified one.</p>
  </div>
</div>

## Scope: sharding & isolation

<div class="rm-cards two">
  <div class="rm-card">
    <div class="h"><span class="id">R15</span> Per-project sharding · <code>--global</code> / <code>RECALL_GLOBAL</code></div>
    <p><span class="lbl">Does</span> Each project has its own nano-graphrag shard; recall defaults to the current project's. <code>--global</code> unions across all.</p>
    <p class="without"><span class="lbl">Without it</span> Every project's recall is polluted by every other's; the relevant local note drowns.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">A6</span> Branch-aware isolation · <code>RECALL_BRANCH</code> / <code>--all-branches</code></div>
    <p><span class="lbl">Does</span> Within a project, each git branch/worktree gets a sub-shard; recall pins to the current branch.</p>
    <p class="without"><span class="lbl">Without it</span> A speculative note from an abandoned <code>feat/y</code> surfaces as fact while you work <code>feat/x</code>.</p>
  </div>
</div>

## Recall modes & inject

<div class="rm-cards two">
  <div class="rm-card">
    <div class="h"><span class="id">M1 ★</span> Staged 3-layer recall · <code>reflect index</code> → <code>reflect hydrate</code></div>
    <p><span class="lbl">Does</span> Index-then-hydrate: returns token-capped ID-only rows; the agent hydrates only the ids it wants.</p>
    <p class="without"><span class="lbl">Without it</span> Every recall pays full-body cost for every candidate; deep digs become token-prohibitive.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R10</span> 3-tier hierarchical inject · <code>REFLECT_TIERED_INJECT</code></div>
    <p><span class="lbl">Does</span> SessionStart consults curated skills first; a strong skill hit is injected and raw recall skipped.</p>
    <p class="without"><span class="lbl">Without it</span> Every session runs full raw recall even when a promoted skill already has the answer.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R11</span> Forced-grounding short-circuit · (R10 freshness gate)</div>
    <p><span class="lbl">Does</span> If the tier-1 skill hit is fresh <strong>and</strong> high-confidence, SessionStart emits just that and never spawns the recall subprocess.</p>
    <p class="without"><span class="lbl">Without it</span> Warm-project boots needlessly spawn the full pipeline — slow and noisy.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">M7</span> Knowledge-corpus Q&amp;A · <code>reflect corpus build</code></div>
    <p><span class="lbl">Does</span> Snapshots a filtered KB subset into <code>corpora/&lt;name&gt;.json</code> for a primed, deterministic Q&amp;A scope.</p>
    <p class="without"><span class="lbl">Without it</span> No way to pin recall to a curated subset; every query hits the whole corpus.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">M4</span> Pluggable-mode inheritance · <code>REFLECT_MODE</code></div>
    <p><span class="lbl">Does</span> Loads taxonomy + prompt templates from a mode JSON (deep-merge inheritance); drives learning types + economics glyphs.</p>
    <p class="without"><span class="lbl">Without it</span> One hard-coded taxonomy; research/writing workflows can't retune what's captured/surfaced.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">O3</span> Persona / preference answer · always-on</div>
    <p><span class="lbl">Does</span> A high-confidence distilled field (e.g. <code>testing_style='TDD'</code>) answers an open-domain query directly. “what testing style does this project use?”</p>
    <p class="without"><span class="lbl">Without it</span> The question falls through to generic recall; a known team fact isn't answered crisply.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">A1</span> Pinned editable memory slots · <code>REFLECT_SLOTS</code></div>
    <p><span class="lbl">Does</span> A pinned scratchpad slot per (project, name) injected at Tier-0, ahead of skills/recall, regardless of ranking.</p>
    <p class="without"><span class="lbl">Without it</span> No way to force an always-present note; critical context depends on it ranking well.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">R20</span> Skills-index query · always-on</div>
    <p><span class="lbl">Does</span> A queryable sqlite index of installed skills (name/tags/summary) replaces per-query SKILL.md scanning; feeds R10/R11.</p>
    <p class="without"><span class="lbl">Without it</span> Tiered inject rescans every SKILL.md per query; slower, and never matches an unindexed skill.</p>
  </div>
</div>

## Caching, dedup & negative-recall

<div class="rm-cards two">
  <div class="rm-card">
    <div class="h"><span class="id">R9</span> Fuzzy cache tier · <code>RECALL_FUZZY_CACHE</code> (on)</div>
    <p><span class="lbl">Does</span> A reworded repeat within the Jaccard threshold (0.85) is served from cache — skips embed+graph+rerank.</p>
    <p class="without"><span class="lbl">Without it</span> Every rephrasing pays the full retrieval cost; a debugging back-and-forth re-runs it dozens of times.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">S7</span> Chunk-hash dedup · always-on</div>
    <p><span class="lbl">Does</span> Slice-chunk hashing at drain so re-draining the same transcript doesn't duplicate the learning.</p>
    <p class="without"><span class="lbl">Without it</span> Recall returns two copies of the same lesson; duplicates crowd the top-k.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">C1</span> Semantic-dedup adjudication · <code>REFLECT_DEDUP_THRESHOLD</code> (0.97)</div>
    <p><span class="lbl">Does</span> Before a CREATE lands, an embedding-cosine twin ≥ threshold is held as a “merge?” adjudication.</p>
    <p class="without"><span class="lbl">Without it</span> Near-identical phrasings accumulate; the KB bloats with restatements of one idea.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">A3</span> <code>forget_after</code> TTL prune · hourly sweep</div>
    <p><span class="lbl">Does</span> Expired learnings are archived and moved to <code>.forgotten/</code>; permanent/future ones survive.</p>
    <p class="without"><span class="lbl">Without it</span> Stale, time-boxed notes linger forever and keep surfacing past their relevance.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">SG6</span> Knowledge-gap signal · <code>RECALL_GAP_LOG</code> (on)</div>
    <p><span class="lbl">Does</span> A 0-result recall logs <code>{query, normalized, session_id}</code> to <code>knowledge-gaps.jsonl</code> as a curation backlog.</p>
    <p class="without"><span class="lbl">Without it</span> Misses vanish silently; you never learn what the KB <em>should</em> have known.</p>
  </div>
  <div class="rm-card">
    <div class="h"><span class="id">S1</span> Structured field extraction · <code>--field NAME</code></div>
    <p><span class="lbl">Does</span> Projects a single typed field (<code>rule</code>/<code>fix</code>/<code>problem</code>/…) instead of the whole note.</p>
    <p class="without"><span class="lbl">Without it</span> Every hit returns the full note body; context-expensive when you only need the rule.</p>
  </div>
</div>

## Worked examples

Illustrative command → output for the marquee arms (representative, not a live run).

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

## The other 29 — capture, storage, consolidation, team

Not query-time features, but the plumbing that fills and maintains the KB the recall arms read.
Listed for completeness so the full 57 are accounted for. (Scroll horizontally.)

<div class="rm-matrix">
<table>
<thead><tr><th class="rowh">Feature</th><th>Category</th><th>Flag (default)</th><th class="wrap">What it does</th></tr></thead>
<tbody>
<tr><td class="rowh">M2 Writer-output classifier + breaker</td><td>capture</td><td><code>REFLECT_DRAIN_INVALID_THRESHOLD</code> (3)</td><td class="wrap">Kills + archives a drifting/poisoned writer after N bad outputs.</td></tr>
<tr><td class="rowh">M3 Quota-aware writer abort</td><td>capture</td><td><code>REFLECT_DRAIN_DAILY_MAX</code></td><td class="wrap">Defers the whole drain queue when the daily LLM gate is closed, instead of burning the cap.</td></tr>
<tr><td class="rowh">M5 Commit-reference verification</td><td>capture</td><td>always-on</td><td class="wrap">Checks every cited commit hash against the repo; rejects all-fabricated notes, flags partials.</td></tr>
<tr><td class="rowh">M6 Private-tag strip</td><td>capture</td><td>always-on</td><td class="wrap">Strips <code>&lt;private&gt;</code> spans at the LLM-prompt boundary so they never reach the writer/index.</td></tr>
<tr><td class="rowh">M8 Token-economics surfacing</td><td>dashboard</td><td><code>RECALL_ECONOMICS</code> (on)</td><td class="wrap">Annotates each result with discovery/read tokens + savings % and a mode glyph.</td></tr>
<tr><td class="rowh">S2 Typed causal-link enum</td><td>capture</td><td>always-on</td><td class="wrap">Closed enum for sidecar relations (caused_by / causes / enables / prevents / contradicts / supersedes / part_of / uses).</td></tr>
<tr><td class="rowh">S5 Belief-revision on ingest</td><td>capture</td><td>always-on</td><td class="wrap">Runs CREATE/UPDATE/DELETE against <code>reflect.db</code> so new learnings revise prior beliefs.</td></tr>
<tr><td class="rowh">S6 History snapshot on update</td><td>capture</td><td>always-on</td><td class="wrap">Snapshots the prior form into <code>learning_history</code> before mutating a live row.</td></tr>
<tr><td class="rowh">S8 Doc→chunk→learning grouping</td><td>capture</td><td>always-on</td><td class="wrap">Persists each learning's lineage back to its source transcript + chunk.</td></tr>
<tr><td class="rowh">S9 Volatile-signals sidecar</td><td>capture</td><td>always-on</td><td class="wrap">Moves churning signals (recall_count, helpful_count…) out of note markdown into a DB sidecar — clean git diffs.</td></tr>
<tr><td class="rowh">S10 Write-validate-retry loop</td><td>capture</td><td>always-on (3 tries)</td><td class="wrap">Validates structure + sidecar after write; re-prompts; flags unfixable notes <code>validated: false</code>.</td></tr>
<tr><td class="rowh">R13 Auto skill-refresh trigger</td><td>capture</td><td>always-on</td><td class="wrap">Flags an existing skill for refresh when a learning it covers lands.</td></tr>
<tr><td class="rowh">R14 Per-skill staleness signal</td><td>signal</td><td><code>REFLECT_STALENESS_DAYS</code> (30)</td><td class="wrap">Marks a skill <code>is_stale</code> when an in-scope learning changed after its last refresh.</td></tr>
<tr><td class="rowh">SG1 Cross-turn contradiction</td><td>capture</td><td>always-on</td><td class="wrap">Detects + reconciles contradicting learnings at capture (sets <code>is_latest</code>).</td></tr>
<tr><td class="rowh">SG2 Git-event capture</td><td>capture</td><td>always-on</td><td class="wrap">Links commits↔sessions; demotes a reverted commit's learnings on <code>git revert</code>.</td></tr>
<tr><td class="rowh">SG3 Idle-sweep trigger</td><td>signal</td><td><code>REFLECT_IDLE_THRESHOLD_SEC</code></td><td class="wrap">Idle timer sweeps quiet transcripts into speculative learnings (down-ranked at recall).</td></tr>
<tr><td class="rowh">SG4 Test-outcome parsing</td><td>signal</td><td>always-on</td><td class="wrap">Parses pass/fail from Bash output in PostToolUse into a capture signal.</td></tr>
<tr><td class="rowh">SG5 Tool-loop detection</td><td>signal</td><td>always-on</td><td class="wrap">Detects repeated/oscillating tool-call loops as a signal.</td></tr>
<tr><td class="rowh">SG7 TodoWrite completion signal</td><td>signal</td><td>always-on</td><td class="wrap">Emits a “how I did X” candidate when a todo flips to <code>completed</code>.</td></tr>
<tr><td class="rowh">SG8 Permission-reply capture</td><td>signal</td><td>always-on</td><td class="wrap">Captures permission-prompt allow/deny replies as policy learnings.</td></tr>
<tr><td class="rowh">A2 Bitemporal graph edges</td><td>infra</td><td>always-on</td><td class="wrap">Edges carry <code>tcommit</code> / <code>tvalid</code> / <code>tvalid_end</code> — “what was true” vs “what we knew” stay separable.</td></tr>
<tr><td class="rowh">A4 Followup-rate diagnostic</td><td>dashboard</td><td><code>RECALL_FOLLOWUP</code> (on)</td><td class="wrap">Logs a recall-quality verdict (did the user immediately re-query differently?) to metrics.</td></tr>
<tr><td class="rowh">A5 Synthetic compression fallback</td><td>capture</td><td>drain <code>--no-llm</code></td><td class="wrap">Builds a structured learning from heuristics alone when the drain LLM is unavailable.</td></tr>
<tr><td class="rowh">C2 Auto-consolidation threshold</td><td>consolidation</td><td><code>REFLECT_SYNTHESIS_AUTO_THRESHOLD</code> (30)</td><td class="wrap">Fires the synthesis pass early once learnings-since-last-consolidation cross the threshold.</td></tr>
<tr><td class="rowh">C3 Graph maintenance sweep</td><td>consolidation</td><td>every N drains</td><td class="wrap">Structural rewrite of the local graphml to repair orphan edges after deletes.</td></tr>
<tr><td class="rowh">C4 Lifecycle events fan-out</td><td>infra</td><td><code>REFLECT_EVENTS_ON_&lt;EVENT&gt;</code></td><td class="wrap">Appends lifecycle events to <code>events.jsonl</code> + runs per-event shell hooks (local webhooks).</td></tr>
<tr><td class="rowh">C5 KB export/import round-trip</td><td>team</td><td><code>kb_export.py</code> / <code>kb_import.py</code></td><td class="wrap">Snapshots <code>documents/</code> + <code>reflect.db</code> into one git-friendly tarball; byte-identical restore elsewhere.</td></tr>
<tr><td class="rowh">O1 Consolidated observations</td><td>consolidation</td><td>always-on</td><td class="wrap">A 2nd drain stream of persona/convention statements that accumulate evidence over time.</td></tr>
<tr><td class="rowh">O2 Auto-refreshing conventions doc</td><td>consolidation</td><td>always-on</td><td class="wrap">Re-renders a conventions markdown doc from accumulated observations each consolidation.</td></tr>
</tbody>
</table>
</div>

---

Every feature above is verified by a behavioural proof under
[`tests/eval/behavioral/proofs/`](https://github.com/stevengonsalvez/ainb-reflect-memory/tree/main/tests/eval/behavioral/proofs)
(57 `proof_*.py`, one per port). See the [`reflect` CLI reference](/knowledge/reflect-cli/) for how to
drive recall directly.
