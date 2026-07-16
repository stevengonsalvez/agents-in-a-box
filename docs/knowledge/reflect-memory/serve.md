---
title: "Memory browser (reflect serve)"
description: "reflect serve — a local, loopback-only web app to browse, search, graph, and curate the reflect knowledge base."
---

`reflect serve` is a local web utility for browsing, searching, and curating the reflect knowledge
base. It ships with the CLI documented on the [`reflect` CLI reference](/knowledge/reflect-cli/); for
the engine's recall pipeline see [Recall reference](/knowledge/reflect-memory/recall/).

```bash
reflect serve                       # http://127.0.0.1:8377, KB at $GLOBAL_LEARNINGS_PATH or ~/.learnings
reflect serve --port 8942           # pick a port
reflect serve --repo /path/to/kb    # browse a specific KB
```

To reach it from another device on your tailnet, front it with a proxy — the server itself never
leaves loopback:

```bash
tailscale serve --bg --https 8942 http://127.0.0.1:8942
```

## What it does

- **Browse** — a faceted memory list (confidence · type · scope · tags/projects), sortable by
  newest, computed recall score, or title.
- **Search** — lexical BM25 ranking; each result carries a match score and a browse-ordering score
  (confidence × recency × tag overlap). This is a browse heuristic, **not** the recall reranker's
  score — the real reranker (see [R2/R3 in the recall reference](/knowledge/reflect-memory/recall/))
  deliberately dropped exp-decay recency. Full semantic search stays on the `reflect search` CLI.
- **Graph** — a two-layer force graph of memory and entity nodes; edge widths encode graphml
  relation weights. Toggle the entity layer, drag/pan/zoom, click a memory to open it.
- **Timeline** — memories grouped chronologically.
- **Superseded** — `superseded_by` chains rendered newest tip to oldest ancestor.
- **Curate** — soft-archive/restore (reversible, no hard delete), edit a note's confidence, and
  multi-select memories to queue a group for compression by the `/reflect` consolidate skill
  (written to a versioned `compress-queue.yaml` — the web app never calls an LLM).
- **Stats** — confidence/type/scope/tag distributions and engine-op usage counts.

Light and dark themes throughout.

### Weightages surfaced

| Weightage | Where |
|---|---|
| Editable confidence | segmented control in the detail drawer |
| Computed browse score | on every card and in the drawer |
| Graph edge weights | edge width in the Graph view |
| Recall usage stats | engine-op counts in the Stats view |

## Curation is metadata-only and file-first

Mutations edit the markdown note (the source of truth) and the browser view reloads immediately.
The nano-graphrag cache used by `reflect search` is **not** rebuilt synchronously — the engine
only supports a full-batch reindex — so after archiving or editing confidence, run
`reflect reindex` to refresh semantic search and the entity graph. The UI hints at this. Note
bodies stay agent-authored; only metadata is editable here. Postgres backends are read-only in
this release.

Soft-archive moves the note into an `archived/` sibling directory. This is the browser's own
reversible delete and is **separate** from the forget sweep's `.forgotten/` directory (see
[A3 in the recall reference](/knowledge/reflect-memory/recall/)), which the sweep manages with its
own database accounting. Archiving here does not run the sweep or touch its records.

## Loopback only — no authentication

Curation is guarded: the server rejects any request whose `Host` isn't loopback (defeats
DNS-rebinding) and requires an `X-Reflect` header on mutations, so only the same-origin SPA can
drive them. There is **no auth** — do not bind a public interface. To reach it across your
tailnet, use the `tailscale serve` proxy shown above rather than exposing the port directly.

## Screenshots

![Memory list, light theme](../../assets/screenshots/reflect-serve-memories-light.png)
![Memory list, dark theme](../../assets/screenshots/reflect-serve-memories-dark.png)
![Detail drawer with curation controls](../../assets/screenshots/reflect-serve-curation-drawer.png)
![Two-layer graph with weighted edges](../../assets/screenshots/reflect-serve-graph.png)
![Superseded chains](../../assets/screenshots/reflect-serve-superseded.png)
![Multi-select to queue for compression](../../assets/screenshots/reflect-serve-compress-select.png)

## Source

[`src/reflect_kb/serve.py`](https://github.com/stevengonsalvez/ainb-reflect-memory/blob/main/src/reflect_kb/serve.py)
and the e2e suite under
[`tests/e2e/`](https://github.com/stevengonsalvez/ainb-reflect-memory/tree/main/tests/e2e) in
[stevengonsalvez/ainb-reflect-memory](https://github.com/stevengonsalvez/ainb-reflect-memory). Full
guide: [`docs/reflect-serve.md`](https://github.com/stevengonsalvez/ainb-reflect-memory/blob/main/docs/reflect-serve.md).
