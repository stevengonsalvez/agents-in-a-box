# Design Spec — `learnings` plugin + per-plugin config framework

**Status:** brainstorm output, pending approval → `/plan`
**Date:** 2026-06-02
**Author:** design session with Stevie

## Goal

A first-class **memory / knowledge-base browser** as an ainb-tui v2 native plugin:
browse, semantic-search, graph-explore, and read the reflect learnings KB (the
`~/.learnings` notes + the QMD vector index + the nano_graphrag graph). Plus the
reusable infrastructure it surfaces: **generic per-plugin configuration** in
config.toml *and* the TUI Settings screen.

## Locked decisions (from design interview)

| # | Decision | Choice |
|---|----------|--------|
| 1 | Data access to the 3 sources | **Hybrid** — browse/detail read files directly; search shells `qmd`/`learnings`; graph reads the graphml/community json directly |
| 2 | Plugin shape | **Single self-contained** plugin (owns read + search + render) |
| 3 | v1 capabilities | **All four**: Browse + filter chips · Semantic search · Graph view · Detail/read pane |
| 4 | Linking | **One KB, three configurable paths** (supersedes earlier "multiple linked KBs" — no KB-management UI, no list editor in v1; multi-KB is a future array extension) |
| 5 | FS access | **Add a `read_paths` capability** (path-scoped grant) to the protocol |
| 6 | Plugin config | **Generic flat per-plugin config** surfaced in BOTH config.toml `[plugins.<name>]` AND the Settings screen; injected to the plugin at `plugin/init` |

## What we're NOT doing (v1)

- No multi-KB management / KB switcher / aggregation UI.
- No `list<record>` settings widget — plugin config is **flat scalars only**.
- No per-plugin custom config screens (config is schema-driven + generic).
- No write/edit of learnings from the browser (read-only viewer).
- No reflect capture/ingest changes — this consumes the KB, doesn't produce it.
- **No CLI namespace** (`ainb memory …`) — TUI screen + slash commands only;
  verification is tmux-ui-tripwire (no cheap CLI parity leg).

---

## Part A — Per-plugin config framework (reusable infra)

The learnings plugin needs three configurable paths. Rather than a one-off config
file, build the generic mechanism every future plugin reuses.

### A1. Manifest `[config]` schema (flat scalars)

Plugins declare their user-configurable variables in `manifest.toml`:

```toml
[[config]]
key     = "learnings_dir"
kind    = "path"                 # path | string | bool | enum | int
label   = "Learnings notes directory"
default = "~/.learnings/documents/learnings"

[[config]]
key     = "qmd_index"
kind    = "path"
label   = "QMD index (sqlite)"
default = "~/.cache/qmd/index.sqlite"

[[config]]
key     = "qmd_collection"
kind    = "string"
label   = "QMD collection"
default = "learnings"

[[config]]
key     = "graph_cache"
kind    = "path"
label   = "nano_graphrag cache dir"
default = "~/.learnings/nano_graphrag_cache"
```

New protocol type in `ainb-plugin-protocol::manifest`: `ConfigField { key, kind,
label, default, choices? }` + `Manifest.config: Vec<ConfigField>`.

### A2. config.toml `[plugins.<name>]` value table

Resolved values persist to the layered config (project → user → system):

```toml
[plugins.learnings]
learnings_dir  = "~/.learnings/documents/learnings"
qmd_index      = "~/.cache/qmd/index.sqlite"
qmd_collection = "learnings"
graph_cache    = "~/.learnings/nano_graphrag_cache"
```

`config/mod.rs`: extend `PluginsConfig` with `values: BTreeMap<String,
toml::Value>` keyed by plugin name (separate from the existing
`enabled`/`disabled` lists). Precedence reuses the existing config layering.

**Reuse the existing persistence pipeline (landed on main `cf7fe1e`):**
`AppConfig::save()` (`config/mod.rs:678`) already writes config.toml, and
`app/events.rs` already calls `config_screen_state.apply_to_app_config(&mut
app_config)` + `app_config.save()` on popup confirm. Our framework plugs into
this — no new write path. We only (a) add `plugins.values` to the serialized
struct so it round-trips through `save()`, and (b) extend `apply_to_app_config`
to route Plugins-category edits into `plugins.values[plugin][key]`.

### A3. Settings screen "Plugins" category (generic renderer)

`components/config_screen.rs` is already category-driven (`General / Auth / Docker
…`) with an editable `config_popup` (text input + choice). Add a **Plugins**
category that, for each loaded plugin exposing a `[config]` schema:

- lists `label = value` rows per `ConfigField`,
- on edit opens `config_popup`: text input for `path`/`string`/`int`, choice popup
  for `bool`/`enum` (reuses existing widgets — no new list widget),
