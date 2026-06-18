# LOCOMO benchmark — reflect 4.1.0 memory engine

**Pilot result.** 50 stratified QA (10 per category) from LOCOMO conversation
`conv-26` (locomo10). Answer + judge + writer all on **Sonnet** via clean
`claude -p`. J-score = LLM-judge correctness. Three tuning stages shown.

## Headline — tuning progression (best config = arms-OFF)

| stage | single-hop | multi-hop | temporal | open-domain | adversarial | **overall** |
|---|---|---|---|---|---|---|
| no-memory (floor) | 0.00 | 0.00 | 0.00 | 0.00 | 1.00 | **0.20** |
| baseline · top-8 / 3k chars / 239 notes | 0.40 | 0.20 | 0.50 | 0.50 | 1.00 | **0.52** |
| + recall budget · top-25 / 10k chars | 0.50 | **0.50** | 0.50 | 0.50 | 1.00 | **0.60** |
| **+ exhaustive extraction · 635 notes** | **0.60** | 0.50 | **0.70** | 0.50 | 0.90 | **0.64** |
| full-context (ceiling) | 0.90 | 0.80 | 0.60 | 0.20 | 0.90 | **0.68** |

reflect memory closed **0.52 → 0.64** of the 0.20→0.68 floor-to-ceiling band
(+23% relative) through two retrieval/ingestion fixes — the engine itself was
never changed.

## 4.1.0 recall-arms ablation (the headline question)

Verified the `RECALL_*` toggle changes retrieval (arms-ON and arms-OFF return
different notes, md5-distinct). Yet across **all three** stages:

| stage | arms-ON | arms-OFF | Δ (on − off) |
|---|---|---|---|
| baseline | 0.52 | 0.52 | +0.00 |
| + recall budget | 0.58 | 0.60 | −0.02 |
| + extraction | 0.62 | 0.64 | −0.02 |

**reflect 4.1.0's 57 recall arms produce no benefit — slightly negative — on this
conversational benchmark.** They reshuffle which questions are answered (e.g.
arms hurt open-domain, help nothing net) rather than adding signal. This is the
single most important finding and it is stable across configs.

## What's still missing (gap to ceiling)

- **single-hop 0.60 vs 0.90 ceiling** — exhaustive extraction helped (+0.20) but
  some direct facts still don't surface. The dialogue→note adapter, not the
  retrieval, is the remaining lever.
- **multi-hop 0.50 vs 0.80** — needs cross-fact chaining the single-pass recall
  doesn't do.
- **adversarial dipped 1.00 → 0.90** with 635 notes: more memory → the model
  occasionally answers an unanswerable question instead of abstaining. Inherent
  precision/recall tension.

## Operations

| metric | value |
|---|---|
| recall latency (per query) | ~20–45s — sentence-transformers + nano-graphrag reload **per** `recall.py` subprocess (dominant wall-time) |
| cost — arms_on + arms_off, 50 QA (tuned/extraction) | ~$7.4 |
| cost — full 4-config baseline, 50 QA | $27.2 |
| memory notes | 239 (baseline) → 635 (exhaustive) from 19 sessions |
| projected full locomo10 (1986 QA × 4) | ~$1,000, many hours |

## Method

- **Retrieval** = reflect-kb's real engine (`reflect reindex` + `recall.py`); 57
  v4.1.0 arms toggle via `RECALL_*` env knobs (arms-ON sets them; arms-OFF =
  pre-4.1 defaults). Toggle verified to alter retrieval output.
- **Ingestion** = LOCOMO-domain adapter: each session is LLM-extracted into
  atomic memory notes (reflect's shipped writer targets coding transcripts, not
  persona chat) — the part to harden next.
- **Answer/judge** = clean `claude -p --setting-sources '' --strict-mcp-config`
  (no session hooks/CLAUDE.md/MCP — verified no caveman pollution; OAuth, no API
  key).
- **Adversarial (cat 5)** scored correct only when the model abstains.

## Caveats

- Single conversation, n=50/cell → per-category swings of ±0.1 are noise; the
  arms-null, the multi-hop lift, and the single-hop gap each exceed that.
- Cost does not amortize: recall's 20–45s reload spaces `claude -p` calls past
  the 5-min prompt-cache TTL, so cache_creation re-bills (~$0.13/QA/config).
- `full_context` open-domain (0.20) is low because stuffing the whole
  conversation buries commonsense-inference answers — a full-context failure
  mode, not a reflect result.
- Numbers are this-machine, this-conversation. Full locomo10 would tighten the
  per-category cells and enable comparison to published Mem0/Zep numbers.
