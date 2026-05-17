# Phase 6 — Plugin Data Plane (session-reader plugin + fs capabilities)

**Status:** Draft
**Depends on:** Phases 0–5b complete (plugin host MVP merged into feat/plugin)
**Beads epic:** agents-in-a-box-6tu (extends with new sub-issues)

## Why

Phase 5b extracted the burndown **render** path into a plugin, but kept the **data path** in the host: live ainb shows "Scanning…" forever because nothing publishes `burndown.usage_data` to the plugin. The host has no business knowing what `usage_data` means — that violates the plugin contract ("host knows nothing about any specific plugin").

Phase 6 makes burndown a real plugin by introducing a **separate `ainb-plugin-session-reader` plugin** that owns the data plane. Burndown subscribes to its events. Host stays dumb.

This also unlocks future plugins (kanban, agent-discovery, claude-peers-mcp port) reusing the same session stream without re-implementing the parser.

## Goals

1. Host has zero knowledge of `usage_data`, `burndown`, or any plugin-specific semantics — TUI **and** CLI.
2. `ainb-plugin-session-reader` cdylib walks Claude/Codex/Gemini/Copilot session files and publishes generic `sessions.usage_data` events.
3. `ainb-plugin-burndown` subscribes, aggregates, renders. Drops its file-read capabilities.
4. New host fns `fs_read_dir` + `fs_read_file` gated by `ReadClaudeLogs`/`ReadCodexLogs` capabilities with rock-solid path-traversal guards.
5. `ainb usage <subcmd>` dispatched through plugin's CLI handler — host's `usage` subtree becomes a thin shim that routes to whichever plugin claims the `usage` namespace.
6. **Acceptance is integration, not compilation.** Two real-binary test surfaces: (a) CLI integration — easy, deterministic stdout assertions; (b) tmux integration — proves the TUI path. Both gate Phase 6.

## What we're NOT doing

- Streaming per-turn events — Phase 6 publishes full snapshots only.
- Cache strategy redesign — keep mtime-based file invalidation; SQLite move-out is Phase 8.
- Provider-per-plugin split — one session-reader plugin handles all four providers.
- Removing the `usage` subcommand entry point from clap — that's still in the host, only the **handler** routes through the plugin.

## Open questions (decide before Phase 6b kickoff)

| # | Question | Default | Reason |
|---|----------|---------|--------|
| 1 | Cache ownership: in-tree or session-reader? | session-reader (under `write_plugin_data` cap) | If host owned cache, host would know plugin-specific schema |
| 2 | Refresh cadence: tick-driven or event-driven (file watcher)? | Tick-driven on user `f` keypress + once at init | File watcher needs new cap; defer |
| 3 | Event payload shape: msgpack `UsageData` or JSON-Value? | msgpack `UsageData` | Already used by tripwire dispatch; no schema rewrite |
| 4 | Init ordering: enforce session-reader-before-burndown? | No — burndown re-renders on event arrival | Avoids host-side dependency graph; loose coupling |
| 5 | Fuel budget for full scan: cap per-tick or async via host task? | Cap per-tick, short-circuit if budget exceeded, log warn | Async tasks need new ABI |

---

## Phase 6a — fs capabilities + host fns
<!-- wave: 1 | depends_on: [] | files: [crates/ainb-plugin-host/src/host_fns/fs.rs, crates/ainb-plugin-host/src/runtime.rs, crates/ainb-plugin-api/src/capabilities.rs, crates/ainb-plugin-api/src/manifest.rs] -->

### Deliverables

- `fs_read_dir(path: String) -> Vec<String>` host fn — lists JSONL files in the directory
- `fs_read_file(path: String) -> Vec<u8>` host fn — reads file bytes
- Capability allowlist enforcement:
  - `read_claude_logs` ⇒ paths under `~/.claude/projects/`
  - `read_codex_logs` ⇒ paths under `~/.codex/sessions/`
  - Both also expand `$HOME` substitution
- Path-traversal guards: reject `..`, reject symlinks pointing outside allowlist root, canonicalize before compare
- Plugin manifest: existing flags work as-is, no schema change

