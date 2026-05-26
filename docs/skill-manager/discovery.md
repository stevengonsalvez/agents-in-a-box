# Skill Discovery (v1.1)

> Read-only adoption flow for users with pre-populated tool homes
> and Claude-Code-installed marketplace plugins. Layers on top of
> the v1 skill-manager — see `ainb-tui/plans/skill-manager/spec.md`
> for v1 mechanics, and
> `.agents/goals/ainb-skill-manager-v1.1-discovery-spec.md` for the
> full v1.1 spec this document summarises.

## Why discovery exists

The v1 skill-manager ships an empty manifest and expects the user
to run `ainb migrate --from-bootstrap` (or hand-edit
`manifest.yaml`) before anything appears in the TUI. That works
for greenfield installs, but most users arrive at `ainb` with
state already on disk:

- Skills they hand-rolled under `~/.claude/skills/`,
  `~/.codex/skills/`, etc.
- Plugins installed via Claude Code's `/plugin install` command,
  cached at `~/.claude/plugins/cache/<marketplace>/<plugin>/<version>/`.
- Bootstrap-era references in a `toolkit/external-dependencies.yaml`.

v1.1 scans for that state on the **first open of the SkillManager
TUI**, computes per-category counts, and offers a one-keystroke
import. Nothing is written without explicit user consent.

## Architecture

```text
DiscoveryWalker (read-only)
├── class_a   — marketplace plugins from ~/.claude/plugins/cache/
├── class_b   — legacy external-dependencies.yaml matcher (opt-in)
└── class_c   — orphan SKILL.md units in ~/.<tool>/skills/, etc.
                across all 9 adapter tools
              ↓
Reconciler (pure fn)
├── classifies each found unit (A / B / C / conflict)
├── synthesises unit URIs per the locked rules
└── emits a ManifestPatch { new_sources, new_units }
              ↓
SkillManager TUI
├── triggers on first-open-with-empty-manifest
├── renders banner overlay with count summary
└── [Enter] import all · [d] details · [s] skip
```

All three walkers are **pure** — they never write, never panic on
malformed input, and tolerate any subset of the layout being
missing (best-effort discovery).

## The three walker classes

### Class A — marketplace plugins

Source: `ainb-cli/src/discovery/class_a.rs`.

Walks `<claude_home>/plugins/cache/<marketplace>/<plugin>/<version>/`
and reads `.claude-plugin/plugin.json`. Lists every `skills/<name>/`
directory and every `agents/<name>.md` file as a
`DiscoveredMarketplaceUnit`.

```text
~/.claude/plugins/
├── known_marketplaces.json     # registry — when present, marketplace
│                               # names are real; when absent every
│                               # marketplace is labelled "unknown"
└── cache/
    └── <marketplace>/
        └── <plugin>/
            └── <version>/
                ├── .claude-plugin/plugin.json   # required marker
                ├── skills/<name>/SKILL.md       # 0..n
                └── agents/<name>.md             # 0..n
```

Output shape:

```rust
DiscoveredMarketplaceUnit {
    plugin: String,
    marketplace: String,       // "unknown" when registry missing
    version: String,
    units: Vec<DiscoveredUnit { kind: Skill|Agent, name, path }>,
}
```

### Class B — legacy YAML matcher (opt-in)

Gated behind `--legacy-yaml=<path>` on `ainb migrate --discover`.
Parses a `toolkit/external-dependencies.yaml` and name-matches
discovered units against the legacy bootstrap manifest, so users
mid-cutover from `bootstrap.js` can adopt their units as
`gh:<repo>@<ref>` URIs instead of `local:`.

Never auto-runs — the bootstrap.js era is over and surfacing its
metadata by default would rot `ainb`. The flag preserves the
migration path for the dwindling set of users still on the old
flow.

### Class C — orphan units across 9 tool homes

Source: `ainb-cli/src/discovery/class_c.rs`.

Walks `~/.<tool>/<subdir>/` for each of the 9 v1 adapter tools
(claude, codex, copilot, gemini, cursor, amazonq, claude-desktop,
cline, roo). The per-tool subdir mapping mirrors each adapter's
`list_installed` so a `skill` discovered for claude-desktop (which
does not accept skills) never surfaces.

Two on-disk layouts are supported:

- **Directory units** (`skills/`, `plugins/`, `hooks/`,
  `mcp-servers/`, `statuslines/`): each `<subdir>/<name>/`
  directory is a candidate; if `SKILL.md` is present inside, its
  YAML frontmatter is parsed best-effort.
- **Flat-md units** (`agents/`, `commands/`): each `*.md` file is
  a candidate; the file itself is the SKILL.md analogue.

Output:

