# Reflect retrieval, by example

> How reflect decides **what prior knowledge to surface** — every retrieval feature with a concrete example, why it matters, and what would break without it.

Reflect's recall layer fuses a vector arm (nano-graphrag), a graph arm, and a BM25 arm (QMD) with reciprocal-rank fusion, then reranks and gates the result before injecting it. Each stage below is an independent, env-gated feature — this is the map of why each one exists.

Every feature here ships with a behavioral proof (`reflect-kb/tests/eval/behavioral/proofs/`) that demonstrates exactly the behavior described, with the knob on **and** off.

## R1 — Graph-expansion arm

**Knob:** `RECALL_GRAPH_ARM`

**Example.** You ask *“why does the checkout flow call `recalcTax` twice?”*. Vector/BM25 finds the note *“recalcTax is idempotent but expensive”*. The graph arm then hops along that note's `caused_by` edge to a second note you never lexically matched — *“double-call was added to fix a rounding bug in EU VAT (commit a1b2c3)”* — and injects both.

**Why it matters.** Most real answers are one hop away from the words you typed. The graph arm turns a flat keyword hit into a *connected* explanation, which is exactly how multi-hop questions (‘what depends on X?’, ‘what caused Y?’) get answered.

**Without it.** Recall returns only the lexically-matching note. You'd see *“recalcTax is expensive”* and conclude it's a perf mistake — and *re-introduce the rounding bug* the double-call was deliberately fixing, because the note explaining *why* never surfaced.

## R2 — Cross-encoder rerank

**Knob:** `RECALL_CROSS_ENCODER`

**Example.** Query *“flaky test in the auth suite”*. RRF returns 5 candidates; a BM25-heavy hit about *“auth token format”* sits at rank 1. The cross-encoder re-reads each candidate against the full query and pushes the actually-relevant *“auth integration test is flaky under parallel xdist”* note to rank 1.

**Why it matters.** Lexical/vector fusion ranks by term overlap; a cross-encoder ranks by *meaning*. It's the difference between ‘contains the same words’ and ‘answers the same question’.

**Without it.** The keyword-similar-but-wrong note wins rank 1. With a tight inject budget (top-1 or top-2), the agent reads about token *format* when it asked about a *flaky test* — relevant prior art exists but never makes the cut.

## R3 — MMR diversity

**Knob:** `RECALL_MMR`

**Example.** Query *“nginx 502 under load”*. The corpus has 4 near-identical notes (*“raise nginx worker_connections”*) plus one distinct hit (*“upstream keepalive must be enabled or you get 502s at scale”*). MMR de-clusters the 4 twins so the distinct keepalive note makes the top-5 instead of being crowded out.

**Why it matters.** A KB accretes duplicate phrasings of the same lesson. Without diversity, your top-k is 4 copies of one idea and you miss the *second* relevant idea entirely.

**Without it.** All 5 inject slots are filled with the same ‘raise worker_connections’ note. You bump worker_connections, the 502s continue (the real cause was keepalive), and the note that would have told you sits at rank 6, never injected.

## R4 — Token-budget retrieval

**Knob:** `REFLECT_RECALL_MAX_TOKENS`

**Example.** SessionStart on a verbose project. A fixed top-5 would inject ~6k tokens of prior learnings. With a 1.5k-token budget, recall packs the highest-ranked notes until the budget is hit — maybe 2 full notes — and stops, instead of blindly taking 5.

**Why it matters.** Context is finite and shared with the user's actual task. Budgeting by *tokens* keeps the inject proportional to what you can afford, not to an arbitrary count.

**Without it.** A fixed top-k blows the context window on a verbose corpus — 5 long notes evict the user's own files from context, or the session boots slowly. You trade the user's working memory for prior trivia.

## R5 — Temporal retrieval arm

**Knob:** `RECALL_TEMPORAL`

**Example.** Query *“what's our current API auth?”*. Two notes match: *“we use JWT”* (archived April) and *“migrated to server-side sessions”* (archived June). The temporal arm ranks the June note above the stale April one.

