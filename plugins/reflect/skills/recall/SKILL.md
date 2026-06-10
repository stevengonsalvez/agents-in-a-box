---
name: reflect:recall
description: |
  Retrieve relevant prior learnings from the global knowledge base. Hybrid
  vector + graph search over 170+ indexed learnings, reranked by confidence,
  recency, and tag overlap. Use when starting work, debugging a recurring
  problem, or before implementing a feature that may have prior art.
version: "3.1.0"
user-invocable: true
triggers:
  - reflect:recall
  - recall learnings
  - prior learnings
  - what have i learned about
  - have we done this before
allowed-tools:
  - Read
  - Bash
  - Grep
---

# /reflect:recall — Retrieve relevant prior learnings

Queries the global learnings KB (GraphRAG + vector) and surfaces the top-N
most relevant learnings for the current work, reranked by confidence, recency,
and tag overlap.

## When to use

- Starting work in a project or on a new branch — "what do we know about X"
- Debugging a recurring issue — "have we seen this error before"
- Before implementing a feature — "has this pattern been tried"
- When the user references past work ("like we did in Y")

**Also fires automatically** via the SessionStart hook (see
`hooks/session_start_recall.py`) with a 3-result cap, any confidence
(reranked by confidence/recency/tag-overlap). This skill is the
explicit, higher-limit path.

## Quick reference

| Invocation | Behavior |
|---|---|
| `/reflect:recall <query>` | Default — 10 results, any confidence, markdown out |
| `/reflect:recall <query> --limit 5 --confidence HIGH` | Tight filter |
| `/reflect:recall <query> --mode local` | Graph-neighborhood search (finds related concepts) |
| `/reflect:recall <query> --mode global` | Community-based (broad patterns) |
| `/reflect:recall <query> --format json` | Structured output for programmatic use |
| `/reflect:recall <query> --no-cache` | Skip cache, force fresh query |

## Staged recall (3-layer workflow — preferred for deep digs)

When you need more than the one-shot top-N — e.g. tracing how a problem
evolved, or hydrating several related learnings — use the staged pipeline in
`scripts/recall_stages.py` instead of repeated full recalls. It is the
token-cheap discipline (~10x savings vs. hydrating everything up front):

```bash
# Step 0 — print the contract (self-documenting bootstrap)
uv run {{HOME_TOOL_DIR}}/skills/recall/scripts/recall_stages.py workflow

# Step 1 — compact ID-only index: {id, title, score, project, date}
#          (~50-100 tokens/result)
uv run {{HOME_TOOL_DIR}}/skills/recall/scripts/recall_stages.py index "$QUERY" --limit 20

# Step 2 — chronological neighbours around an interesting hit
#          (anchor by ID, or pass a query to find the anchor automatically)
uv run {{HOME_TOOL_DIR}}/skills/recall/scripts/recall_stages.py timeline --anchor <ID> --depth-before 3 --depth-after 3

# Step 3 — full bodies + entity sidecars, ONLY for the filtered IDs
#          (~500-1000 tokens/result; always batch 2+ ids)
uv run {{HOME_TOOL_DIR}}/skills/recall/scripts/recall_stages.py hydrate <ID> [<ID> ...]
```

**Never run Step 3 without filtering through Steps 1-2 first.** The index and
timeline rows are deliberately ID-only so you can triage many results cheaply
and hydrate only what survives.

## Workflow

1. **Build the query** — combine the user's question with project context:
   current cwd, branch name, any relevant tags the user mentioned.
2. **Run recall** — invoke `{{HOME_TOOL_DIR}}/skills/recall/scripts/recall.py`:
   ```bash
   uv run {{HOME_TOOL_DIR}}/skills/recall/scripts/recall.py "$QUERY" --limit 10 --format markdown
   ```
3. **Inspect results** — each result has `[lrn-id]`, key insight, and how-to-apply.
4. **Fetch full docs if needed** — for any interesting learning ID, the user can
   run `reflect search <id>` or check the reflect repo's `documents/` dir
   (`~/.claude/global-learnings/documents/` by default).

## Query construction tips

- Short, focused queries beat long sentences (the backend does vector similarity).
- Include proper nouns: project names, tool names, error snippets.
- Add tags explicitly with `--tags a,b,c` for reranking boost.

## Backend details

- **Retrieval**: wraps the `reflect search` CLI (from `reflect-kb`,
  install via `uv tool install reflect-kb`) as a subprocess. Resolved via
  `shutil.which("reflect")`; falls back to the legacy
  `~/.learnings/cli/learnings` only if the canonical CLI is missing.
- **Ranking**: `CE × confidence_boost × recency_boost × tag_boost × proof_boost`
  — cross-encoder relevance is primary; each secondary signal is a
  multiplicative boost `1 + α·(norm − 0.5)` bounded to ±α/2 (Hindsight
  shape, port R8), so no single signal can dominate.
  - Confidence (α=0.2, ±10%): HIGH=1.0, MEDIUM=0.5 (neutral), LOW=0.0
  - Recency (α=0.2, ±10%): linear decay over 365 days → [0.1, 1.0],
    neutral 0.5 when undated
  - Tags (α=0.2, ±10%): query-tag coverage fraction, neutral without tags
  - Proof count (α=0.1, ±5%): clamp(0.5 + ln(count)/10, 0, 1)
  - Tune each α via `RECALL_CONFIDENCE_ALPHA` / `RECALL_RECENCY_ALPHA` /
    `RECALL_TAG_ALPHA` / `RECALL_PROOF_ALPHA`
- **Cache**: per-query SHA1 hash at `~/.reflect/recall_cache/`, 1h TTL.
- **Log**: every recall is appended to `~/.reflect/recall_log.jsonl` for
  future helpfulness analysis (Phase 6 of the retrieval plan).

## Related

- `/reflect:ingest` — populate the KB
- `/reflect-status` — KB health, coverage, pending reviews
- SessionStart hook — auto-recall on project entry (see `hooks/settings-snippet.json`)
