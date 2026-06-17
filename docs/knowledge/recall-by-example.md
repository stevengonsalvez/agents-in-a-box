---
title: "How memory recall works — by example"
description: "A worked, end-to-end example of reflect's memory, then every retrieval feature with a concrete example and what would break without it."
---

> "Correct once, never again." This page shows what that actually *looks like* — first a single learning's whole journey from capture to recall, then each retrieval feature with a concrete example and the counterfactual: what you'd get if that feature didn't exist.

For the architecture (tiers, QMD vs GraphRAG, the capture pipeline) see [How reflection works](/knowledge/overview/). This page is the **behavioural** companion — recall by example.

## Memory end to end — one learning's journey

Follow one correction from the moment it happens to the moment it saves you weeks later.

**1 · Capture.** Mid-session you tell the agent: *"no — don't bump the shared `payments.proto` without regenerating the clients, it broke staging last time."* A `PostToolUse`/`Stop` hook detects the correction signal (the *"no — don't…"* shape), slices just the relevant dialogue window (not the whole 100k-token transcript), and the drain writes a structured learning:

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

**2 · Index.** `reflect reindex` embeds the note (vector arm), adds its entities + edges to the GraphRAG graph (graph arm), and registers it in the BM25 index (QMD arm). It's now reachable three different ways.

**3 · Recall — three weeks later, a different session.** A new teammate's agent opens `billing-svc` and is about to edit `payments.proto`. **SessionStart** fires, builds a query from the project + branch context, and runs hybrid recall:

- the **vector arm** matches on *proto / payments / codegen* meaning,
- the **BM25 arm** matches the literal `payments.proto`,
- the **graph arm** hops the `prevents` edge to the staging-outage context,
- **RRF** fuses the three rankings, the **cross-encoder** reranks, the **OOD gate** confirms it's genuinely relevant, and the **token budget** packs it into the inject block.

Before the agent writes a single line, it sees: *"Regenerate gRPC clients after editing payments.proto — broke staging last time; run `make proto-gen`."* The mistake never happens twice.

That whole chain — capture → index → fuse → rerank → gate → inject — is what the features below tune. Each one is independently togglable, and each ships a behavioural proof that demonstrates exactly the behaviour described.

## Retrieval, feature by feature

Each card is one retrieval feature: a concrete **example**, **why it matters**, and **without it** — what you'd get if the feature didn't exist.

### R1 · Graph-expansion arm — `RECALL_GRAPH_ARM`
- **Example.** You ask *"why does the checkout flow call `recalcTax` twice?"*. Vector/BM25 finds *"recalcTax is idempotent but expensive"*. The graph arm hops that note's `caused_by` edge to a note you never lexically matched — *"the double-call fixes a rounding bug in EU VAT (commit a1b2c3)"* — and injects both.
- **Why it matters.** Most real answers are one hop away from the words you typed. The graph arm turns a flat keyword hit into a *connected* explanation — how multi-hop questions ("what depends on X?", "what caused Y?") actually get answered.
- **Without it.** Recall returns only the lexically-matching note. You see *"recalcTax is expensive"*, call it a perf mistake, and **re-introduce the rounding bug** the double-call was deliberately fixing — because the note explaining *why* never surfaced.

### R2 · Cross-encoder rerank — `RECALL_CROSS_ENCODER`
- **Example.** Query *"flaky test in the auth suite"*. RRF puts a BM25-heavy *"auth token format"* note at rank 1. The cross-encoder re-reads each candidate against the full query and lifts the real answer — *"auth integration test is flaky under parallel xdist"* — to rank 1.
- **Why it matters.** Fusion ranks by term overlap; a cross-encoder ranks by *meaning*. It's "contains the same words" vs "answers the same question".
- **Without it.** The keyword-similar-but-wrong note wins rank 1. With a tight inject budget you read about token *format* when you asked about a *flaky test* — the right prior art exists but never makes the cut.

### R3 · MMR diversity — `RECALL_MMR`
- **Example.** Query *"nginx 502 under load"*. The corpus has 4 near-identical *"raise worker_connections"* notes plus one distinct *"enable upstream keepalive or you get 502s at scale"*. MMR de-clusters the 4 twins so the keepalive note makes the top-5.
- **Why it matters.** A KB accretes duplicate phrasings of the same lesson. Without diversity your top-k is 4 copies of one idea and you miss the *second* idea.
- **Without it.** All 5 inject slots are the same "raise worker_connections" note. You bump it, the 502s continue (the real cause was keepalive), and the note that would have told you sits at rank 6, never injected.

