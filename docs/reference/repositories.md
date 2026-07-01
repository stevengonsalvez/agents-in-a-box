---
title: "Repositories"
---

The agents-in-a-box ecosystem spans three repositories. Skills/installer/catalog
live in their own standalone repo; the `reflect` long-term-memory system lives
in its own public repo; the `ainb` tool consumes both as external sources.

## The repos

| Repo | What it holds | Consumed how |
|---|---|---|
| **[stevengonsalvez/agents-in-a-box](https://github.com/stevengonsalvez/agents-in-a-box)** | The `ainb` TUI/CLI unit manager (Rust workspace under `ainb-tui/`), the v2 JSON-RPC plugin system, and this documentation site. | — |
| **[stevengonsalvez/ainb-toolkit](https://github.com/stevengonsalvez/ainb-toolkit)** | The canonical home for the curated **skills** (`skills/`), **agents** (`agents/`), **workflows** (`workflows/`), **utilities** (`utilities/`), per-tool rule layouts, the `external-dependencies.yaml` manifest, the legacy `bootstrap.js` installer, and the generated `catalog.yaml`. Flattened at the repo root. | `ainb` browses + installs from it as a pinned external source; the agents-in-a-box release CI clones a pinned tag of it to generate the curated `catalog-index.json` release asset. |
| **[stevengonsalvez/ainb-reflect-memory](https://github.com/stevengonsalvez/ainb-reflect-memory)** | The `reflect` long-term-memory system — the Python retrieval/GraphRAG engine (at the repo root) plus its Claude plugin (under `plugin/`). Extracted out of agents-in-a-box into its own public repo. | Engine installs via `uv tool install --upgrade 'git+https://github.com/stevengonsalvez/ainb-reflect-memory.git[graph]'`; the plugin ships from the repo's `plugin/` dir; `ainb reflect bootstrap` installs the engine from that URL. |

## How they fit together

```
┌────────────────────────────┐        pinned external source        ┌──────────────────────────┐
│ agents-in-a-box            │  ─────────────────────────────────▶  │ ainb-toolkit             │
│  • ainb (Rust TUI/CLI)     │   gh:stevengonsalvez/ainb-toolkit    │  • skills/  agents/      │
│  • v2 plugin system        │       @<tag>/skills/<name>           │  • workflows/ utilities/ │
│  • docs (this site)        │                                      │  • bootstrap.js          │
│                            │  ◀─ catalog-index.json (release CI)  │  • external-deps.yaml    │
└────────────────────────────┘     clone @tag → xtask → asset       │  • catalog.yaml          │
                                                                    └──────────────────────────┘
```

- **Browsing & installing skills** — `ainb skill browse "" --catalog ainb` reads
  the `catalog-index.json` published as an agents-in-a-box release asset. Each
  owned entry's install URI is `gh:stevengonsalvez/ainb-toolkit@<tag>/skills/<name>`,
  pinning the ainb-toolkit ref the catalog was generated from.
- **Authoring a skill or agent** — open a PR against **ainb-toolkit**, then
  regenerate its catalog with `bash bin/generate-catalog.sh`.
- **The reflect long-term-memory system was extracted to its own repo,
  [`stevengonsalvez/ainb-reflect-memory`](https://github.com/stevengonsalvez/ainb-reflect-memory)** —
  the retrieval/GraphRAG engine sits at that repo's root and its Claude plugin
  under `plugin/`. It is no longer in agents-in-a-box's marketplace; the engine
  installs via
  `uv tool install --upgrade 'git+https://github.com/stevengonsalvez/ainb-reflect-memory.git[graph]'`,
  and `ainb reflect bootstrap` installs it from that URL.

## Pinning

The agents-in-a-box release CI pins a specific ainb-toolkit tag (`AINB_TOOLKIT_REF`
in `.github/workflows/release.yml`). Bump it when adopting a new ainb-toolkit
release so the published catalog and its install URIs move together.