### Files

- `crates/ainb-plugin-host/src/host_fns/fs.rs` (new)
- `crates/ainb-plugin-host/src/runtime.rs` (wire host fns into Linker conditionally)
- `crates/ainb-plugin-host/src/path_guard.rs` (new — allowlist resolver + traversal check)
- `crates/ainb-plugin-api/src/capabilities.rs` (already declares `read_claude_logs`/`read_codex_logs` — verify)
- `crates/ainb-core/tests/cts/canaries/cts-fs-read/` (new CTS canary for capability denial)

### Test gates

- Unit tests in `path_guard.rs`:
  - `allowlist_root_resolves_home`
  - `traversal_attempt_rejected` (`../etc/passwd`, `~/.claude/../sensitive`)
  - `symlink_to_outside_rejected`
- Capability denial test: plugin without `read_claude_logs` calling `fs_read_dir` ⇒ instantiation refuses (Linker omission, matches existing CTS axis 6 pattern)
- New CTS canary `axis_13_fs_read_with_capability_succeeds` + `axis_14_fs_read_without_capability_refused`

### Risks

- Path canonicalization on macOS vs Linux differs (`/private/var` vs `/var`); use `std::fs::canonicalize` and treat both as equal
- TOCTOU: file replaced between canonicalize + read; accept (low-severity for read-only logs)

---

## Phase 6b — `ainb-plugin-session-reader` cdylib
<!-- wave: 2 | depends_on: [6a] | files: [crates/ainb-plugin-session-reader/, crates/ainb-core/src/models/usage/parsers/] -->

### Deliverables

- New crate `crates/ainb-plugin-session-reader/` (cdylib, wasm32-wasip1 target)
- Manifest: `read_claude_logs = true`, `read_codex_logs = true`, `write_plugin_data = true` (cache)
- **Move** Claude/Codex/Gemini/Copilot scanners from `crates/ainb-core/src/models/usage/parsers/` into the plugin
- Plugin's `init` walks all four log dirs, builds `UsageData`, publishes event:
  - Topic: `sessions.usage_data`
  - Payload: msgpack-encoded `UsageData` (shared type lives in `ainb-plugin-api`)
- Plugin's `tick` (every 30s, fuel-capped) re-scans changed files only (mtime check) and re-publishes if data changed

### Files

- `crates/ainb-plugin-session-reader/Cargo.toml` (new)
- `crates/ainb-plugin-session-reader/src/lib.rs` (new — Plugin trait impl)
- `crates/ainb-plugin-session-reader/src/scanner.rs` (new — moved parsers)
- `crates/ainb-plugin-session-reader/plugin.toml` (new — manifest)
- `crates/ainb-plugin-api/src/types/usage.rs` (lift `UsageData` here so both burndown + session-reader share without depending on host)
- `crates/ainb-core/src/models/usage/parsers/` — gut these, leave thin re-export pointing at the plugin types for the in-tree CLI's continued use (or duplicate; revisit Phase 7)
- `xtask/src/main.rs` — `build-plugins.sh`/`build-canaries` extends to build session-reader
- `dist/plugins/session-reader/{plugin.wasm,plugin.toml}` (build output)

### Test gates

- `cargo test -p ainb-plugin-session-reader` — golden-file fixtures for all 4 providers, parse → encode → decode roundtrip
- Fuel budget test: parsing 200K-file fixture stays under per-tick fuel cap or short-circuits cleanly
- Snapshot equivalence: `UsageData` from plugin matches in-tree scanner byte-for-byte on shared fixture

### Risks

- **`UsageData` schema sharing**: lifting into `ainb-plugin-api` couples plugin SDK to a domain type. Mitigation: keep it `serde`-stable, version-tagged, and document as part of the host-plugin event contract (not a host-private type).
- **Cache migration**: existing usage_cache (`~/.agents-in-a-box/cache/usage.sqlite`) — does plugin own it or copy logic? Default is plugin owns under `write_plugin_data` path scope. Migration script if format changes.
- **Parser cross-compile**: scanners use `chrono`, `serde`, `rusqlite`. rusqlite needs wasi-sdk emulated libs (already in build chain per memory). Verify session-reader compiles to wasm32-wasip1.
- **In-tree CLI**: parser moves wholesale into session-reader. Host CLI handler becomes a plugin dispatch shim (see 6c-cli below).