- on confirm, the existing apply→`AppConfig::save()` hook (`app/events.rs`,
  `cf7fe1e`) persists to config.toml `[plugins.<name>]` — and `config_popup`
  already supports Ctrl+V paste (`fd46155`), both free from the FF.

```
  Settings ▸ Plugins ▸ learnings
  ┌──────────────────────────────────────────────┐
  │ learnings_dir  = ~/.learnings/documents/lea… ✎│
  │ qmd_index      = ~/.cache/qmd/index.sqlite   ✎│
  │ qmd_collection = learnings                   ✎│
  │ graph_cache    = ~/.learnings/nano_graphrag… ✎│
  └──────────────────────────────────────────────┘
```

### A4. Inject resolved config at `plugin/init`

Extend `InitParams` (`params.rs`) with `config: serde_json::Value` (the resolved
`[plugins.<name>]` table). Host fills it; plugin parses into its typed struct in
`on_init`. Plugin owns **no** config file. Apply-on-spawn for v1 (config change →
next spawn picks it up); hot-reload is a later nicety.

---

## Part B — `read_paths` capability (protocol extension)

The capability enum (`manifest.rs::Capabilities`) has fixed fields and **no
path-scoped fs read**. Browse/detail/graph all read the configured KB dirs.

- Add `read_paths: CapabilityGrant` (a `List` of allowed path prefixes) to
  `Capabilities`.
- Grant + (where `host/fs/*` is used) gate it in `ainb-plugin-runtime`: a read is
  allowed iff the target is under a granted prefix; deny → `-32001`.
- **Propagate the new variant** to every exhaustive matcher in `ainb-plugin-runtime`
  and `ainb-plugin-cts-v2` (forbid wildcard arms — see prior-art note below).
