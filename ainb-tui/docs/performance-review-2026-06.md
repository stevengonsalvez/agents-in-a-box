# ainb-tui performance review — June 2026

Execution-construct review of the TUI, plugin runtime, all in-tree plugins, and
the hangar daemon. Scope is runtime behaviour — event loops, timers, threads,
file IO, subprocess spawns, render cycles — not code style. Findings are backed
by live profiling on macOS (release builds, no sudo). Each finding has a bead
labeled `performance-improvement`; this document is the summary.

## How it was measured

- Isolated, synthetic environment only — never touched real sessions: synthetic
  `HOME`, isolated `TMUX_TMPDIR`, daemon `AINB_HANGAR_HOME` under `/tmp`.
- In-binary timing via an env-gated facility (`crate::perf`, enabled by
  `AINB_PERF_TRACE`; zero cost when unset) recording cold-start, per-frame draw
  duration, favorites-load count, and key-to-render latency. A periodic dump
  thread writes the summary to `$AINB_PERF_TRACE_FILE` so measurement does not
  depend on the exit path.
- Hot-path unit costs via gated micro-benchmarks (`AINB_PERF_MICRO=1`, test
  `crates/ainb-core/tests/perf_micro.rs`).
- CPU/RSS sampled with `ps` once per second while the process was driven in an
  isolated tmux pane.

Both the instrumentation and the micro-benchmarks are retained behind flags and
are inert in normal runs.

## Headline numbers

| Metric | Measured |
|---|---|
| TUI idle CPU (home / session-list) | **~5.7%** (range 4.0–7.2%), RSS ~13 MB flat |
| Time in `draw()` while idle | **904 ms / 15 s = 6.0%** — accounts for ~all idle CPU |
| Frames drawn with zero input | ~37 fps (unconditional redraw) |
| Cold start → first paint | **345 ms** |
| Key-to-render latency (empty home) | p50 **1.7 ms** / p99 2.7 ms |
| `FavoritesStore::load()` | **527 µs/call**, once per session-list frame |
| `git2 open + remote` | **561 µs/call**, **per workspace per frame** |
| Hangar daemon idle CPU | **0.0%**, RSS ~10 MB flat |

## The story (reframing "TUI lag")

Per-keystroke latency is **low (~1.7 ms)** when the render is cheap. The real
problems are:

1. **Idle CPU ~6%** from redrawing the full layout ~37×/second when nothing
   changed. A pure footprint/battery cost. The hangar daemon, by contrast,
   idles at 0% — the TUI's redraw loop is the dominant idle cost in the system.
2. **Render cost that scales** with the session list: every frame on the primary
   screen re-parses `favorites.yaml` (527 µs) and opens each workspace's git repo
   (561 µs/workspace). At 10 workspaces that is ~184 ms/s (~18% CPU) of redundant
   work and adds ~5.6 ms/frame, dragging key-to-render from 1.7 ms toward 7 ms+.

One fix — **gate the redraw on dirty state** — removes the idle baseline *and*
stops the per-frame favorites/git multiplier from running when nothing changed.

## Findings (severity-ranked)

### P1 — TUI lag & idle CPU (primary)
- **`agents-in-a-box-wai`** — Full layout redrawn every 33 ms unconditionally
  (`main.rs:344`). 6% idle CPU. Gate on dirty state (plugin renders already are;
  the host layout is not, at `state.rs:9316`). Keystone fix.
- **`agents-in-a-box-9ov`** — `git2` open + remote lookup per workspace per frame
  (`session_list.rs:303-327`) for the ⭐ indicator. 561 µs × N × 30 fps. Cache
  per-workspace remote/favorite status; never call git2 in render.

### P2 — heavy per-event work
- **`agents-in-a-box-8rn`** — `favorites.yaml` parsed every session-list frame
  (`session_list.rs:244` → `favorites_store.rs:167`). Cache in memory.
- **`agents-in-a-box-h04`** — burndown drops cache+indices and fully recomputes
  (O(N log N), 13+ rollups) on every `sessions.usage_data` chunk — 50+ times per
  refresh (`plugin.rs:512,516`; `usage.rs:1614,2286`). Recompute once per data
  change, after reassembly.

### P3 — IO churn, runtime backpressure, subprocess fan-out
- **`agents-in-a-box-ma9`** — session-reader walks the provider tree twice per
  refresh (`scanner.rs:243-271`). Merge count into parse.
- **`agents-in-a-box-178`** — session-reader chunker clones + re-encodes the whole
  chunk per size probe (`plugin.rs:479,482,550`). Estimate size incrementally.
- **`agents-in-a-box-uf3`** — plugin runtime command/key inboxes are unbounded
  (`plugin_task.rs:144,155`). Bound them; shed stale renders.
- **`agents-in-a-box-h3t`** — two-pass JSON decode per inbound frame
  (`rpc.rs:132-164`). Single-pass typed decode.
- **`agents-in-a-box-9pb`** — `App::tick` spawns `tmux capture-pane` for all
  non-selected sessions every 5 s (`state.rs:9001-9013`). Capture only the
  selected pane.
- **`agents-in-a-box-dsp`** — hangar daemon RPC connections have no read timeout
  (`rpc/mod.rs:145`). Hung client pins a task.

### P4 — low / already healthy
- **`agents-in-a-box-cn9`** — daemon claim loop polls every 1 s *when bound to a
  runtime* (`run_loop.rs:154-173`). Bare daemon measured 0%. Tunable; optional
  event-driven wakeup.
- **`agents-in-a-box-nm3`** — notifyd emits OS notifications via blocking
  subprocess in an async handler (`listener.rs:205`). Debounced; move to
  `spawn_blocking`.
- **`agents-in-a-box-10a`** — witr re-spawns `witr --version` on every refresh
  while non-Ready (`plugin.rs:147-154`). Cache detect with a TTL.

## Coverage

Event loops, timers, threads, and file-IO paths were traced for every in-scope
crate: `ainb-core`, `ainb-plugin-runtime` (+protocol/sdk/types), `abtop`,
`burndown`, `cts-v2`, `notifyd`, `session-reader`, `witr`, and
`ainb-hangar-daemon`/`store`/`core`. `abtop` and `witr` are event-driven and
clean; `cts-v2` is a test harness, not shipped runtime. The hangar daemon is
well-architected for idle (0% CPU, bounded buffers, event-driven RPC).

Full method, raw per-crate notes, and sample tables:
`.agents/research/ainb-tui-perf-review-sweep.md` and
`.agents/research/ainb-tui-perf-measurements.md`.