---

## Phase 6c — burndown subscribes
<!-- wave: 3 | depends_on: [6b] | files: [crates/ainb-plugin-burndown/] -->

### Deliverables

- Burndown manifest: drop `read_claude_logs`, `read_codex_logs`. Add `[subscribes]\ntopics = ["sessions.usage_data"]`.
- Plugin SDK: extend `Plugin` trait if needed so `handle_event` filters by subscribed topic (or burndown filters in handler — simpler).
- On `sessions.usage_data` event: replace internal `UsageData` snapshot, mark dirty, render fires on next tick.
- Drop the placeholder "Scanning session files…" text — replace with proper empty state ("Waiting for session-reader plugin…" with provider hint).

### Files

- `crates/ainb-plugin-burndown/plugin.toml` (drop fs caps, add subscribes)
- `crates/ainb-plugin-burndown/src/lib.rs` (drop scanner code, simplify `handle_event`)
- `crates/ainb-plugin-host/src/event_bus.rs` (verify topic-based routing already filters by `subscribes` — wire if missing)

### Test gates

- Update tripwire: `cli_usage_via_plugin_matches_baseline` already exercises plugin-without-data path; add `analytics_via_session_reader_matches_baseline` that loads BOTH plugins and verifies real data flow
- Unit test: burndown receives event → re-render produces non-empty buffer

### Risks

- Late-event re-render: existing render flow is tick-driven; ensure event arrival schedules a re-render (or relies on next tick — accept latency)

---

## Phase 6c-cli — CLI rerouting (`ainb usage` → plugin dispatch)
<!-- wave: 3 | depends_on: [6b] | files: [crates/ainb-core/src/cli/usage.rs, crates/ainb-core/src/cli/registry.rs, crates/ainb-plugin-burndown/src/cli.rs, crates/ainb-plugin-session-reader/src/lib.rs] -->

### Deliverables

CLI handlers for `usage report|status|today|month|export|optimize|...` route through plugin dispatch instead of in-tree code. Host owns clap surface (so `--help` still works without plugins loaded), plugin owns implementation.