- Security model: `read_paths` is the **outer envelope** (what's allowed);
  config.toml KB paths are the **choice within** it. A configured path outside the
  grant is denied at grant time and surfaced to the user.

> Prior art: new `CapabilityGrant` variants break exhaustive matchers in
> plugin-runtime + plugin-cts-v2 — propagate to every match site, no wildcards.

---

## Part C — The `learnings` plugin

### C1. Crate + manifest

`ainb-tui/crates/ainb-plugin-learnings/`

```toml
[plugin]
name = "learnings"
version = "0.1.0"
abi_version = 2
description = "Browse, search & graph your learnings knowledge base"

[capabilities]
read_paths        = ["~/.learnings", "~/.cache/qmd"]   # NEW (Part B)
spawn_subprocess  = ["qmd", "learnings"]               # search + graph-expand
write_plugin_data = true                               # ui state / cache
event_bus         = true                               # refresh snapshots

[provides]
screens        = ["learnings"]
commands       = ["/recall", "/memory"]
# NO cli_namespaces — TUI-only (v1 verification is tmux-ui-tripwire, not a CLI leg)

[subscribes]
snapshots = []

[lifecycle]
spawn          = "lazy"
idle_reap_secs = 600

# [config] schema per Part A1 (learnings_dir / qmd_index / qmd_collection / graph_cache)
```

### C2. Data layer (`src/data/`)

| Source | Access (Hybrid) | What it powers |
|--------|-----------------|----------------|
| `learnings_dir/*.md` + `*.entities.yaml` | direct fs read + frontmatter/yaml parse | Browse list, Detail pane, filter facets |
| QMD index (`qmd query --json` / `vsearch`) | shell `qmd` subprocess | Search tab (semantic, ranked) |
| `graph_cache/graph_chunk_entity_relation.graphml` | direct fs read + GraphML (XML) parse | Graph tab — entities + relationships |
| `graph_cache/kv_store_community_reports.json` | direct fs read | Graph tab — community clusters |
| (optional) `learnings search --mode local --format json` | shell `learnings` | graph-expanded search results |

A learning record = `{ id, title, scope, confidence, category, tags, source_tool,
project, key_insight, body_md, entities[], relationships[], provenance }` parsed
from the `.md` frontmatter + `.entities.yaml` sidecar.

### C3. UI (`src/ui/`) — burndown-style, 3 tabs + detail

```
┌─ 🧠 Learnings ───────────────────── Browse │ Search │ Graph ──┐
│ filters: scope[univ] conf[≥0.8] category[*] source[*] proj[*] │
│ ┌ list ─────────────────────────┐ ┌ detail ─────────────────┐│
│ │ ▶ lrn-audit-after-rebase  0.9 │ │ # Audit after rebase    ││
│ │   lrn-tokio-runtime-panic 1.0 │ │ key_insight: …          ││
│ │   lrn-claude-plugin-auto… 0.8 │ │ ## Problem / Solution   ││
│ │   …                           │ │ entities: tokio, …      ││
│ │                               │ │ rels: spawn_blocking    ││
│ │                               │ │   --solves--> panic     ││
│ │                               │ │ provenance: claude · …  ││
│ └───────────────────────────────┘ └─────────────────────────┘│
│ ↑↓ move  ⏎ open  / search  g graph  f filter  Tab pane  q     │
└──────────────────────────────────────────────────────────────┘
```

- **Browse**: scrollable list + typed filter chips (scope/confidence/category/
  source_tool/project) derived from the parsed records.
- **Search**: live query box → `qmd query` → ranked results (reuses the list/detail
  layout); Enter opens detail.
- **Graph**: pick an entity → neighbor entities with relationship-typed edges
  (`caused_by`/`solves`/`requires`/`relates_to`) from the graphml; `c` toggles
  community-cluster view from the community json.
- **Detail pane**: full `.md` body + entities + relationships + provenance.

Render path mirrors burndown: render locally with ratatui (`Table`, `List`,
`Block`, `Tabs`, gradient spans, TUI palette from the style guide) → convert to
`WireBuffer`. Input via `handle_key` (generation-bumped re-render).

### C4. Surfaces

- **TUI screen** `learnings` (keybinding / sidebar).
- **Slash commands** `/recall`, `/memory` (open the screen).
- **No CLI** — TUI-only by decision. Verification rests entirely on tmux-ui-tripwire.

---

## Phasing (feeds `/plan`)

| Phase | Title | Wave | Depends |
|-------|-------|------|---------|
| P0 | Protocol: `read_paths` cap + `[config]` schema + `InitParams.config` (+ cts/runtime variant propagation) | 1 | — |
| P1 | Host: config.toml `[plugins.<name>]` values + resolve + inject at init + grant read_paths | 2 | P0 |
| P2 | Host UI: Settings ▸ Plugins category (schema-driven, reuse config_popup) | 3 | P1 |
| P3 | Plugin scaffold: crate + manifest + Server + empty render + registration | 2 | P0 |
| P4 | Plugin data layer: config struct, fs reader, graphml parser, community json, qmd/learnings shell | 3 | P3 |
| P5 | Browse tab + filter chips | 4 | P4 |
| P6 | Detail pane | 4 | P4 |
| P7 | Search tab (qmd shell) | 4 | P4 |
| P8 | Graph tab (graphml + communities) | 4 | P4 |
| P9 | Slash commands `/recall` `/memory` (open screen) | 5 | P3 |
| P10 | Tests: cts axis (read_paths + config), **tmux-ui-tripwire (primary gate)**, unit (parsers, config resolve) | 5 | all |

(P5–P8 share the plugin UI crate; serialize by file ownership or split modules per tab.)
**No CLI leg** — tmux-ui-tripwire is the sole user-visible verification path, so it must be robust (heed the staged-binary SIGKILL / first-run-wizard / EnvFilter / substring-OR traps).

## Testing

- **Unit**: GraphML parse → entities/edges; `.md`+`.entities.yaml` → record;
  config.toml `[plugins.<name>]` resolution + layering; filter predicates.
- **CTS**: new axis exercising a plugin that declares `read_paths` + `[config]`
  (grant honored, out-of-envelope path denied -32001, config injected at init).
- **Tripwire (tmux) — PRIMARY GATE**: open the `learnings` screen, assert list
  renders real learning ids; `/` search returns ranked rows; `g` shows an entity
  neighborhood; detail pane shows body + provenance. This is the only user-visible
  verification leg (no CLI), so each phase that adds UI must add/extend a tripwire.
  Heed the macOS staged-binary SIGKILL (exit 137), first-run-wizard keystroke
  interception, EnvFilter crate-name drift, and substring-OR-passes traps per the
  tmux-ui-tripwire skill.

## Open questions (resolve in `/plan`)

1. Hot-reload on config change, or apply-on-next-spawn only? (v1: apply-on-spawn.)
2. Does `host/fs/read_file` need wiring, or do we read direct via std::fs and use
   `read_paths` purely as a declared/policy envelope? (Lean: direct std::fs +
   read_paths as policy envelope, since subprocess plugins aren't sandboxed.)
3. Search: `qmd query` only, or also fold `learnings search --mode local` for
   graph-expanded hits? (v1: qmd query; add graph-expand if cheap.)

## References

- Plugin model map: this session's Explore report (protocol methods, WireBuffer,
  capabilities, burndown exemplar).
- Pipeline/source-of-truth: `research/2026-06-02_11-06-17_reflect-memory-to-qmd-graph-pipeline.md` (in the prior stale worktree).
- Exemplar: `ainb-tui/crates/ainb-plugin-burndown/` (list + chips + zoom detail).
- Config UI: `ainb-tui/crates/ainb-core/src/components/config_screen.rs`,
  `config_popup.rs`.
- Capability/variant propagation prior art: memory `reference_gated_by_variant_propagation`.
