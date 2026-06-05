# ainb test layers — reference

One-line summaries + when-to-use for each of the 5 layers.

## L1 — Unit (cargo test)

**Command**: `cargo test --workspace --lib --bins --tests`

Pure-rust unit tests across every crate in the ainb workspace, plus inline `#[cfg(test)] mod tests` blocks, plus integration tests in each crate's `tests/*.rs` directory. Covers BSP tree logic, layout walker, key dispatch tables, manifest parsing, WireBuffer paint helper math, mouse hit-test math.

**Important**: `--lib --bins` alone **skips** integration tests in `tests/*.rs`. The `--tests` flag is required to include them. This was the source of a "tests pass but P0 RED files never compiled" drift during BSP work — see `noteworthy_cargo_lib_bins_skips_tests_dir`.

**When**: every PR; default in `quick` and `ci` modes.

**Don't catch**: visual regressions, plugin compositing, async runtime interaction.

## L2 — Insta snapshot (ratatui TestBackend)

**Command**: `cargo test --workspace --test 'snapshot_*'` (+ `cargo insta accept` when `--update`)

Component-level render snapshots. Each tileable host component has a `snapshot_<name>.rs` integration test that renders the component into a `TestBackend` at min size (30×10) and large size (200×60), then diffs against a checked-in golden via `insta::assert_snapshot!`.

**When**: every PR; default in `quick` and `ci` modes. Filter by `--component <name>` to run just one.

**Don't catch**: cross-component layout interactions, plugin tile composition.

**Memory tripwire**: `insta` strips one trailing newline on read; tests use `trim_end_matches('\n')`. Never re-record to "fix" a diff that looks correct.

## L3 — Mock-plugin compositing

**Command**: `cargo test --workspace --test 'plugin_tile_*'`

Tests the `Pane(plugin_id)` BSP-leaf path using `MockPlugin` from `ainb-plugin-testkit`. Mock returns deterministic `WireBuffer`s for any viewport. Validates that:
- Host sends `Cmd::Render(plugin_id, tile_rect)` for the correct viewport
- `try_recv` of `Out::Rendered(plugin_id, WireBuffer)` paints into the correct tile rect
- Border, focus indicator, and adjacent component renders interleave correctly

**When**: PR touches `ui/bsp.rs`, `ui/wire_paint.rs`, or plugin-runtime. Part of `--layer all`.

**Don't catch**: real plugin behaviour (resource allocation, stdio framing, subprocess lifecycle).

## L4 — Real-plugin spawn

**Command**: `cargo test --workspace --test 'real_plugin_*' -- --ignored --test-threads=1`

Spawns the actual plugin binary as a subprocess (e.g. `cargo run -p ainb-plugin-burndown`), sends JSON-RPC `plugin/render(viewport)`, asserts the returned `WireBuffer` matches expected cells. Slow (subprocess spawn cost). Single-threaded to avoid stdio JSON-RPC interleaving across tests.

**When**: opt-in via `--plugin <name>` or `--layer L4`. Runs in CI nightly, not per-PR.

**Don't catch**: visual issues (cells render correctly but might look wrong to a human).

## L5 — VHS recording

**Command**: `vhs tests/vhs/bsp/<tape>.tape -o tests/vhs/bsp/out/<tape>.gif`

Charm `vhs` records a real ainb session driven by a `.tape` script. Produces a `.gif` (and `.png` keyframes) artefact. Manual visual regression — Stevie eyeballs the gif on PR.

Standard BSP tape suite (lives at `ainb-tui/tests/vhs/bsp/`):
- `default-layout.tape` — launch ainb, verify default 40/60 layout looks identical to pre-BSP
- `split-vertical.tape` — `Ctrl+W v`, capture split mid-action
- `drag-resize.tape` — mouse drag border, capture mid-drag + post-drag
- `close-tile.tape` — `Ctrl+W x`, capture rebalanced sibling
- `persist-restore.tape` — quit, relaunch, assert restored layout
- `stale-tile.tape` — restore layout with unknown `component_id`, assert `⚠ unknown` placeholder

**When**: opt-in via `--vhs <tape>` or `--vhs all`. Never auto-runs. Stevie wants this manual.

**Don't catch**: things that don't render as visual diff (e.g. silent state corruption).

**Memory tripwire**: `vhs` cold-paint times — ainb home screen ~30s cold, post-`i` panels 40-60s. Tapes must bake in `Sleep 30s` / `Sleep 60s` appropriately or capture loading screen with no error.

## Layer combinations

| Combination | Layers | Time |
|-------------|--------|------|
| `quick` (default) | L1 + L2 | ~3-5s |
| `--ci` | L1 + L2 (single-threaded) | ~5-10s |
| `--layer all` | L1 + L2 + L3 | ~5-10s |
| `--plugin <name>` | L4 (one plugin) | ~10-30s per plugin |
| `--vhs <tape>` | L5 (one tape) | 30s-2min per tape |
| `--vhs all` | L5 (all tapes) | 5-15min |
| `--vhs all --update` | L5 with regeneration | 5-15min |

## What this layered model is NOT

- Not a replacement for `cargo clippy --workspace -- -D warnings` (run separately as a quality gate)
- Not a replacement for `cargo fmt --check` (run separately)
- Not a security/audit scan (use `cargo audit` separately)
- Not a benchmark suite (use `cargo bench` separately)
- Not E2E testing of ainb against real Claude Code containers (that's `ainb run --tool claude` itself)
