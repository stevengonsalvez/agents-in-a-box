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
- **Ranking**: `confidence × recency × (1 + tag_overlap_bonus)`.
  - Confidence: HIGH=1.0, MEDIUM=0.7, LOW=0.4
  - Recency: exp(-days_ago / 90), half-life ~60 days
  - Tag bonus: 0.1 × count(query_tags ∩ learning_tags)
- **Cache**: per-query SHA1 hash at `~/.reflect/recall_cache/`, 1h TTL.
- **Log**: every recall is appended to `~/.reflect/recall_log.jsonl` for
  future helpfulness analysis (Phase 6 of the retrieval plan).

## Related

- `/reflect:ingest` — populate the KB
- `/reflect-status` — KB health, coverage, pending reviews
- SessionStart hook — auto-recall on project entry (see `hooks/settings-snippet.json`)
