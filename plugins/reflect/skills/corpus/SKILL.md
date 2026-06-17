---
name: corpus
description: >-
  Build a long-lived, filtered SLICE of the global learnings KB and answer a
  question-set over it ("ask the auth subsystem", "ask the migration log").
  Unlike /reflect:recall (a fresh hybrid query each time), a corpus is a saved
  filter snapshotted to disk — prime it once, ask many questions, reprime when
  the KB drifts. Use when the user wants a durable Q&A session against one
  filtered area of their own code history rather than a one-shot search.
---

# /reflect:corpus — Knowledge-corpus Q&A (build → prime → query → reprime)

reflect-kb is search-shaped: every `/reflect:recall` is a fresh hybrid query.
A **corpus** is the complementary pattern — a long-lived, filtered slice of the
user's own learnings that you hold open and answer a question-set against.

You (the agent) ARE the Q&A session. There is no SDK subprocess: the build step
snapshots the filtered learnings to a JSON file and prints a primed context
document; you read that document and answer questions over it.

## When to use

- The user wants to "ask the auth subsystem", "ask the migration log", "query
  everything tagged X" as a sustained back-and-forth — not a single search.
- A filtered slice (tag / category / project / date-window) of the KB is the
  unit of conversation.

For a one-shot retrieval, use `/reflect:recall` instead.

## Filter spec

Space-separated `key:value` tokens. Bare tokens are treated as tags. All
predicates AND together; `tag:` may repeat (or use `tag:a,b`) to OR tags.

| token | meaning |
|-------|---------|
| `tag:auth` / `auth` | learning carries this tag (case-insensitive) |
| `category:security` | frontmatter `category` equals this |
| `project:api` | frontmatter `project_id`/`project` equals this |
| `since:2026-01-01` | learning `created`/archived date ≥ this (inclusive) |
| `until:2026-06-30` | learning `created`/archived date ≤ this (inclusive) |

## Build / prime

Run the corpus builder via the recall script (no embedding model loads — the
filter is pure frontmatter logic):

```bash
plugins/reflect/skills/recall/scripts/recall.py \
  --corpus auth-subsystem \
  --corpus-filter "tag:auth category:security project:api" \
  --format markdown
```

This snapshots every matching learning to
`$REFLECT_STATE_DIR/corpora/auth-subsystem.json` (default
`~/.reflect/corpora/`) — persisting the filter, a last-built timestamp, and the
KB mtime — and prints the **primed context document**.

**Then: read the printed document and answer the user's questions using ONLY
the learnings in it.** Treat it as the full context for this corpus session.

## Reprime on drift

The KB drifts as the user works (new learnings ingested, old ones archived).
Re-run the saved filter — no need to restate it:

```bash
plugins/reflect/skills/recall/scripts/recall.py \
  --corpus auth-subsystem --corpus-rebuild --format json
```

`--corpus-rebuild` re-applies the persisted filter against the current KB:
newly-matching learnings are pulled in, entries that no longer match (or were
deleted) are dropped. The JSON form reports `"stale": true/false` — `stale`
means the KB has been written since the snapshot was built, i.e. it's time to
reprime before answering further.

## Lifecycle (claude-mem KnowledgeAgent / CorpusBuilder, adapted)

1. **build** — `build_corpus(filter)` runs the filter, writes the snapshot.
2. **prime** — read the printed primed document; that is your Q&A context.
3. **query** — answer the user's questions over the primed document.
4. **reprime** — on KB drift (`stale`), `--corpus-rebuild` and re-read.

The deterministic build/filter/snapshot/reprime engine lives in
`reflect-kb/src/reflect_kb/recall/corpus.py`; the LLM-driven Q&A is yours.