**Why it matters.** Design knowledge evolves. ‘Most cited’ or ‘most lexically similar’ will happily hand you April's answer in June. Recency-as-a-signal keeps *current* truth on top.

**Without it.** Recall returns the April ‘we use JWT’ note (it's older, so more-cited). The agent writes JWT code into a codebase that moved to sessions months ago — confidently wrong, from stale memory.

## R6 — Query-time date parsing

**Knob:** `RECALL_TEMPORAL`

**Example.** Query *“what did we change in the payments module last week?”*. Recall parses *“last week”* into a real date range and filters the temporal arm to notes archived in that window — not notes that merely contain the words ‘last week’.

**Why it matters.** Humans ask about time in words. Turning ‘in April’ / ‘last week’ / ‘before the migration’ into an actual filter is the difference between time-aware recall and text-matching the word ‘April’.

**Without it.** ‘last week’ is treated as two more keywords. You get notes that happen to say ‘last week’ from any date, and miss the actual recent changes — the temporal intent is silently dropped.

## R7 — OOD relevance gate

**Knob:** `--min-overlap`

**Example.** SessionStart in a brand-new repo with nothing relevant indexed. The best hit barely overlaps the project name. The OOD gate detects ‘nearest is still junk’ and injects *nothing* rather than a misleading top-5.

**Why it matters.** Most sessions have **no** relevant prior art. Injecting the least-bad junk every time trains the agent to distrust the memory and wastes context on noise.

**Without it.** Every session gets 5 vaguely-related notes injected whether or not they help. The signal-to-noise of the memory craters; the agent learns to ignore the inject block, and a genuinely relevant hit later gets ignored with the rest.

## R8 — Bounded multiplicative boosts

**Knob:** `RECALL_*_ALPHA`

**Example.** Two notes tie on base relevance for *“retry strategy”*. One is newer / higher-confidence / more-proven, so its bounded boost breaks the tie to rank 1. But a 2-year-old note that *directly* answers the query still beats a barely-related note that was archived today — the recency boost is capped and can't override a decisive relevance gap.

**Why it matters.** Secondary signals (recency, confidence, proof-count, tags) should *break ties*, not *dominate*. Bounding each boost keeps them honest — a tie-breaker, never a hijacker.

**Without it.** Unbounded boosts let one signal win outright: a brand-new but off-topic note buries a 2-year-old note that perfectly answers the question, purely because it's newer. Ranking becomes ‘whatever is most recent’ instead of ‘whatever is most relevant’.

## R9 — Fuzzy cache tier

**Knob:** `RECALL_FUZZY_CACHE`

**Example.** You run *“how do I debounce search input”*, then minutes later *“debouncing the search-as-you-type box”*. The second query's tokens are within the fuzzy threshold of the first, so it's served from cache — no fresh embedding + graph walk.

**Why it matters.** Re-worded repeats of a question are common in a session. Serving them from a similarity-keyed cache skips the whole pipeline — faster boot, fewer tokens, same answer.

**Without it.** Every rephrasing pays the full retrieval cost (embed + vector + graph + rerank, seconds each). A back-and-forth debugging session re-runs near-identical recalls dozens of times.

## R10 — 3-tier hierarchical inject

**Knob:** `REFLECT_TIERED_INJECT`

**Example.** SessionStart on a familiar project. A curated *skill* (‘this repo: always run `make fmt` before commit’) scores high in the tier-1 skills lookup, so it's injected outright and the broad raw-learnings recall is skipped.

**Why it matters.** A curated, promoted skill is higher-signal than raw notes. Consulting skills first — and letting a strong hit win — gives the cleanest possible inject on familiar ground.

**Without it.** Every session does the full raw-learnings recall even when a curated skill already has the answer. You get a noisy pile of notes instead of the one promoted convention that matters, and pay the full retrieval cost to get worse signal.

## R11 — Forced-grounding short-circuit

**Knob:** `(R10 freshness gate)`

**Example.** Returning to a warm project: the tier-1 skill hit is both fresh and high-confidence, so SessionStart emits just that one skill and *stops* — no lower-tier recall subprocess runs at all.

**Why it matters.** On the common case (familiar workflow), one skill lookup is the whole answer. Short-circuiting there makes boot instant and silent — zero extra tokens, zero latency.

**Without it.** Even when one fresh skill fully grounds the session, recall still spawns the full lower-tier pipeline. Warm-project boots are needlessly slow and noisy, paying for retrieval the session didn't need.

## R12 — Per-arm calibrated thresholds

**Knob:** `RECALL_ARM_*_MIN_SCORE`

**Example.** The vector arm's cosine scores and the BM25 arm's scores live on totally different scales. R12 gives each arm its own floor (calibrated by `reflect calibrate-thresholds`), so a weak BM25 hit is dropped by the BM25 floor without also nuking a legitimately-strong graph hit.

**Why it matters.** One global relevance threshold mis-gates non-comparable arms — too loose for one, too strict for another. Per-arm floors tighten the OOD gate without collateral damage.

**Without it.** A single global cutoff either lets BM25 noise through (set loose enough for the graph arm) or starves the graph arm (set tight enough for BM25). You can't tune one arm without breaking another.

## R15 — Per-project sharding

**Knob:** `RECALL_BRANCH / --global`

**Example.** You're in `repo-a`. Recall reads `repo-a`'s learning shard only. The lesson *“in repo-b, never bump the shared proto without regenerating clients”* does **not** surface — unless you pass `--global` to deliberately union across projects.

**Why it matters.** A learning from one codebase is usually noise in another. Sharding makes same-project recall sharp by default, with an explicit escape hatch for cross-project search.

**Without it.** Every project's recall is polluted by every other project's learnings. In `repo-a` you get `repo-b`'s deploy quirks and `repo-c`'s test flakes — the relevant local note drowns in unrelated cross-project history.

## R16 — Project-affinity boost

**Knob:** `RECALL_PROJECT_ALPHA`

**Example.** With `--global` on (cross-project search), a note from the *current* project and an equally-relevant note from another project both match. The affinity boost lifts the current-project note above the foreign one — softly, so a decisively-better foreign note can still win.

**Why it matters.** Even when you *want* cross-project recall, your own project's prior art is usually the better answer. A bounded affinity boost prefers local without hard-excluding the occasionally-superior foreign hit.

**Without it.** Cross-project recall treats every project equally. A foreign note outranks your own project's more-applicable note just because it's lexically a hair closer — you get someone else's answer to your project's question.

## M1 — Staged 3-layer recall

**Knob:** `(recall_stages)`

**Example.** Instead of dumping 5 full notes (~3k tokens), recall first returns a token-capped *index* (id + title + score, ~50 tokens each). The agent picks the 2 interesting ids and *hydrates* only those to full bodies. A cheap broad scan, then an expensive read only where it pays.

**Why it matters.** Reading every candidate in full is wasteful when the agent only needs one or two. Index-then-hydrate matches retrieval cost to actual interest.

**Without it.** Every recall pays full-body token cost for every candidate, most of which the agent skims and discards. Deep digs over a large KB become token-prohibitive — you can't afford to look at 20 candidates, so you look at 5 and miss the right one.

## A6 — Branch-aware isolation

**Knob:** `RECALL_BRANCH / --all-branches`

**Example.** You're on a `feat/x` worktree. SessionStart pins recall to the `feat/x` sub-shard, so a half-finished learning captured on `feat/y` in a sibling worktree doesn't leak into this session. `--all-branches` unions them when you want the full picture.

**Why it matters.** Parallel worktrees are how agents actually work. Branch isolation stops one branch's in-progress, possibly-wrong learnings from contaminating another's recall.

**Without it.** Every worktree sees every other worktree's learnings. A speculative note from an abandoned `feat/y` experiment surfaces as fact while you work `feat/x` — cross-branch contamination of exactly the kind branch isolation exists to prevent.

---

*Generated from the retrieval-feature catalogue. The full 57-port catalogue (storage, signals, consolidation, open-domain) and the proof matrix are tracked on the `feat/recall-wave1` branch / PR #248.*
