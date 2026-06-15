# TDD Plan — `learnings` plugin + per-plugin config framework

**Drives:** a per-phase Workflow engine (one reusable script, run P0→P10, gated between phases).
**Design:** `ainb-tui/docs/plugins/learnings-plugin-design.md` (read first).
**Branch:** `worktree-reflect-memory-refine` @ origin/main `cb32824`.
**Method:** each phase is RED → GREEN → REFACTOR. Write the listed tests FIRST (they
must fail), implement to green, then refactor. No phase closes with a red or ignored test.

## Verified anchors (don't re-discover)

| Thing | Location | Note |
|-------|----------|------|
| `Capabilities` struct (named fields, per-field `is_granted()`) | `crates/ainb-plugin-protocol/src/manifest.rs:108` | `read_paths` = **additive field**, not an enum-variant sweep |
| `CapabilityGrant` = `Bool \| List` | `manifest.rs:73` | unchanged; `read_paths` uses `List` form |
| `collect_granted_capabilities()` | `crates/ainb-plugin-runtime/src/plugin_task.rs:851` | add one `if c.read_paths.is_granted()` arm |
| init params built | `plugin_task.rs:480` | inject `config` here |
| `PluginInitParams` | `crates/ainb-plugin-protocol/src/params.rs:31` | add `#[serde(default)] config` |
| cts-v2 axes incl. **"fs path guard"** + "capability gating" | `crates/ainb-plugin-cts-v2/src/lib.rs:8`; canaries `tests/canaries/<axis>/main.rs`; host tests `tests/axes.rs`; `harness::register_canary` | extend/mirror for `read_paths` + `[config]` |
| `PluginsConfig` (`enabled`/`disabled` only) | `crates/ainb-core/src/config/mod.rs:341` | add `values: BTreeMap<String, toml::Value>` |
| `AppConfig::save()` → config.toml | `config/mod.rs:678` | REUSE, no new write path |
| apply→save-on-confirm hook | `crates/ainb-core/src/app/events.rs` (commit `cf7fe1e`) | REUSE |
| `ConfigCategory::Plugins` EXISTS | `crates/ainb-core/src/app/state.rs:1192` | P2 extends it |
| `ConfigSetting` / `ConfigValue{Text,Secret,Bool,Choice,Number}` | `state.rs:1243` | maps our `kind`: path/string→Text, bool→Bool, enum→Choice, int→Number |
| `ConfigScreenState.settings: HashMap<ConfigCategory, Vec<ConfigSetting>>` | `state.rs:~1290` | populate `[Plugins]` per loaded plugin |
| settings builder (per-category `ConfigSetting{}`) | `state.rs:~1322` | extend Plugins arm |
| plugin exemplar (render → `buffer_to_wire`, handle_key, ui state) | `crates/ainb-plugin-burndown/src/` | copy structure |
| SDK: `Plugin` trait, `Server::new().run_stdio()`, `WireBuffer/Cell/Coord/Color` | `crates/ainb-plugin-sdk-rust/src/` | plugin entrypoint |
| plugin test harness (in-proc render/handle_key) | `crates/ainb-plugin-testkit/` | unit-test render without tmux |
| tripwires | `crates/ainb-core/tests/tripwire_*.rs` | primary user-visible gate |

## Testing strategy (3 legs — TUI-only, no CLI)

1. **Unit** (fast, per-crate): parsers (`.md`+`.entities.yaml`→record; GraphML→entities/edges;
   community json), config resolution/layering, filter predicates, manifest `[config]`
   round-trip. Plugin render/key via **testkit** (in-proc, no tmux) → assert on `WireBuffer`
   cells / record state.
2. **CTS canary** (protocol conformance): new canary plugin declaring `read_paths` + `[config]`;
   host-side axis asserts grant honored, **out-of-envelope path denied `-32001`**, `config`
   injected at init. Mirror the existing "fs path guard" + "capability gating" axes.
