# AINB TUI

Terminal UI and CLI for Agents-in-a-Box.

## Usage Analytics

Open Stats in the TUI with `i`. The Usage screen includes Daily, Weekly, Project, Burndown, and Optimize tabs. Burndown supports Claude Code and Codex local session histories, period switching, provider filtering, include/exclude project filters, and read-only optimization findings.

Keys: `Tab` changes usage tab, `1`-`5` selects Today/Week/30 days/Month/All, `p` cycles All/Claude/Codex, `/` adds include filter, `x` adds exclude filter, `d` enters custom `YYYY-MM-DD YYYY-MM-DD` range, `c` clears filters, and `r` reloads.

CLI usage:

```bash
ainb usage report --period week
ainb usage report --from 2026-04-01 --to 2026-04-10 --format json
ainb usage export --format csv --output /tmp/ainb-usage.csv
ainb usage optimize --period 30days
ainb usage compare --period all
ainb usage yield --period week
```

AINB reads `~/.claude/projects/**/*.jsonl` and `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` or `$CODEX_HOME/sessions/...`. Parsing is read-only. Cost values are estimates; unknown model prices are omitted while tokens/calls remain visible.

## BSP tiling (epic `agents-in-a-box-siu`)

The main content region is moving from a hardcoded 40/60 split to a generalised **binary space-partition tree**: each leaf names a component or plugin, internal nodes bisect horizontally (side-by-side) or vertically (stacked) with a drag-resizable ratio.

Foundation modules live under `crates/ainb-core/src/ui/`:

| Module | Responsibility |
|--------|----------------|
| `bsp` | `LayoutNode` tree · `LayoutSnapshot` serde wrapper · walker producing per-leaf `Rect` · focus / split / close mutators |
| `bsp_render` | `composite_snapshot` walks tree + paints each leaf's `WireBuffer` via the paint helper |
| `bsp_keys` | tmux-style prefix-mode dispatch (`Ctrl+W` chord → `v/s/x/o/r/Esc`) |
| `bsp_mouse` | hit-test for `(col, row) → Interior \| Border` · `apply_drag_horizontal/vertical` |
| `bsp_persist` | atomic save/load of `LayoutSnapshot` to `~/.agents-in-a-box/layout.json` |
| `wire_paint` | paint a `WireBuffer` into a ratatui `Buffer` at a given `Rect` |

Keys (in tmux-style prefix mode):

| Chord | Action |
|-------|--------|
| `Ctrl+W` | Enter prefix mode |
| `Ctrl+W v` | Split focused tile vertically (stacked) |
| `Ctrl+W s` | Split focused tile horizontally (side-by-side) |
| `Ctrl+W x` | Close focused tile (sibling promotes) |
| `Ctrl+W o` / `Ctrl+W Tab` | Cycle focus to next leaf (pre-order) |
| `Ctrl+W r` | Enter resize mode (`h/j/k/l` adjust focused parent ratio) |
| `Ctrl+W Esc` | Exit prefix mode |

Plugins can advertise a preferred minimum tile size in their manifest:

```toml
[provides]
preferred_min_size = [40, 12]   # cols × rows
```

The host clamps splits so the plugin's tile never falls below this floor (with a global 30×10 fallback when the hint is absent).

The foundation work (atomic data structures, dispatch primitives, persistence I/O) lives behind `AppState.bsp: Option<LayoutSnapshot>` — `None` keeps the legacy hardcoded split as the v1 default for parity. Wiring of the actual `bsp.walk(...)` render path through `components/layout.rs`, the mouse/keyboard event loop, the plugin runtime's `last_rendered`, and the manifest hint clamp is tracked as follow-up beads under the `siu` epic.