```rust
DiscoveredOrphanUnit {
    tool: String,             // "claude", "codex", ...
    kind: UnitKind,           // from frontmatter, or path-inferred
    name: String,             // from frontmatter `name:`, or dir/file
    path: PathBuf,            // absolute
    frontmatter_valid: bool,  // true iff `---\n...\n---` parsed
}
```

#### Frontmatter validity

A SKILL.md (or flat `.md`) is treated as "parseable" when:

- File exists and is readable.
- Optional YAML front-matter (`---\n...\n---`) parses without
  panic into a YAML mapping.
- `name:` field present → used. Otherwise → parent directory name
  (dir-layout) or file stem (flat-md).
- `kind:` field present and one of
  `skill|agent|command|hook|mcp|statusline|plugin` → used (`mcp`
  aliases to `mcp-server`). Otherwise → inferred from path
  (`skills/` → skill, `agents/` → agent, …) → fallback to `skill`.

`frontmatter_valid = true` reports that the YAML block existed
AND parsed. It does NOT require both `name` and `kind` to be
populated — name-only and kind-only frontmatter are valid. The
walker never aborts on a single malformed unit; the malformed one
falls back to directory name + kind=skill.

#### Walker performance budget

`tripwire perf_budget_under_500ms_for_100_units` asserts the
class-C walker completes a 100-unit fixture in under 500ms on a
debug build. Class-A walks file-system-bound but small layouts —
empirically <100ms for a full Claude Code plugin cache.

## Reconciler — URI synthesis + conflict detection

Source: `ainb-cli/src/discovery/reconcile.rs`. Pure fn:

```rust
fn reconcile(walker_out: &WalkerOutput) -> ManifestPatch
```

### URI shapes emitted

- **Class C orphan** → `local:<tool-home>/<subdir>@head/<name>`
  - `<tool-home>` is the canonical per-tool path
    (`~/.claude`, `~/.aws/amazonq`,
    `~/Library/Application Support/Claude`, …).
  - One `local:<tool-home>/<subdir>` source per (tool, subdir);
    units share that source via the `@head/<name>` suffix.
- **Class A unit** →
  `marketplace:<plugin>@<marketplace>/<subdir>/<name>`
  - Per-unit URI so a single shadow relationship doesn't drag the
    whole plugin offline.
  - One `marketplace:<marketplace>` source per discovered
    marketplace, `kind=claude-marketplace`, `read_only=true` so
    `ainb` does not run install/update/uninstall against it.

### Conflict matrix

The reconciler indexes class-C orphans by `(tool, name)` then
walks the class-A output looking for collisions. The default is
**orphan wins** — the user's hand-edited files take precedence —
which the `[s]` keybind in the TUI Units panel can flip per-unit.

| Scenario                                                  | Default                                                              |
|----------------------------------------------------------|----------------------------------------------------------------------|
| A + C same name in `claude` tool home                    | C active, A shadowed (`A.shadowed_by = C.uri`).                       |
| A + C same name but C in non-`claude` tool home          | Not a conflict. Marketplace plugins only deploy to `~/.claude`.       |
| C + C same name across two tool homes                    | Not a conflict. Two separate units, each scoped to its tool.         |
| A + A same unit name across two marketplaces             | First-seen wins; second shadowed (`second.shadowed_by = first.uri`). |
| A + C + A: same name, C in claude + two A's              | C active; BOTH A's shadowed_by = C.uri (chained to C, not to A1).    |

The `shadowed_by` field lives on the unit that is BEING shadowed
and points to the unit that is ACTIVE. The TUI renders the active
side normally and the shadowed side with a "(shadowed by X)"
suffix.

### Determinism + purity

`reconcile()` runs no I/O, reads no env vars, and produces
byte-identical output for identical input. Tests assert both the
determinism (repeated calls match) and the env-independence
(setting `AINB_TOOL_HOME_*` env vars does not affect URI
shapes).

## TUI discovery banner

Source: `ainb-tui/crates/ainb-core/src/components/skill_manager_screen.rs`.

### Trigger logic

When the user navigates to SkillManager (pressing `m` on Home),
`AppEvent::GoToSkillManager` calls:

```rust
run_discovery_walkers(claude_home) -> WalkerOutput
maybe_show_discovery_banner(state, ainb_home, walker)
```

The banner flips to `Visible` when ALL three conditions hold:

1. `manifest.units.is_empty()` — the user hasn't yet adopted
   anything.
2. `<ainb_home>/.discovery-skipped` marker absent — the user has
   not previously pressed `[s]`.
3. Walker output has at least one candidate.