### R4 · Token-budget retrieval — `REFLECT_RECALL_MAX_TOKENS`
- **Example.** SessionStart on a verbose project. A fixed top-5 would inject ~6k tokens. With a 1.5k-token budget, recall packs the highest-ranked notes until the budget is hit — maybe 2 full notes — and stops.
- **Why it matters.** Context is finite and shared with the user's task. Budgeting by *tokens* keeps the inject proportional to what you can afford, not to an arbitrary count.
- **Without it.** A fixed top-k blows the window on a verbose corpus — 5 long notes evict the user's own files from context, or boot crawls. You trade working memory for prior trivia.

### R5 · Temporal retrieval arm — `RECALL_TEMPORAL`
- **Example.** Query *"what's our current API auth?"*. Two notes match: *"we use JWT"* (April) and *"migrated to server-side sessions"* (June). The temporal arm ranks June above stale April.
- **Why it matters.** Design knowledge evolves. "Most cited" hands you April's answer in June. Recency-as-a-signal keeps *current* truth on top.
- **Without it.** Recall returns the older, more-cited "we use JWT" note. The agent writes JWT code into a codebase that moved to sessions months ago — confidently wrong, from stale memory.

### R6 · Query-time date parsing — `RECALL_TEMPORAL`
- **Example.** Query *"what did we change in payments last week?"*. Recall parses *"last week"* into a real date range and filters to notes archived in that window — not notes that merely contain the words "last week".
- **Why it matters.** Humans ask about time in words. Turning "in April" / "last week" / "before the migration" into an actual filter is the difference between time-aware recall and text-matching the word "April".
- **Without it.** "last week" is two more keywords. You get notes that *say* "last week" from any date and miss the actual recent changes — the temporal intent is silently dropped.

### R7 · OOD relevance gate — `--min-overlap`
- **Example.** SessionStart in a brand-new repo with nothing relevant indexed. The best hit barely overlaps the project. The OOD gate detects "nearest is still junk" and injects *nothing* rather than a misleading top-5.
- **Why it matters.** Most sessions have **no** relevant prior art. Injecting the least-bad junk every time trains the agent to distrust the memory and wastes context.
- **Without it.** Every session gets 5 vaguely-related notes whether or not they help. Signal-to-noise craters, the agent learns to ignore the inject block — and a genuinely relevant hit later is ignored with the rest.

### R8 · Bounded multiplicative boosts — `RECALL_*_ALPHA`
- **Example.** Two notes tie on base relevance for *"retry strategy"*. The newer / higher-confidence / more-proven one wins the tie. But a 2-year-old note that *directly* answers the query still beats a barely-related note archived today — the recency boost is capped and can't override a decisive relevance gap.
- **Why it matters.** Secondary signals (recency, confidence, proof-count, tags) should *break ties*, not *dominate*. Bounding each keeps it a tie-breaker, never a hijacker.
- **Without it.** Unbounded boosts let one signal win outright: a brand-new but off-topic note buries a 2-year-old note that perfectly answers the question, purely for being newer. Ranking becomes "most recent" instead of "most relevant".

### R9 · Fuzzy cache tier — `RECALL_FUZZY_CACHE`
- **Example.** You run *"how do I debounce search input"*, then *"debouncing the search-as-you-type box"*. The second is within the fuzzy threshold of the first, so it's served from cache — no fresh embedding + graph walk.
- **Why it matters.** Re-worded repeats are common in a session. Serving them from a similarity-keyed cache skips the whole pipeline — faster, fewer tokens, same answer.
- **Without it.** Every rephrasing pays the full retrieval cost (embed + vector + graph + rerank, seconds each). A back-and-forth debugging session re-runs near-identical recalls dozens of times.

### R10 · 3-tier hierarchical inject — `REFLECT_TIERED_INJECT`
- **Example.** SessionStart on a familiar project. A curated *skill* ("this repo: always run `make fmt` before commit") scores high in the tier-1 skills lookup, so it's injected outright and the broad raw-learnings recall is skipped.
- **Why it matters.** A curated, promoted skill is higher-signal than raw notes. Consulting skills first — and letting a strong hit win — gives the cleanest inject on familiar ground.
- **Without it.** Every session runs the full raw-learnings recall even when a curated skill has the answer. You get a noisy pile of notes instead of the one promoted convention, and pay full retrieval cost for worse signal.

