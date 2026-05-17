# reflect CLI — Per-Subcommand Reference

reflect-kb v0.1.1. All subcommands write to stderr for diagnostic output and to stdout for machine-parseable
results (`--format json`). Run `reflect --version` to confirm your installed version.

**Table of contents**

- [`reflect init`](#reflect-init)
- [`reflect add`](#reflect-add)
- [`reflect search`](#reflect-search)
- [`reflect reindex`](#reflect-reindex)
- [`reflect stats`](#reflect-stats)
- [`reflect critical-patterns`](#reflect-critical-patterns)
- [`reflect generate-sidecars`](#reflect-generate-sidecars)
- [`reflect metrics stats`](#reflect-metrics-stats)
- [`reflect timeline`](#reflect-timeline)

---

## `reflect init`

Initialise the knowledge base on a new machine.

### Synopsis

```
reflect init
```

### What it does

Creates the directory structure under `~/.claude/global-learnings/` (or `$GLOBAL_LEARNINGS_PATH` if set):

```
~/.claude/global-learnings/
├── documents/            # learning .md files land here
└── nano_graphrag_cache/  # graph index (auto-populated by add/reindex)
```

Also runs `git init` and writes a `.gitignore` that excludes the graph cache. Safe to re-run — if the
directory already exists the command is a no-op.

### Examples

```bash
# First-time setup on a new machine
reflect init

# With a custom KB path
GLOBAL_LEARNINGS_PATH=~/team-kb reflect init
```

### Notes

- `reflect init` must be run before any other subcommand on a fresh install.
- The KB path is resolved at runtime from `$GLOBAL_LEARNINGS_PATH`. If unset, the default
  `~/.claude/global-learnings/` is used by every subcommand.
- The `.gitignore` excludes `nano_graphrag_cache/` — the graph index is rebuilt on demand
  via `reflect reindex` and does not need to be committed.

---

## `reflect add`

Add a learning document to the knowledge base.

### Synopsis

```
reflect add [OPTIONS] FILE_PATH
```

### Flags

| Flag | Short | Default | Description |
|---|---|---|---|
| `--entities PATH` | `-e` | — | Path to a `.entities.yaml` sidecar with pre-extracted entities |
| `--force` | `-f` | false | Overwrite an existing document without prompting |

### What it does

1. Reads the source `.md` file and parses YAML frontmatter.
2. Validates that `title`, `category`, and `key_insight` fields are present.
3. Generates a stable `doc_id` = `slug(title) + sha256(title + body)[:6]` (see content-hash note below).
4. Copies the file to `documents/<doc_id>.md`.
5. Loads or auto-generates an `.entities.yaml` sidecar.
6. Inserts the document into the GraphRAG index.
7. Syncs the QMD (quantised multi-doc) index if `qmd` is on `$PATH`.

### Content-hash `doc_id` behaviour

The `doc_id` is derived from both the document title and its body:

```
doc_id = slug(title) + "-" + sha256(title + "\n" + body)[:6]
```

| Scenario | Result |
|---|---|
| Same title, same body | Same `doc_id` — idempotent re-ingest, safe to re-add |
| Same title, different body | Different `doc_id` — treated as a distinct document |
| Different title (even with similar body) | Different `doc_id` — no collision |

This replaced the previous title-only hash, which caused silent collisions when two documents shared
a slug-able title (e.g. capitalisation or punctuation differences that collapse in slugification).

### `--force` flag and the non-TTY guard

When a document with the same `doc_id` already exists in the KB:

- **TTY (interactive shell):** prompts `Document <id>.md exists. Overwrite? [y/N]`
- **Non-TTY (subprocess / ingest pipeline) without `--force`:** exits with code 2 and prints:

  ```
  Error: document <id>.md already exists and stdin is not a TTY.
  Re-run with --force to overwrite, or update the source title/body so the generated id differs.
  ```

- **With `--force`:** overwrites silently regardless of TTY state.

Use `--force` in any automated context (CI, drain script, batch ingest) to avoid silent failures.
The old behaviour (calling `click.confirm` from a non-TTY) silently aborted, dropping files from
the ingest queue without any error.

### Document frontmatter

Documents must include at minimum:

```yaml
---
title: "Descriptive title of the learning"
category: "architecture-decisions"   # or patterns, debugging, tooling, ...
key_insight: "One-sentence summary of what was learned"
---

Body of the learning document...
```

Optional fields recognised by search and `critical-patterns`:

```yaml
confidence: high        # high | medium | low
language: rust          # programming language filter
tags:
  - performance
  - async
```

### Examples

```bash
# Add a document, auto-generate entity sidecar
reflect add ./learning-2024-tokio-fix.md

# Add with a pre-extracted entity sidecar
reflect add ./learning.md --entities ./learning.entities.yaml

# Non-interactive overwrite (for scripts and ingest pipelines)
reflect add --force ./learning.md

# Add to a custom KB path
GLOBAL_LEARNINGS_PATH=~/team-kb reflect add ./shared-learning.md
```

### Notes / gotchas

- The command exits non-zero (code 1) if required frontmatter fields are missing.
- Graph indexing failure (e.g. missing `[graph]` extra) is a warning, not a hard error — the document
  is still saved. Run `reflect reindex` afterwards.
- QMD sync can take up to 2 minutes on large KBs — progress is printed to stderr.
- Auto-generated sidecars use heuristic extraction (no LLM). If the document is very short or generic,
  no sidecar is written and the document contributes to vector search only.

---

## `reflect search`

Search the knowledge base using hybrid GraphRAG + vector retrieval.

### Synopsis

```
reflect search [OPTIONS] QUERY
```

### Flags

| Flag | Short | Default | Description |
|---|---|---|---|
| `--mode MODE` | `-m` | `naive` | Search mode: `naive`, `local`, or `global` |
| `--tags TEXT` | `-t` | — | Filter by tags (comma-separated, appended to query) |
| `--category TEXT` | `-c` | — | Filter by category (appended to query) |
| `--limit INT` | `-l` | `10` | Maximum number of results |
| `--format FORMAT` | `-f` | `rich` | Output format: `rich`, `json`, or `simple` |

### Search modes

| Mode | Technique | Best for |
|---|---|---|
| `naive` | Vector similarity only (fast) | Exact symptom matching, specific error messages |
| `local` | Entity neighbourhood graph traversal | Finding related concepts and patterns |
| `global` | Community-based search across all learnings | Broad themes, cross-cutting patterns |

### Examples

```bash
# Basic search (vector similarity, rich output)
reflect search "tokio runtime panic"

# Entity-neighbourhood search for related concepts
reflect search "async timeout" --mode local

# Filter by tags
reflect search "n+1 query" --tags rust,performance

# Filter by category
reflect search "database migration" --category architecture-decisions

# Machine-readable output for scripting
reflect search "connection pool sizing" --format json

# Minimal text output (for piping)
reflect search "retry strategy" --format simple

# Combine filters
reflect search "cache invalidation" --tags redis --limit 5
```

### Notes / gotchas

- If the graph index does not exist, `naive` mode falls back to scanning document frontmatter.
  Run `reflect reindex` first for full GraphRAG fidelity.
- `--tags` and `--category` filters are appended to the query string, not applied as strict predicates —
  they bias retrieval rather than restrict it.
- Latency is written to the metrics JSONL log after each call. View aggregates with `reflect metrics stats`.
- `--format json` writes to stdout; `--format rich` and `--format simple` write to stderr.

---

## `reflect reindex`

Rebuild the GraphRAG index from all documents.

### Synopsis

```
reflect reindex [OPTIONS]
```

### Flags

| Flag | Default | Description |
|---|---|---|
| `--force` | false | Clear the graph cache before rebuilding |

### What it does

1. Reads all `.md` files in `documents/`.
2. Auto-generates missing `.entities.yaml` sidecars (heuristic extraction, no LLM).
3. Builds a batch of `(text, entities)` tuples.
4. Calls `engine.insert_documents_batch(batch)` — batch mode avoids nano-graphrag state issues
   with sequential inserts (community reports dropped, KV persistence skipped).

### Examples

```bash
# Incremental rebuild (add missing docs to existing index)
reflect reindex

# Full rebuild from scratch (clears cache first)
reflect reindex --force
```

### Notes / gotchas

- Batch indexing is preferred over sequential inserts. Calling `reflect add` for every document
  individually can produce an inconsistent graph; use `reflect reindex` after a bulk import.
- `--force` deletes the `nano_graphrag_cache/` directory before rebuilding. On a large KB this
  can take several minutes.
- Run after `reflect generate-sidecars` to incorporate newly generated entity data.

---

## `reflect stats`

Show statistics about the knowledge base.

### Synopsis

```
reflect stats
```

### What it shows

Three Rich tables printed to stderr:

1. **Knowledge Base Statistics** — total documents, repository path, graph entity count,
   graph relationship count, docs-with-entities ratio.
2. **By Category** — document count per category, sorted descending.
3. **By Confidence** — document count per confidence level.

### Examples

```bash
reflect stats
```

### Notes

- If the graph index has not been built, the graph entity and relationship counts show `Not initialized`.
- `reflect stats` is a read-only inspection command — it never modifies the KB.

---

## `reflect critical-patterns`

Surface high-confidence, widely-applicable patterns from the KB.

### Synopsis

```
reflect critical-patterns [OPTIONS]
```

### Flags

| Flag | Short | Default | Description |
|---|---|---|---|
| `--language TEXT` | `-l` | — | Filter by programming language |
| `--domain TEXT` | `-d` | — | Filter by domain keyword (matches body text and tags) |

### What it does

Filters documents where `confidence == "high"` and `category` is either `architecture-decisions`
or `patterns`. Renders each match as a Rich panel showing title and `key_insight`.

### Examples

```bash
# All critical patterns across all languages and domains
reflect critical-patterns

# Rust-specific patterns
reflect critical-patterns --language rust

# Backend patterns
reflect critical-patterns --domain backend

# Combined filters
reflect critical-patterns --language python --domain async
```

### Notes / gotchas

- Only documents with `confidence: high` in their frontmatter appear here.
- `--domain` matches against the document body and tags — it is a fuzzy filter, not a strict predicate.
- If no patterns match, a yellow "No critical patterns found" message is printed. This usually means
  the KB has no documents with `confidence: high` yet.

---

## `reflect generate-sidecars`

Backfill missing `.entities.yaml` sidecars using heuristic extraction.

### Synopsis

```
reflect generate-sidecars [OPTIONS]
```

### Flags

| Flag | Default | Description |
|---|---|---|
| `--force` | false | Regenerate all sidecars, replacing existing ones |

### What it does

Iterates every document in `documents/`. For each document that is missing a `.entities.yaml` sidecar
(or all documents when `--force` is set), runs heuristic entity extraction and writes the sidecar
alongside the document. No LLM is required — extraction is purely text-based.

Output shows per-document entity and relationship counts, plus a summary:

```
  My Learning Title - 4 entities, 6 relationships
  ...
Results:
  Generated: 12
  Skipped:   3
```

After running, call `reflect reindex` to incorporate the new entity data into the graph.

### Examples

```bash
# Generate sidecars for documents that don't have one yet
reflect generate-sidecars

# Regenerate all sidecars from scratch
reflect generate-sidecars --force

# Then rebuild the graph to use the new sidecars
reflect reindex
```

### Notes / gotchas

- Very short or generic documents may yield zero entities. These are skipped (counted as "Skipped").
- Existing sidecars are preserved unless `--force` is passed.
- This does not modify the graph index — run `reflect reindex` afterwards.

---

## `reflect metrics stats`

Aggregate the recall-metrics JSONL log.

### Synopsis

```
reflect metrics stats [OPTIONS]
```

`metrics` is a subcommand group. `stats` is currently its only sub-subcommand.

### Flags

| Flag | Default | Description |
|---|---|---|
| `--metrics-path FILE` | `~/.learnings/metrics.jsonl` | Override the metrics file path |
| `--format [table\|json]` | `table` | Output format |
| `--window-days INT` | `7` | Rolling window for the "last N days" table |

### What it shows

Two tables (or a JSON object with `--format json`):

- **Last N days** — total events, recall events, recall with hits, hit rate %, p50 latency (ms),
  p95 latency (ms), top tags.
- **All time** — same metrics over the full log.

### Examples

```bash
# Default: last 7 days as Rich table
reflect metrics stats

# Custom rolling window
reflect metrics stats --window-days 30

# Machine-readable JSON
reflect metrics stats --format json

# Custom metrics file (e.g. a team-shared log)
reflect metrics stats --metrics-path /shared/metrics.jsonl
```

### Notes / gotchas

- The metrics file is written by `reflect search` and `reflect add` after each operation.
  If it does not exist yet, the command prints empty tables.
- The file rotates automatically at 10 MB (handled by `reflect_kb.metrics`). Historical data before
  the last rotation is not included.
- `--format json` writes to stdout; `--format table` writes to the Rich console (stderr).

---

## `reflect timeline`

Show or drill into the reflect statusline dashboard.

### Synopsis

```
reflect timeline [OPTIONS]
```

### Flags

| Flag | Default | Description |
|---|---|---|
| `--explain TEXT` | — | Drill-down for a single row or `all`. Values: `REC`, `MEM`, `ING`, `DRN`, `TOK`, `ERR`, `COM`, `AGT`, `all` |

### What it does

**Without `--explain`:** prints a usage hint:

```
Live dashboard renders on your Claude Code statusline.
Run `reflect timeline --explain <ROW>` for drill-down. ROW = REC|MEM|ING|DRN|TOK|ERR|COM|AGT|all
```

**With `--explain ROW`:** delegates to the reflect plugin's `reflect_timeline.sh` helper script,
discovered in this order:

1. `$CLAUDE_PLUGIN_ROOT/scripts/reflect_timeline.sh`
2. Highest-versioned path matching
   `~/.claude/plugins/cache/agents-in-a-box/reflect/*/scripts/reflect_timeline.sh`

The helper renders a plain-text drill-down for the requested row over the last 2 hours.

### Row codes

| Code | Statusline signal |
|---|---|
| `REC` | Recall events (session-start context injections) |
| `MEM` | Memory operations |
| `ING` | Ingest events (documents added) |
| `DRN` | Drain operations (queue flush) |
| `TOK` | Token usage per turn |
| `ERR` | Pipeline errors |
| `COM` | Compact events |
| `AGT` | Agent subcommand events |
| `all` | All rows combined |

### Examples

```bash
# Print usage hint (no plugin invocation)
reflect timeline

# Drill into token usage for the last 2 hours
reflect timeline --explain TOK

# Full drill-down across all signals
reflect timeline --explain all

# Drill into error events
reflect timeline --explain ERR
```

### Notes / gotchas

- `reflect timeline --explain` requires the agents-in-a-box reflect plugin to be installed:
  `claude plugin install reflect@agents-in-a-box`. If the plugin is not found, the command
  prints an install hint and exits with a non-zero code.
- The live dashboard itself is rendered by the plugin's statusline hook, not by this command.
  `reflect timeline` is the *drill-down* companion — it expands a single row.
- `$CLAUDE_PLUGIN_ROOT` can be set to point at a development checkout of the plugin, which is
  useful when iterating on the timeline script itself.
