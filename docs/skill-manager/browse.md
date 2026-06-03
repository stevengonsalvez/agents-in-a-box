---
title: Catalog browse
description: Search the skills catalog and install from the TUI or CLI.
---

> Browse a remote skill catalog (skills.sh) before installing — the
> single biggest UX gap vs other skill managers. See
> `.agents/plans/skill-manager-v1.3-tdd.md` (bead `ai-a20`) for the full
> spec this document summarises.

## Why browse exists

Before v1.3 you had to know a unit's exact `gh:owner/repo@ref/path` URI
before `ainb skill install` could touch it. Browse closes that gap: a
ranked, searchable view of a remote catalog, with one-key install from
the SkillManager TUI. **Results are ephemeral** — there is no SQLite, no
cache file; a search returns a `Vec<CatalogHit>` that the caller renders
and (on install) routes through the existing install flow.

## Architecture

```text
┌─────────────────┐     ┌───────────────────────────┐
│ CatalogBackend  │◀────│ SkillsShHttpBackend (prod) │ reqwest → skills.sh
│ (trait, Send+   │     ├───────────────────────────┤
│  Sync)          │◀────│ MockCatalogBackend (tests) │ canned Vec<CatalogHit>
│  fn search(q)   │     └───────────────────────────┘
└─────────────────┘
        │
        ▼
   Vec<CatalogHit { name, repo, stars, install_uri, description }>
```

The trait boundary is the whole point: **every test injects the mock, so
the network is never touched in CI.** Mirrors the `DriftBackend` design.

## `ainb skill browse` CLI

```bash
ainb skill browse <query>           # ranked table (name / stars / repo / uri)
ainb skill browse <query> --json    # [{name, repo, stars, install_uri, ...}]
ainb skill browse ""                # empty query → a hint, not an error
```

Read-only. The default output is a fixed-width table ranked by GitHub
stars (descending); `--json` emits the raw `Vec<CatalogHit>` for
scripting.

## TUI `[b]` browse modal

On the SkillManager screen:

```text
[b]  → open the browse modal (Query mode)
type → the query buffer (Enter searches)
↑↓   → select a result (Results mode)
Enter→ install the selected hit (source add + skill install)
/    → back to Query mode to refine
Esc  → close (results discarded — ephemeral)
```

Enter on a result routes the hit's `install_uri` through the existing
install flow exactly like the CLI: derive the source URI, `ainb source
add` it (idempotent — "already exists" is fine), then `ainb skill install
<uri> --yes`. On success the modal closes and the Units table reloads.

## API key (optional)

skills.sh may gate search behind a credential. The key is resolved in
this order (first hit wins):

| Order | Source |
|-------|--------|
| 1 | `AINB_SKILLS_API_KEY` environment variable |
| 2 | `[skills].api_key` in `~/.agents-in-a-box/config/config.toml` |
| 3 | none — search is attempted unauthenticated |

```toml
# ~/.agents-in-a-box/config/config.toml
[skills]
api_key = "sk-your-skills-sh-key"
```

The key travels in an `Authorization: Bearer <key>` header — it is
**never** placed in the request URL. When skills.sh returns `401`/`403`
the backend surfaces `CatalogError::AuthRequired` with the message
*"set AINB_SKILLS_API_KEY or [skills].api_key in config.toml"*.

## Test / offline escape hatches

These keep the **live binary** offline (CI never hits skills.sh):

| Env var | Effect |
|---------|--------|
| `AINB_CATALOG_MOCK=1` | `search()` returns canned hits — no socket, no reqwest client. The `[b]` TUI tripwire sets this. |
| `AINB_SKILLS_API_BASE=<url>` | Reroute the catalog base host at a local stub (e.g. a `wiremock` server). |
| `AINB_CATALOG_MOCK_INSTALL_URI=<uri>` | Replace the top canned hit's `install_uri` — a tripwire points it at a local `git:file://…` bare remote so `[b] → Enter` install fetches locally. |

Because the TUI builds the backend on the tokio event-loop thread, the
`reqwest::blocking::Client` is built **lazily** inside the network branch
of `search()` (constructing a blocking client inside a tokio runtime
panics). In mock mode `search()` returns before ever touching reqwest.

## Acceptance tripwire

`tripwire_core_skill_manager_browse_live.rs` drives the real `ainb`
binary against the Full-tier sandbox via tmux: `m → b → type query →
Enter (results) → Enter (install)`. It sets `AINB_CATALOG_MOCK=1` and
points `AINB_CATALOG_MOCK_INSTALL_URI` at a second local bare remote
seeded with `browse-demo-skill`, then asserts the manifest gains that
source and `lock.yaml` records the deployed unit — **zero network**.
Skips cleanly when `tmux` is unavailable.