3. **tmux tripwire** (PRIMARY user-visible gate): `tripwire_learnings_*.rs`. Every UI phase adds/
   extends one. **Heed traps** (tmux-ui-tripwire skill): macOS AMFI SIGKILL of staged binaries
   (exit 137, no stderr) → stage+sign per the skill; first-run wizard eats keystrokes → pre-seed
   config/onboarding; EnvFilter crate-name drift hides logs; **substring-OR assertions pass while
   broken** → assert exact, unique tokens. **Don't guess constants** — run each new tripwire once
   with a deliberately wrong expected value, read the actual render, THEN lock the assertion.

**Fixtures:** a committed mini-KB under `crates/ainb-plugin-learnings/tests/fixtures/kb/`
(3–4 `.md`+`.entities.yaml`, a tiny `graph.graphml`, a `community_reports.json`) so tests never
touch the real `~/.learnings`. Tripwires point the plugin's `learnings_dir`/`graph_cache` config
at this fixture dir.

## Waves / dependencies

```
 Wave 1:  P0 (protocol)
 Wave 2:  P1 (host config)      P3 (plugin scaffold)        ‹parallel, disjoint crates›
 Wave 3:  P2 (host UI ▸Plugins) P4 (plugin data layer)      ‹parallel›
 Wave 4:  P5 Browse · P6 Detail · P7 Search · P8 Graph      ‹share plugin UI crate — serialize by module/file ownership›
 Wave 5:  P9 (slash cmds)       P10 (cts axis + cross tripwire)
```
File-ownership rule: within a wave no file appears in two phases. P5–P8 each own a distinct
`src/ui/<tab>.rs` module + a distinct `tripwire_learnings_<tab>.rs`; the shared `src/ui/mod.rs`
tab-dispatch is touched only in P5 (others add their module behind a stable enum arm).

---

## P0 — Protocol: `read_paths` capability + `[config]` schema + `InitParams.config`
**Wave 1 · deps: none · crates: ainb-plugin-protocol (+ runtime collector/init only)**