### R11 · Forced-grounding short-circuit — R10 freshness gate
- **Example.** Returning to a warm project: the tier-1 skill hit is fresh *and* high-confidence, so SessionStart emits just that skill and *stops* — no lower-tier recall subprocess runs at all.
- **Why it matters.** On the common case (familiar workflow) one skill lookup is the whole answer. Short-circuiting there makes boot instant and silent.
- **Without it.** Even when one fresh skill fully grounds the session, recall still spawns the full pipeline. Warm-project boots are needlessly slow and noisy.

### R12 · Per-arm calibrated thresholds — `RECALL_ARM_*_MIN_SCORE`
- **Example.** The vector arm's cosine scores and the BM25 arm's scores live on different scales. R12 gives each arm its own floor (set by `reflect calibrate-thresholds`), so a weak BM25 hit is dropped by the BM25 floor without nuking a legitimately-strong graph hit.
- **Why it matters.** One global threshold mis-gates non-comparable arms — too loose for one, too strict for another. Per-arm floors tighten gating without collateral damage.
- **Without it.** A single cutoff either lets BM25 noise through (loose enough for the graph arm) or starves the graph arm (tight enough for BM25). You can't tune one arm without breaking another.

### R15 · Per-project sharding — `RECALL_BRANCH` / `--global`
- **Example.** You're in `repo-a`. Recall reads `repo-a`'s shard only. The lesson *"in repo-b, never bump the shared proto without regenerating clients"* does **not** surface — unless you pass `--global` to deliberately union across projects.
- **Why it matters.** A learning from one codebase is usually noise in another. Sharding makes same-project recall sharp by default, with an explicit cross-project escape hatch.
- **Without it.** Every project's recall is polluted by every other's. In `repo-a` you get `repo-b`'s deploy quirks and `repo-c`'s test flakes; the relevant local note drowns.

### R16 · Project-affinity boost — `RECALL_PROJECT_ALPHA`
- **Example.** With `--global` on, a current-project note and an equally-relevant foreign note both match. The affinity boost lifts the current-project note above the foreign one — softly, so a decisively-better foreign note can still win.
- **Why it matters.** Even when you *want* cross-project recall, your own project's prior art is usually the better answer. A bounded affinity boost prefers local without hard-excluding a superior foreign hit.
- **Without it.** Cross-project recall treats every project equally. A foreign note outranks your own more-applicable note just for being a hair closer lexically — you get someone else's answer to your project's question.

### M1 · Staged 3-layer recall — `recall_stages`
- **Example.** Instead of dumping 5 full notes (~3k tokens), recall first returns a token-capped *index* (id + title + score, ~50 tokens each). The agent picks the 2 interesting ids and *hydrates* only those to full bodies.
- **Why it matters.** Reading every candidate in full is wasteful when only one or two matter. Index-then-hydrate matches retrieval cost to actual interest.
- **Without it.** Every recall pays full-body cost for every candidate, most of which the agent skims and discards. Deep digs over a large KB become token-prohibitive — you look at 5 instead of 20 and miss the right one.

### A6 · Branch-aware isolation — `RECALL_BRANCH` / `--all-branches`
- **Example.** You're on a `feat/x` worktree. SessionStart pins recall to the `feat/x` sub-shard, so a half-finished learning captured on `feat/y` in a sibling worktree doesn't leak in. `--all-branches` unions them when you want the full picture.
- **Why it matters.** Parallel worktrees are how agents actually work. Branch isolation stops one branch's in-progress, possibly-wrong learnings from contaminating another's recall.
- **Without it.** Every worktree sees every other's learnings. A speculative note from an abandoned `feat/y` experiment surfaces as fact while you work `feat/x`.

---

Every feature above is verified by a behavioural proof under [`reflect-kb/tests/eval/behavioral/proofs/`](https://github.com/stevengonsalvez/agents-in-a-box/tree/main/reflect-kb/tests/eval/behavioral/proofs) that demonstrates the exact behaviour with the knob on **and** off. See the [`reflect` CLI reference](/knowledge/reflect-cli/) for how to drive recall directly.