Subsequent re-opens of SkillManager are **idempotent** — if the
banner is already `Visible`, the trigger is a no-op so the user
sees the same counts they did first time. Per spec §Edge cases:
"Banner appears but user navigates away before pressing Enter →
Banner re-appears next open until dismissed via [s]."

### Banner overlay

```text
┌ Detected existing units — import them? ┐
│ Marketplace plugins:                 3 │
│ Orphan units:                        9 │
│                                        │
│ Conflicts (orphan wins by default):  0 │
│                                        │
│ [Enter] import all  [d] details  [s] skip
└────────────────────────────────────────┘
```

Counts come from `compute_counts(&WalkerOutput)`:

- `marketplace_plugins` = `class_a.len()`.
- `orphan_units_total` = `class_c.len()`.
- `orphan_units_per_tool` = `(tool, count)` pairs in first-seen
  order; rendered only in `Details` mode to keep the compact view
  terse.
- `conflicts` = count of class-A units that share a name with a
  class-C orphan in the `claude` tool home. Mirrors the
  reconciler's A-vs-C-in-claude rule 1:1 so the banner number
  matches the import outcome exactly.

### Key handlers

| Key      | Action                                                                            |
|----------|-----------------------------------------------------------------------------------|
| `Enter`  | `apply_discovery_import` — calls `reconcile()` on the cached walker output, merges the patch into `<ainb_home>/manifest.yaml`, refreshes the Sources / Units / Detail panels in place, and dismisses the banner. |
| `d`      | `toggle_discovery_details` — flips between the compact `Visible` and expanded `Details` rendering. |
| `s`      | `apply_discovery_skip` — writes `<ainb_home>/.discovery-skipped`, dismisses the banner, and clears the cached walker output. |
| `Esc`/`q`| Returns to Home without touching the banner state. The banner re-appears next time SkillManager opens. |

### Persistence

- **Import** writes the new sources + units to
  `<ainb_home>/manifest.yaml` and dismisses the banner. No
  separate skip-marker is written — the import is the "yes"
  answer.
- **Skip** writes a zero-byte `<ainb_home>/.discovery-skipped`
  marker file. Future opens skip the trigger entirely until the
  marker is removed via the future `ainb migrate --discover
  --force` flag (cleared programmatically by
  `clear_discovery_skip_marker(&ainb_home)`).
- **Navigate-away (Esc/q)** writes nothing. Re-entry runs the
  trigger again, sees the still-empty manifest + absent marker,
  and re-shows the same banner.

## Walker output snapshot via `walker_cache`

`SkillsScreenData::walker_cache: Option<WalkerOutput>` snapshots
the walker run that produced the visible counts. The `[Enter]`
import path uses the snapshot, not a fresh walk, so a file that
lands between paint and keystroke never causes a mismatch between
the banner advertised counts and the actually-imported entries.

The cache is cleared on both `[Enter]` (consumed by the import)
and `[s]` (skip discards it).

## How to test discovery locally

Seed an isolated `$HOME`, then point `ainb` at it:

```bash
HOME=$(mktemp -d)
export HOME
mkdir -p $HOME/.claude/skills/my-skill
cat > $HOME/.claude/skills/my-skill/SKILL.md <<'EOF'
---
name: my-skill
kind: skill
---
body
EOF

# Marketplace plugin layout (class-A coverage):
mkdir -p $HOME/.claude/plugins/cache/example-mp/example-plugin/v0.1.0/.claude-plugin
echo '{"name":"example-plugin"}' > $HOME/.claude/plugins/cache/example-mp/example-plugin/v0.1.0/.claude-plugin/plugin.json
echo '{"example-mp": {}}' > $HOME/.claude/plugins/known_marketplaces.json

# Run ainb against the seeded home; press `m` on Home.
AINB_HOME=$HOME/.agents-in-a-box \
AINB_USE_REAL_HOMES=1 \
  ainb
```

The full tripwire (`crates/ainb-core/tests/tripwire_core_skill_manager_discovery_banner.rs`)
drives this flow via tmux + the live `ainb` binary so the banner
contract holds end-to-end, not just at the unit-test layer.

## Reference

- Spec: `.agents/goals/ainb-skill-manager-v1.1-discovery-spec.md`
- Walker source: `ainb-tui/crates/ainb-cli/src/discovery/`
- TUI source:
  `ainb-tui/crates/ainb-core/src/components/skill_manager_screen.rs`
- Tripwire:
  `ainb-tui/crates/ainb-core/tests/tripwire_core_skill_manager_discovery_banner.rs`
- Manifest schema additions:
  `ainb-tui/crates/ainb-skill-core/src/manifest.rs`
  (`SourceKind::ClaudeMarketplace`, `SourceEntry::read_only`,
  `UnitEntry::shadowed_by`, `MarketplaceUri`)