- Host's `cli/usage.rs` becomes a thin shim:
  1. Parse argv with existing clap derive
  2. Init plugin host
  3. Strip host-global flags (`--format`, `--profile`, `--log-level`) per existing memory `reference_plugin_clap_strip_global_flags.md`
  4. Call `host.dispatch_cli("burndown", "usage", &remaining_argv)` — burndown's CLI handler runs
  5. Burndown sends `cli_request_data` event to session-reader, awaits `sessions.usage_data` event reply (synchronous round-trip via host's event bus blocking dispatch)
  6. Burndown formats output, returns via `host.captured_stdout`
  7. Host writes captured stdout to real stdout, exits with plugin's status code
- Failure modes:
  - Plugin not loaded: print `error: usage analytics requires the burndown plugin (install via 'ainb plugin install burndown')` to stderr, exit 2
  - `AINB_DISABLE_PLUGINS=1`: same error message, exit 2 (consistent with TUI's empty state)
  - Plugin loaded but session-reader missing: print `error: usage analytics requires the session-reader plugin`, exit 2

### Files

- `crates/ainb-core/src/cli/usage.rs` — gut in-tree implementation, leave clap structs + dispatch shim
- `crates/ainb-core/src/cli/registry.rs` — register usage subcommand as plugin-dispatched
- `crates/ainb-plugin-burndown/src/cli.rs` — handler for `report`/`status`/`today`/`month`/`export`/`optimize`/etc.
- `crates/ainb-plugin-burndown/src/lib.rs` — wire the new request/response with session-reader (synchronous data fetch on CLI path)
- `crates/ainb-plugin-session-reader/src/lib.rs` — handle `sessions.fetch` event for synchronous CLI use case (in addition to the periodic publish for TUI)
- `crates/ainb-plugin-host/src/runtime.rs` — verify `dispatch_cli` path correctly drains stdout/stderr per `reference_wasmi_fd_write_capture.md`

### Test gates

- `cargo test -p ainb --features test-support --test plugin_burndown_cli_dispatch` extends to cover all `usage` subcommands (was just `report`)
- New CLI integration test (gated `#![cfg(feature = "cli-tests")]`):
  - `cli_usage_report_via_plugin_pipeline`:
    1. Build release ainb + both plugins
    2. Spawn `ainb usage report --format=json` with `AINB_PLUGIN_ROOT=$DIST`, real `HOME`
    3. Capture stdout, assert it parses as JSON with non-empty `by_project` array
  - `cli_usage_report_fails_cleanly_without_plugins`:
    1. Spawn `AINB_DISABLE_PLUGINS=1 ainb usage report`
    2. Assert exit 2, stderr matches `requires the burndown plugin`, stdout empty
  - `cli_usage_report_byte_identical_to_baseline`:
    1. Spawn against frozen fixture session dir (override `HOME`)
    2. Stdout byte-equal to `tests/baselines/usage_cli_json_report.snap`
  - `cli_usage_today_subcommand_routes_to_plugin`:
    1. Spawn `ainb usage today`, assert non-empty plain-text output

### Risks

- **Synchronous fetch via event bus**: existing event bus is fire-and-forget. CLI needs request/response. Options: (a) host fn `request_data(topic, payload, timeout) -> Vec<u8>` synchronously walks event bus; (b) burndown CLI handler imperatively calls into session-reader via a new direct-call host fn. (a) is cleaner architecturally; (b) is faster to ship. Pick (a).
- **Subcommand surface drift**: every `usage` subcommand has its own clap args + output format. Plugin must implement all. Mitigation: golden-file test per subcommand.
- **Cold-start latency**: spinning up wasmi for each `ainb usage` invocation adds ~200ms. Acceptable. If users complain, add a daemon mode later.

---

## Phase 6d — host purge
<!-- wave: 4 | depends_on: [6c] | files: [crates/ainb-core/src/main.rs, crates/ainb-core/src/app/state.rs, crates/ainb-core/src/plugins/] -->

### Deliverables

- Remove **all** host-side `usage_data` / `UsageData` / `burndown` references — TUI startup, app state, **and** CLI handler
- After 6c-cli moved CLI implementation into the plugin, in-tree scanner code in `crates/ainb-core/src/models/usage/parsers/` is fully unreachable — delete it
- Verify no `burndown::*` import outside the plugin
- Verify CLI test surface still green via plugin dispatch only

### Files

- `crates/ainb-core/src/main.rs`
- `crates/ainb-core/src/app/state.rs` (remove any usage_data fields/refs feeding the TUI screen)
- `crates/ainb-core/src/plugins/` (host loader stays generic)
- `crates/ainb-core/src/models/usage/parsers/` — delete entire dir
- `crates/ainb-core/src/models/usage.rs` — strip parser refs, leave only the wire type if still re-exported (or delete)
- `crates/ainb-core/src/cli/usage.rs` — confirm reduced to clap+dispatch shim, no domain logic

### Test gates

- `! grep -RE "burndown|usage_data|UsageData" crates/ainb-core/src/ --include='*.rs'` ⇒ **zero matches across the entire host** (no `--exclude-dir=cli` carve-out)
- `cargo test --workspace --features test-support` green
- CTS 12/12 still green
- `cargo build --workspace` succeeds with deleted parser dir (proves nothing referenced it)

### Risks

- Stripping touches large files; risk of accidental breakage in adjacent code paths. Mitigation: split the purge into one commit per file, easy to bisect.

---

## Phase 6e — tmux integration test harness
<!-- wave: 4 | depends_on: [6c] | files: [crates/ainb-core/tests/support/, crates/ainb-core/tests/tmux_burndown.rs] -->

### Deliverables

This phase is the **acceptance gate**, not a nice-to-have. Compilation + unit tests do NOT prove the plugin works.

- New module `crates/ainb-core/tests/support/tmux_demo.rs` (new test helper, not in production lib)
- Helper functions:
  - `spawn_ainb(env: &[(&str, &str)]) -> TmuxSession` — spawns release binary in fresh tmux session, returns handle
  - `wait_for_text(s: &TmuxSession, pattern: &str, timeout: Duration) -> Result<()>`
  - `send_keys(s: &TmuxSession, keys: &str)`
  - `capture_pane(s: &TmuxSession) -> String`
  - `assert_visible(s: &TmuxSession, pattern: &str)`
  - `Drop` impl on `TmuxSession` calls `tmux kill-session -t <name>` (named, never wildcard, never `kill-server`)
- Tests in `crates/ainb-core/tests/tmux_burndown.rs` (gated `#![cfg(feature = "tmux-tests")]`, requires real `dist/plugins/`):
  - `burndown_renders_real_data_via_session_reader_plugin`:
    1. Build release ainb + session-reader + burndown into `dist/plugins/`
    2. Spawn ainb with `AINB_PLUGIN_ROOT=$DIST`
    3. Wait for `Usage Analytics` text (plugin chrome rendered)
    4. Send keys to navigate to burndown screen if not default
    5. `wait_for_text` matching `Total Calls\b\s+\d+` (real data, not "Scanning…")
    6. `assert_visible("By Project")` — confirms aggregator ran
  - `burndown_screen_placeholder_when_plugins_disabled`:
    - Spawn with `AINB_DISABLE_PLUGINS=1`
    - Assert `"plugin analytics: rendering"` placeholder visible (current empty state)
    - Assert `"Total Calls"` NOT visible
  - `burndown_waits_when_session_reader_missing`:
    - Spawn with `AINB_PLUGIN_ROOT` containing burndown only (no session-reader)
    - Assert burndown chrome renders, "Waiting for session-reader" empty state visible, no real data

### Files

- `crates/ainb-core/tests/support/tmux_demo.rs` (new)
- `crates/ainb-core/tests/support/mod.rs` (export)
- `crates/ainb-core/tests/tmux_burndown.rs` (new)
- `crates/ainb-core/Cargo.toml` (add `tmux-tests` feature)

### Test gates

- `cargo test -p ainb --features tmux-tests` green on macOS + Linux
- Tests detect missing tmux binary and skip cleanly with explanatory message (CI matrix may not have tmux)
- Each test runs in isolated session; teardown verified via `tmux has-session -t <name>` returning non-zero post-test

### Risks

- **Test flakiness**: TUI render timing varies. Mitigation: `wait_for_text` polls capture-pane every 200ms up to 30s ceiling, with verbose failure dump (last capture + duration).
- **CI tmux availability**: may need `brew install tmux` / `apt install tmux` step in workflow.
- **Concurrent tests stomping sessions**: each test generates unique session name `tmux-test-<test_name>-<pid>-<ts>`.
- **`tmux kill-server` discipline**: explicit safeguard — `Drop` impl calls `tmux kill-session -t <exact-name>` only. Documented in helper module.

---

## Phase 6f — tripwire extension
<!-- wave: 5 | depends_on: [6c, 6e] | files: [crates/ainb-core/tests/tripwire.rs] -->

### Deliverables

Extend tripwire to cover the real two-plugin pipeline (currently injects fake data):

- New test `analytics_via_session_reader_matches_baseline`:
  - Load BOTH `session-reader` and `burndown` plugins
  - Pre-stage a fixture session-data dir (small Claude JSONL fixtures under tmpdir)
  - Override `HOME` env var so session-reader's allowlist resolution finds fixtures
  - Tick host until burndown's render buffer matches insta baseline
  - Byte-identical comparison to the existing `usage_cli_json_report.snap` baseline
- Existing tests stay (they prove the data-injection contract still works for unit-level)

### Test gates

- Tripwire 4/4 (was 3/3)
- Insta snapshot stable across re-runs

### Risks

- Fixture data must be deterministic (timestamps, project paths). Mitigation: dedicated `tests/fixtures/sessions/` with frozen content.

---

## Acceptance criteria (all must pass before closing 6tu Phase 6)

### Build gates

- `cargo build --workspace --release`
- `cargo test --workspace --features test-support` green
- `cargo test --workspace --features cli-tests` green (CLI integration — fastest, deterministic)
- `cargo test --workspace --features tmux-tests` green (TUI integration — proves render path)
- `cargo xtask build-plugins && cargo xtask build-canaries` green
- CTS 12/12 + new 14/14 (with new fs canaries)

### Architectural gate

- `! grep -RE "burndown|usage_data|UsageData" crates/ainb-core/src/ --include='*.rs'` returns zero matches **across the whole host**, including CLI
- `crates/ainb-core/src/models/usage/parsers/` directory deleted (proves CLI now uses plugin path exclusively)
- Host has no `[subscribes]` or `[publishes]` knowledge — those live in plugin manifests only

### Demo gates (the real ones)

CLI integration (preferred — easy, deterministic, runs first in CI):

- `ainb usage report --format=json` with both plugins loaded produces non-empty JSON parsing as `UsageData`
- `AINB_DISABLE_PLUGINS=1 ainb usage report` exits 2 with stderr `requires the burndown plugin`, stdout empty
- Byte-identical output vs `tests/baselines/usage_cli_json_report.snap` against frozen fixture session dir

TUI integration (tmux):

- Tmux integration tests pass on developer machine and CI
- Manual `tmux attach` to demo session shows burndown rendering real per-project costs/calls within 5s of nav, **without** any test-injected data
- `AINB_DISABLE_PLUGINS=1` shows placeholder, no analytics
- Removing `dist/plugins/session-reader/` (leaving only burndown) shows burndown chrome with "Waiting for session-reader" empty state — proves data-flow gap is detectable, not silent

## Risks summary

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Path-traversal CVE in fs caps | M | H | Canonicalize + allowlist + new CTS canary |
| 200K-file scan trips fuel budget | H | M | Tick-cap + mtime incremental + per-tick partial publish |
| `UsageData` schema lock-in via `ainb-plugin-api` | L | M | Versioned in api crate, treat as wire format, tripwire byte-identity catches drift |
| Synchronous CLI fetch via async event bus | M | M | New `request_data(topic, payload, timeout)` host fn that walks bus and blocks; thread-safe |
| CLI cold-start latency from wasmi spinup | L | L | Accept ~200ms; daemon mode is post-MVP if users complain |
| tmux integration test flakiness | M | M | Poll-with-ceiling, verbose failure dumps, isolated session names |
| rusqlite cross-compile to wasm32-wasip1 | L | H | Already proven in burndown plugin per memory |

## Phase summary

| Phase | Wave | Output | Test gate |
|-------|------|--------|-----------|
| 6a | 1 | fs host fns + caps | unit + 2 new CTS axes |
| 6b | 2 | session-reader plugin | golden-file roundtrip + cross-compile |
| 6c | 3 | burndown subscribes (TUI render) | tripwire equivalence |
| 6c-cli | 3 | CLI dispatched through plugin | per-subcommand golden + CLI integration tests |
| 6d | 4 | host purge of plugin specifics | grep gate (no carve-out) + parser dir deleted |
| 6e | 4 | **CLI + tmux integration harnesses + tests** | **CLI + 3 tmux demo gates** |
| 6f | 5 | tripwire extension | 4/4 tripwire |

## References

- `crates/ainb-plugin-host/src/runtime.rs` — Linker pattern for capability gating (existing)
- `crates/ainb-plugin-api/src/capabilities.rs` — capability flags (already declared)
- `crates/ainb-plugin-burndown/plugin.toml` — manifest schema reference
- Memory: `reference_wasmi_fd_write_capture.md` — host fn capture pattern
- Memory: `reference_rusqlite_wasm32_wasi_sdk.md` — wasm SQLite build flags
- Memory: `feedback_swarm_leader_send_keys_enter.md` — tmux send-keys gotcha to enforce in test harness