### RED — tests first
`crates/ainb-plugin-protocol/src/manifest.rs` (#[cfg(test)]):
- `test_read_paths_capability_parses_list` — TOML `read_paths = ["~/.learnings"]` → `Capabilities.read_paths == List(["~/.learnings"])`; default (absent) → `Bool(false)`, `is_granted()==false`.
- `test_config_schema_roundtrip` — manifest with `[[config]] key/kind/label/default(/choices)` → `Manifest.config: Vec<ConfigField>` parses; serialize→parse is identity; unknown `kind` errors.
- `test_config_kind_variants` — each `kind` (`path,string,bool,enum,int`) deserializes to the right `ConfigKind`.

`crates/ainb-plugin-protocol/src/params.rs`:
- `test_init_params_config_defaults_empty` — `PluginInitParams` without `config` field deserializes (serde default = `Value::Null`/`{}`), preserving ABI-2 back-compat; with `config` it round-trips.

`crates/ainb-plugin-runtime/` (unit):
- `test_collect_granted_includes_read_paths` — manifest with `read_paths=["/x"]` → `collect_granted_capabilities()` contains `"read_paths"`; absent → does not.

### GREEN — implementation
- `manifest.rs`: add `pub read_paths: CapabilityGrant` to `Capabilities` (`#[serde(default)]`); add `ConfigKind` enum + `ConfigField{key,kind,label,default,choices:Vec<String>}` + `pub config: Vec<ConfigField>` (`#[serde(default)]`) on `Manifest`.
- `params.rs`: add `#[serde(default)] pub config: serde_json::Value` to `PluginInitParams`.
- `plugin_task.rs:851`: add `if c.read_paths.is_granted() { out.push("read_paths".into()) }`.

### Success
- Automated: `cargo test -p ainb-plugin-protocol -p ainb-plugin-runtime` green; `cargo build -p ainb-plugin-protocol` (no other crate breaks — additive fields).
- Manual: none (pure types).

---

## P1 — Host: `plugins.values` + resolve + inject + grant `read_paths`
**Wave 2 · deps: P0 · crate: ainb-core (config + plugins.rs)**

### RED
`crates/ainb-core/src/config/mod.rs` tests:
- `test_plugins_values_roundtrip` — config.toml `[plugins.learnings]\nlearnings_dir="x"` → `PluginsConfig.values["learnings"]["learnings_dir"]=="x"`; `save()`→reload identity; absent `[plugins.<x>]` → empty map (serde default), existing `enabled/disabled` unaffected.
- `test_plugins_values_layering` — project layer overrides user layer for the same `[plugins.<n>].key` (mirror existing layering test).

`crates/ainb-core/src/plugins.rs` tests:
- `test_resolved_config_injected_at_init` — host builds `PluginInitParams.config` from `plugins.values[name]` (assert the JSON passed to init equals the resolved table).
- `test_read_paths_granted_from_manifest` — a plugin manifest with `read_paths` → granted list passed to runtime includes `read_paths` (integration with P0 collector).

### GREEN
- `config/mod.rs`: add `#[serde(default)] pub values: BTreeMap<String, toml::Value>` to `PluginsConfig`.
- `plugins.rs`: when spawning/initing a plugin, resolve `plugins.values[id]` → `serde_json::Value`, set on `PluginInitParams.config`; ensure `read_paths` flows through grant.

### Success
- Automated: `cargo test -p ainb-core config:: plugins::` green; `cargo build -p ainb-core`.
- Manual: none.

---

## P2 — Host UI: Settings ▸ Plugins renders manifest `[config]`, edits persist
**Wave 3 · deps: P1 · crate: ainb-core (app/state.rs, config_screen.rs, app/events.rs)**

### RED
`app/state.rs` tests:
- `test_plugins_category_settings_from_manifest` — given a loaded plugin with a `[config]` schema, the builder populates `settings[ConfigCategory::Plugins]` with one `ConfigSetting` per `ConfigField`, mapping `kind`→`ConfigValue` (path/string→Text, bool→Bool, enum→Choice, int→Number), defaulting from `plugins.values` then schema `default`.
- `test_apply_routes_plugin_edit_to_values` — editing a Plugins-category `ConfigSetting` then `apply_to_app_config` writes into `app_config.plugins.values[plugin][key]` (NOT a top-level config field).

### GREEN
- `state.rs`: extend the Plugins-category settings builder (~1322 region) to append per-plugin `[config]` rows (keep the existing enable/disable rows); extend `apply_to_app_config` to route `ConfigCategory::Plugins` field edits → `plugins.values`.
- `config_screen.rs`: render the per-plugin rows (reuse existing row render + `config_popup`; Ctrl+V paste already present).
- `app/events.rs`: confirm path already calls `apply_to_app_config` + `AppConfig::save()` (cf7fe1e) — verify it persists `plugins.values`; add only if a gap.

### Success
- Automated: `cargo test -p ainb-core` green; `cargo build`.
- Tripwire `tripwire_config_plugins.rs`: open Settings → Plugins category → assert a plugin's config row renders its key+value; edit a value via popup + confirm → re-open → value persisted; grep the temp config.toml for `[plugins.<name>]`. (Pre-seed onboarding to skip the first-run wizard; stage+sign the binary.)
- Manual: visually confirm the row layout matches the TUI style guide.

---

## P3 — Plugin scaffold: crate + manifest + Server + empty render + registration
**Wave 2 · deps: P0 · crate: NEW ainb-plugin-learnings (+ workspace + bootstrap registration)**

### RED
`crates/ainb-plugin-learnings/` tests (via testkit):
- `test_manifest_parses` — `include_str!("../manifest.toml")` parses; declares `screens=["learnings"]`, `read_paths`, `spawn_subprocess=["qmd","learnings"]`, `write_plugin_data`, `event_bus`, and a `[config]` schema with `learnings_dir/qmd_index/qmd_collection/graph_cache`.
- `test_empty_render_smoke` — `Plugin::render` on an 80×24 viewport returns a `WireBuffer` with the title cell present (e.g. unique token `🧠 Learnings`).
- `test_init_consumes_config` — `on_init` parses `InitParams.config` into the typed `LearningsConfig{learnings_dir,qmd_index,qmd_collection,graph_cache}` with defaults when keys absent.

### GREEN
- New crate `ainb-plugin-learnings` (Cargo.toml, manifest.toml per spec C1, `src/main.rs` = `Server::new(LearningsPlugin::default()).run_stdio()`, `src/plugin.rs` Plugin impl, `src/config.rs` `LearningsConfig`).
- Add to workspace `members`. Register for discovery (mirror how burndown is staged into `~/.agents-in-a-box/plugins/` for the host; confirm the bootstrap/registration path used by burndown).

### Success
- Automated: `cargo test -p ainb-plugin-learnings` green; `cargo build -p ainb-plugin-learnings`.
- Tripwire `tripwire_learnings_open.rs`: launch host, open the `learnings` screen (keybinding/sidebar), assert the title token renders. (Staged-binary SIGKILL + first-run-wizard traps.)
- Manual: screen opens, empty state legible.

---

## P4 — Data layer: fs reader, GraphML parser, community json, qmd shell, config
**Wave 3 · deps: P3 · crate: ainb-plugin-learnings (src/data/)**

### RED (fixtures under `tests/fixtures/kb/`)
- `test_parse_learning_record` — `.md` frontmatter + `.entities.yaml` → `LearningRecord{id,title,scope,confidence,category,tags,source_tool,project,key_insight,body_md,entities[],relationships[],provenance}`; missing sidecar → record with empty entities (no panic).
- `test_scan_learnings_dir` — directory scan returns N records, sorted stable; ignores non-`.md`.
- `test_parse_graphml` — fixture `graph.graphml` → `Vec<Entity>` + `Vec<Edge{source,target,rel_type}>`; malformed XML → typed error, not panic.
- `test_parse_community_reports` — `community_reports.json` → `Vec<Community>`.
- `test_qmd_query_parse` — given a captured `qmd query --json` sample (committed), parse → ranked `Vec<SearchHit{id,score,title}>`. (Shell call itself wrapped behind a trait so tests use the sample; one `#[ignore]` live-`qmd` smoke test.)
- `test_filter_predicates` — scope/confidence/category/source/project filters select the expected subset.

### GREEN
- `src/data/{record.rs (md+yaml), graph.rs (graphml + community), search.rs (qmd shell behind a trait), filter.rs}`; `LearningsConfig` paths drive all readers; `~`-expansion; all reads confined to configured (read_paths-covered) dirs.

### Success
- Automated: `cargo test -p ainb-plugin-learnings` green (live-qmd test `#[ignore]`).
- Manual: none.

---

## P5 — Browse tab + filter chips   ·   ## P6 — Detail pane
## P7 — Search tab   ·   ## P8 — Graph tab
**Wave 4 · deps: P4 · crate: ainb-plugin-learnings (src/ui/<tab>.rs each)**

Shared pattern per tab (TDD):
- **RED (testkit, in-proc):** `test_<tab>_render` drives `render`/`handle_key` over the fixture KB and asserts on `WireBuffer` cells for unique tokens + selection state (e.g. P5: a fixture learning id + a filter chip label after pressing `f`; P6: `## Problem` + provenance token after `Enter`; P7: ranked rows after typing a query that the sample matches; P8: an entity name + a `--solves-->` edge token after `g`). Plus pure-logic unit tests (filter application, list scroll, search debounce, graph neighborhood selection).
- **GREEN:** implement `src/ui/<tab>.rs`; P5 also owns `src/ui/mod.rs` tab dispatch + filter-chip bar + key routing (`Tab` switch, `/` search, `g` graph, `f` filter, `↑↓ ⏎`); P6/P7/P8 add their module behind a stable `Tab` enum arm (no edits to P5's dispatch beyond their own arm — keep arms additive to respect file ownership; if a shared edit is unavoidable, serialize P6–P8 after P5).
- **Success:**
  - Automated: `cargo test -p ainb-plugin-learnings` green.
  - **Tripwire per tab** (`tripwire_learnings_browse.rs`, `_detail.rs`, `_search.rs`, `_graph.rs`): open screen pointed at the fixture KB (config injected), perform the keys, assert the exact unique token renders. Run each once with a wrong expected value first to read the real render (don't guess).
  - Manual: each tab matches the spec mock + TUI style guide.

(P5–P8 share the UI crate → serialize by module/file ownership; P5 lands first as it owns `ui/mod.rs`.)

---

## P9 — Slash commands `/recall` `/memory` (open the screen)
**Wave 5 · deps: P3 · crate: ainb-core (command palette wiring) + manifest `provides.commands`**

### RED
- `test_slash_recall_opens_learnings_screen` — dispatching `/recall` (and `/memory`) routes to opening the `learnings` screen (assert the screen-open event/state).

### GREEN
- Manifest already declares `commands=["/recall","/memory"]`; wire the host command dispatch → open screen `learnings`.

### Success
- Automated: `cargo test -p ainb-core` green.
- Tripwire (extend `tripwire_learnings_open.rs`): type `/recall`, Enter → learnings screen renders.

---

## P10 — CTS axis (read_paths + [config]) + cross-cutting tripwire sweep
**Wave 5 · deps: all · crate: ainb-plugin-cts-v2 (+ final tripwire pass)**

### RED
- New canary `crates/ainb-plugin-cts-v2/tests/canaries/read_paths_config/main.rs` — declares `read_paths=["<tmp>/allowed"]` + a `[config]` schema; on a host `fs/read` it attempts both an in-envelope and out-of-envelope path and echoes the injected `config`.
- Host axis in `tests/axes.rs`:
  - `axis_read_paths_grant_honored` — in-envelope read succeeds.
  - `axis_read_paths_denied_out_of_envelope` — out-of-envelope read → `-32001`.
  - `axis_config_injected_at_init` — canary reports the `config` the host injected.

### GREEN
- Extend the existing fs-path-guard to consult `read_paths`; register the canary via `harness::register_canary`.

### Success
- Automated: `cargo test -p ainb-plugin-cts-v2` green (all prior 14 axes still pass — no wildcard arms broken).
- Final tripwire sweep: full `cargo test -p ainb-core --test 'tripwire_learnings_*'` green; `cargo clippy -- -D warnings` (scoped to touched crates if main carries pre-existing debt); `cargo fmt --check` on touched files.
- Manual: open the real `~/.learnings` KB, exercise Browse/Search/Graph/Detail end-to-end.

---

## Per-phase Workflow contract (how the engine consumes this)

Each phase agent receives: this plan + the phase id. It MUST:
1. Write the RED tests first; run them; confirm they fail for the right reason.
2. Implement to GREEN; run the phase's `cargo test` targets.
3. For UI phases: add/extend the tripwire; run it once with a wrong expected value, read the
   actual render, then lock the assertion (`don't guess constants`).
4. Run `cargo clippy -- -D warnings` (scoped) + `cargo fmt` on touched files (per-file
   `rustfmt`, never `cargo fmt -p` while a sibling edits the crate).
5. Hand back: files changed, test names + pass/fail, tripwire token asserted, any deviation.
A phase is GREEN only when its automated criteria pass; the orchestrator gates the next wave on it.
