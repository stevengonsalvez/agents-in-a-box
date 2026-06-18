---
title: "Repositories"
---

The agents-in-a-box ecosystem spans two repositories. Skills/installer/catalog
live in their own standalone repo; the `ainb` tool consumes it as a pinned
external source.

## The two repos

| Repo | What it holds | Consumed how |
|---|---|---|
| **[stevengonsalvez/agents-in-a-box](https://github.com/stevengonsalvez/agents-in-a-box)** | The `ainb` TUI/CLI unit manager (Rust workspace under `ainb-tui/`), the v2 JSON-RPC plugin system, the `reflect` knowledge plugin + `reflect-kb` retrieval CLI, and this documentation site. | — |
| **[stevengonsalvez/ainb-toolkit](https://github.com/stevengonsalvez/ainb-toolkit)** | The canonical home for the curated **skills** (`skills/`), **agents** (`agents/`), **workflows** (`workflows/`), **utilities** (`utilities/`), per-tool rule layouts, the `external-dependencies.yaml` manifest, the legacy `bootstrap.js` installer, and the generated `catalog.yaml`. Flattened at the repo root. | `ainb` browses + installs from it as a pinned external source; the agents-in-a-box release CI clones a pinned tag of it to generate the curated `catalog-index.json` release asset. |

## How they fit together

```
┌────────────────────────────┐        pinned external source        ┌──────────────────────────┐
│ agents-in-a-box            │  ─────────────────────────────────▶  │ ainb-toolkit             │
│  • ainb (Rust TUI/CLI)     │   gh:stevengonsalvez/ainb-toolkit    │  • skills/  agents/      │
│  • v2 plugin system        │       @<tag>/skills/<name>           │  • workflows/ utilities/ │
│  • reflect plugin + kb     │                                      │  • bootstrap.js          │
│  • docs (this site)        │  ◀─ catalog-index.json (release CI)  │  • external-deps.yaml    │
└────────────────────────────┘     clone @tag → xtask → asset       │  • catalog.yaml          │
                                                                    └──────────────────────────┘
```

- **Browsing & installing skills** — `ainb skill browse "" --catalog ainb` reads
  the `catalog-index.json` published as an agents-in-a-box release asset. Each
  owned entry's install URI is `gh:stevengonsalvez/ainb-toolkit@<tag>/skills/<name>`,
  pinning the ainb-toolkit ref the catalog was generated from.
- **Authoring a skill or agent** — open a PR against **ainb-toolkit**, then
  regenerate its catalog with `bash bin/generate-catalog.sh`.
- **The reflect plugin and reflect-kb stay in agents-in-a-box** (`plugins/reflect/`,
  `reflect-kb/`) — they are a knowledge engine, not portable skills, and are not
  mirrored into ainb-toolkit.

## Pinning

The agents-in-a-box release CI pins a specific ainb-toolkit tag (`AINB_TOOLKIT_REF`
in `.github/workflows/release.yml`). Bump it when adopting a new ainb-toolkit
release so the published catalog and its install URIs move together.
