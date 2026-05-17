# Specification: Plugin Phase 6 — Data Plane

**Generated from:** `plans/plugin-phase-6-data-plane.md`
**Interview date:** 2026-05-09
**Version:** 1.0
**Plugin ABI:** bumps to **1.2.0**

## Executive Summary

Phase 6 makes burndown a real plugin by introducing a separate `ainb-plugin-session-reader` plugin that owns the data plane, plus a generic host-owned cache primitive and synchronous request/response over the event bus. Host loses every reference to `usage_data`/`burndown`/`UsageData`. Both TUI and CLI route through the plugin. Acceptance is integration: CLI tests gate CI; tmux tests gate local pre-merge.

## Objectives

### Primary Goals

1. **Host has zero knowledge** of `usage_data`, `burndown`, or any plugin-specific semantic — TUI **and** CLI.
2. `ainb-plugin-session-reader` owns parsing of Claude/Codex/Gemini/Copilot logs and publishes a single combined `sessions.usage_data` event.
3. `ainb-plugin-burndown` subscribes (TUI) or synchronously requests (CLI) and renders.
4. New host primitives (`fs_*`, `cache_*`, `request_data`) are generic — no plugin-specific names anywhere in host code.
5. Architectural gate: `! grep -RE "burndown|usage_data|UsageData" crates/ainb-core/src/` returns zero matches with **no carve-outs**.

### Success Metrics

- **Architectural**: zero matches in the grep gate; `crates/ainb-core/src/models/usage/parsers/` directory deleted.
- **Functional CLI**: `ainb usage report --format=json` produces non-empty output via plugin pipeline; byte-identical to frozen baseline.
- **Functional TUI**: tmux integration test sees real per-project costs in burndown screen within 5s of nav.
- **Resilience**: tripwire 4/4 (existing 3 + new full-pipeline).
- **Failure isolation**: removing session-reader from `dist/plugins/` shows actionable empty state in burndown, not silent staleness.

## Scope

### In Scope

- `fs_read_dir(path) -> Vec<String>` and `fs_read_file(path) -> Vec<u8>` host fns gated by `read_claude_logs` / `read_codex_logs` capabilities.
- Path-traversal guard (`crates/ainb-plugin-host/src/path_guard.rs`).
- Generic plugin-scoped cache primitive: `cache_get(key) -> Option<bytes>`, `cache_put(key, bytes, ttl_secs) -> ()`. Scoped to `~/.agents-in-a-box/cache/<plugin-id>/<key>`. Per-plugin quota (default 200 MB), global LRU eviction.
- `request_data(topic, payload, timeout_ms) -> Vec<u8>` host fn — synchronous request/response over event bus.
- New crate `ainb-plugin-types-sessions` containing `UsageData` wire schema. Both plugins depend on it; nothing else does.
- New crate `ainb-plugin-session-reader` (cdylib, wasm32-wasip1). Walks log dirs, builds `UsageData`, publishes `sessions.usage_data` on init + every 30s tick + on user `f` keypress trigger.
- Burndown CLI handlers for **9 of 12** `usage` subcommands: `report`, `status`, `today`, `month`, `export`, `optimize`, `compare`, `yield`, `model-alias`.
- Burndown subscribes to `sessions.usage_data` for TUI render path. Loose-coupled: plugin loads in any order, re-renders on event arrival.
- Manifest schema additions: `[provides] cli_namespaces`, `[paths]` block with `$HOME` expansion + user override, `[subscribes] topics`. ABI version `1.2.0`.
- CLI integration test surface (`--features cli-tests`) — **the CI gate**.
- tmux integration test surface (`--features tmux-tests`) — **local-only**, developer pre-merge sanity.
- Existing `~/.agents-in-a-box/cache/usage.sqlite` dropped + rebuilt on first launch with the new plugin (one-time ~30s rebuild).

### Out of Scope

- `plan` / `currency` / `cache` admin subcommands — stay in host (config not analytics).
- Streaming per-turn events — Phase 6 publishes full snapshots only.
- Per-provider event topics (`sessions.usage_data.claude`, etc.) — Phase 7+.
- File watcher (inotify / fsevents) refresh — needs new `fs_watch` capability, deferred.
- Daemon mode for CLI cold-start — accept ~200ms wasmi spinup latency.
- Async `host_task_spawn` for long scans — partial publish + resume covers the budget exhaustion case.
- Cache schema migration code — drop-and-rebuild instead.
- Multiple plugins claiming the same CLI namespace via explicit precedence — first-loaded wins.

### Future Considerations

- Phase 7: `cache` admin subcommand routing (or remove if plugin owns its cache fully).
- Phase 7: per-provider event topics for plugins that only care about one provider.
- Phase 8: SQLite-backed shared session store, queried via host fn.
- Phase 8: file watcher capability + reactive scanning.

## Technical Requirements

### Architecture

**Two plugins, one host primitive set, no host knowledge of plugin semantics.**

```
┌──────────────────────────────────────────────────────────┐
│ Host (ainb)                                              │
│ ─────────────────────────────────────────────────────── │
│ TUI            CLI shim         Generic primitives       │
│  │              │                 fs_read_dir/file       │
│  │              │                 cache_get/put          │
│  │              │                 request_data           │
│  │              │                 event_bus              │
│  ▼              ▼                 path_guard             │
│ Plugin host (wasmi runtime, capability-gated Linker)     │
└─────┬───────────┬───────────────┬────────────────────────┘
      │           │               │
      ▼           ▼               ▼
 ┌────────┐ ┌────────────┐ ┌──────────────────────┐
 │burndown│ │session-    │ │plugin-types-sessions │
 │plugin  │◄│reader      │ │(shared wire crate)   │
 └────────┘ │plugin      │ └──────────────────────┘
            └────────────┘
```

- **session-reader**: walks `~/.claude/projects/`, `~/.codex/sessions/`, etc.; builds `UsageData`; publishes `sessions.usage_data` on init/tick/refresh. Caches via `cache_get/put` with 1h TTL.
- **burndown** (TUI path): subscribes to `sessions.usage_data`, replaces snapshot, re-renders on event.
- **burndown** (CLI path): handler calls `request_data("sessions.usage_data", b"", 5000)`, formats output, emits via fd_write capture.

### Components

| Component | Purpose | Crate / Path | New? |
|-----------|---------|--------------|------|
| `path_guard` | Capability-scoped allowlist root + traversal check | `crates/ainb-plugin-host/src/path_guard.rs` | **New** |
| `host_fns/fs` | `fs_read_dir`, `fs_read_file` | `crates/ainb-plugin-host/src/host_fns/fs.rs` | **New** |
| `host_fns/cache` | `cache_get`, `cache_put` (TTL + LRU + per-plugin quota) | `crates/ainb-plugin-host/src/host_fns/cache.rs` | **New** |
| `host_fns/request_data` | Synchronous bus walk, blocking on matching reply | `crates/ainb-plugin-host/src/host_fns/request.rs` | **New** |
| `ainb-plugin-types-sessions` | `UsageData` and friends (wire-stable) | `crates/ainb-plugin-types-sessions/` | **New** |
| `ainb-plugin-session-reader` | Provider parsers + event publish | `crates/ainb-plugin-session-reader/` | **New** |
| Burndown manifest update | Drop fs caps, declare `[subscribes]`, declare `[provides] cli_namespaces = ["usage"]` | `crates/ainb-plugin-burndown/plugin.toml` | Edit |
| Burndown CLI handler | 9 subcommands routed via plugin dispatch | `crates/ainb-plugin-burndown/src/cli.rs` | **New** |
| Host CLI shim | `cli/usage.rs` becomes pure dispatch shim, no domain logic | `crates/ainb-core/src/cli/usage.rs` | Reduce |
| Manifest schema | `cli_namespaces`, `paths`, `subscribes`, ABI `1.2.0` | `crates/ainb-plugin-api/src/manifest.rs` | Edit |
| CLI integration tests | `--features cli-tests`, exit-code + byte-identity gates | `crates/ainb-core/tests/cli_burndown.rs` | **New** |
| tmux integration tests | `--features tmux-tests`, capture-pane + send-keys | `crates/ainb-core/tests/tmux_burndown.rs` | **New** |
| Tmux test helper | Spawn / wait_for_text / send_keys / capture / safe drop | `crates/ainb-core/tests/support/tmux_demo.rs` | **New** |
| Tripwire extension | New 4th test: full pipeline byte-identical to baseline | `crates/ainb-core/tests/tripwire.rs` | Edit |
| New CTS canaries | `axis_13_fs_read_with_capability`, `axis_14_fs_read_without_capability_refused` | `crates/ainb-plugin-host/tests/cts/canaries/` | **New** |
| In-tree parser dir | **Deleted** (proves CLI fully plugin-routed) | `crates/ainb-core/src/models/usage/parsers/` | **Delete** |

### Integrations

- **Capability system**: existing `read_claude_logs`, `read_codex_logs`, `write_plugin_data` flags from Phase 5b stay; `fs_read_*` host fns gated by them via Linker omission (existing CTS axis 6 pattern).
- **Event bus**: existing publish/subscribe extended with synchronous `request_data` walk.
- **Insta snapshots**: existing baselines reused for tripwire byte-identity check.
- **xtask build chain**: `build-plugins.sh` extended to build session-reader + bump ABI; `build-canaries` extended for new fs canaries.

### Performance Requirements

- **Per-tick fuel cap**: scan must complete or yield within fuel budget; partial publish + resume next tick.
- **CLI cold-start**: < 500 ms wasmi spinup + scan acceptable; daemon mode deferred.
- **Cache hit**: `cache_get` on warm cache returns in < 10 ms.
- **TUI first-data latency**: < 5 s from process start to first `sessions.usage_data` event on a 200K-file workload (cached).
- **Cache quota**: 200 MB per plugin, host enforces global LRU when total cache > 1 GB.

### Security Requirements

- **Path traversal**: every fs host fn canonicalizes input and compares against capability-derived allowlist root. Reject `..`, symlinks pointing outside, NUL bytes. Documented in `path_guard.rs` with adversarial unit tests.
- **Capability denial**: plugin without `read_claude_logs` cannot call `fs_read_dir` — Linker omission causes instantiation failure (matches CTS axis 6).
- **Cache scoping**: plugin can only `cache_get`/`cache_put` keys under its own plugin id. Host computes physical path; plugin never sees absolute paths.
- **TOCTOU on file reads**: accepted (read-only logs, low severity).
- **No subprocess spawn** in either plugin's manifest.

## User Experience

### CLI flows

1. `ainb usage report` — host parses argv, strips host-globals (`--format`, `--profile`, `--log-level`), dispatches to burndown plugin's `report` handler. Plugin calls `request_data("sessions.usage_data", b"", 5000)` on session-reader, formats, returns. Host writes captured stdout, exits with plugin's status.
2. `AINB_DISABLE_PLUGINS=1 ainb usage report` — host's CLI shim detects no plugin claims `usage` namespace; exits 2 with `error: usage analytics requires the burndown plugin (install via 'ainb plugin install burndown')`. Stdout empty.
3. `ainb usage` (with burndown loaded but session-reader absent) — burndown CLI handler's `request_data` times out at 5s, returns exit 2 with `error: usage analytics requires the session-reader plugin`.

### TUI flows

1. **Cold start with both plugins** — host loads plugins. burndown shows "Waiting for session-reader…" empty state. session-reader publishes first event within ~5s. burndown re-renders with real data.
2. **Disabled** — `AINB_DISABLE_PLUGINS=1` shows current `[plugin analytics: rendering...]` placeholder.
3. **Refresh** — user presses `f` in burndown screen. Plugin sends a `sessions.refresh_request` event to session-reader. session-reader re-scans (mtime-aware), publishes fresh `sessions.usage_data`. burndown re-renders.
4. **Provider parser failure** — Codex JSONL corrupt; session-reader publishes UsageData with Claude/Gemini/Copilot only. burndown shows warning chip on Codex tab: "Codex parser error — see logs".

### Edge Cases

| Scenario | Expected Behavior |
|----------|-------------------|
| User has no Claude logs, has Codex logs | session-reader publishes UsageData with empty Claude provider, populated Codex |
| User overrides `~/.claude/projects` to non-default path via plugin config | Manifest path expansion respects override; capability allowlist root re-canonicalizes |
| Plugin tries to read `/etc/passwd` | `path_guard` rejects; plugin gets `Err(PathDenied)`; logged as security event |
| First launch after Phase 6 deploy | Old `usage.sqlite` detected, deleted, log info: `rebuilding usage cache for plugin schema`. Re-scan ~30s. |
| Plugin crashes mid-scan | Existing CTS axis 8 pattern: host marks plugin degraded, render shows degraded badge, other plugins unaffected |
| CLI invoked while TUI running on same machine | Both share host-cache; CLI hits warm cache, returns < 100 ms |
| Two plugins both declare `cli_namespaces = ["usage"]` | First-loaded wins; warning logged. Stevie's pinned config is alphabetical-by-default. |

## Constraints & Dependencies

### Technical Constraints

- **wasmi 0.40** — no WASI preview1; existing host fn capture pattern (`fd_write`) reused for plugin stdout.
- **wasm32-wasip1 target** — rusqlite needs wasi-sdk emulated libs (already in build chain).
- **Cargo workspace** — new plugin crates go in `[workspace.exclude]` sub-workspace per existing pattern.
- **Plugin ABI bump to 1.2.0** — old plugins (CTS canaries) still compile under 1.1.0; manifest validator accepts both during transition.
- **macOS path canonicalization quirk** — `/private/var` vs `/var`; canonicalize twice for compare.

### External Dependencies

- `tmux` binary required for `--features tmux-tests` (skipped if missing with explanatory message).
- No network — both plugins declare `network = []`.

### Timeline Constraints

- Phase 6 ships as **one PR** with **atomic per-phase commits** mapping to 6a / 6b / 6c / 6c-cli / 6d / 6e / 6f.
- Estimated 5–7 working days for a single engineer; parallelizable via swarm only if calendar pressure dominates quality.

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| Path-traversal CVE in fs caps | High | Medium | Canonicalize twice + allowlist + adversarial unit tests + 2 new CTS canaries gating CI |
| `request_data` deadlock if session-reader hangs | High | Low | 5s timeout, exit 2 with actionable stderr |
| CLI cold-start > 500 ms | Medium | Medium | Cache-warm path; profile if regression; daemon mode deferred to Phase 8 |
| 200K-file scan trips fuel budget | Medium | High | Partial publish + resume + mtime incremental |
| `UsageData` schema drift between plugins | Medium | Low | Shared crate `ainb-plugin-types-sessions` enforces type-level compatibility; tripwire byte-identity catches wire drift |
| tmux integration test flakiness on local run | Low | Medium | Poll-with-ceiling, verbose failure dumps, isolated session names; CI doesn't run them so PR cycles aren't affected |
| Cache LRU eviction kicks during active scan | Low | Low | Per-plugin quota (200 MB) sized for full UsageData snapshot; eviction only on cross-plugin contention |
| ABI bump breaks CTS canaries declared at 1.1.0 | Low | Low | Manifest validator accepts both during transition; canaries explicitly stay at 1.1.0 to test back-compat |

## Decisions Made

### Cache: host-owned generic primitive (Stevie diverged from default)

- **Decision**: Host owns generic plugin-scoped KV with TTL (`cache_get`, `cache_put`). Schema knowledge stays in plugin.
- **Alternatives considered**: plugin-owned SQLite under `write_plugin_data` (default), shared host-owned SQLite with query primitive.
- **Rationale**: Generic primitive serves multiple future plugins without coupling host to any plugin's schema. Host stays "dumb" in the sense Stevie wants — it manages bytes, not domain types.

### CLI scope: 9 of 12 subcommands

- **Decision**: Plugin handles `report`/`status`/`today`/`month`/`export`/`optimize`/`compare`/`yield`/`model-alias`. Admin (`plan`/`currency`/`cache`) stays in-tree.
- **Alternatives**: only `report` (default), all 12.
- **Rationale**: Admin subcommands are config, not analytics — they don't belong in a plugin. Doing 9 means parser dir CAN be fully deleted, which strengthens the architectural gate.

### Tmux tests local-only, CLI tests as CI gate (Stevie diverged)

- **Decision**: tmux integration tests gated `--features tmux-tests`, run locally before merge; CLI integration tests gate CI.
- **Alternatives**: tmux as required CI gate (default), nightly.
- **Rationale**: CLI is faster, deterministic, and covers the same dispatch+data-flow path. Tmux is the developer's "did the render path actually work?" sanity check, not a CI-replicable surface. Avoids tmux-on-CI install + flakiness costs.

### UsageData schema in separate crate

- **Decision**: New `crates/ainb-plugin-types-sessions/` owns `UsageData` and related types.
- **Alternatives**: stuff into `ainb-plugin-api`, no shared type.
- **Rationale**: Plugins that don't care about session usage don't pull in domain types. Keeps `ainb-plugin-api` lean and stable.

### Single PR, atomic commits per sub-phase

- **Decision**: One PR on `feat/plugin-phase-6`. Commits map 1:1 to 6a/6b/6c/6c-cli/6d/6e/6f. Single merge commit (per existing merge-not-squash rule).
- **Alternatives**: PR per sub-phase, swarm parallelism.
- **Rationale**: Tight integration between phases; serial review by phase is faster than coordinating six PR cycles. Atomic commits enable bisect.

### Drop-and-rebuild old cache

- **Decision**: First launch with new plugin host detects `~/.agents-in-a-box/cache/usage.sqlite`, deletes it, plugin re-scans.
- **Alternatives**: in-place migration (default candidate), leave orphan.
- **Rationale**: Migration code rots; one-time 30s rebuild is acceptable cost. Single source of truth post-Phase 6.

### Deferred Decisions

- **Per-provider event topics** — single combined event for now; split when a plugin actually needs only one provider.
- **Daemon mode for CLI** — measure cold-start in real workloads before committing complexity.
- **File watcher capability** — added when an actual user flow demands sub-30s freshness.

## Implementation Notes

### Priority Order

1. **6a** — fs host fns + capabilities + path_guard + 2 CTS canaries. Foundation.
2. **6b** — `ainb-plugin-types-sessions` + `ainb-plugin-session-reader` cdylib. Move parsers, publish event.
3. **6c** — burndown TUI path: subscribes, drops fs caps. Tripwire equivalence.
4. **6c-cli** — burndown CLI handlers (9 subcommands) + host CLI shim + `request_data` host fn. **CI integration tests live here.**
5. **6d** — host purge: delete `models/usage/parsers/`, grep gate green with no carve-out.
6. **6e** — tmux integration test harness + 3 demo gates. **Local-only.**
7. **6f** — tripwire 4/4 with full real pipeline.

### Technical Debt Accepted

- `network = []` capability check exists but no allowlist enforcement yet (no plugin needs it). Phase 7+.
- CLI subcommands `plan`/`currency`/`cache` stay in-tree until they have an obvious plugin home.
- `request_data` is a synchronous block on the host main thread — acceptable because plugin code is all on a separate wasmi store; host UI thread isn't blocked. Revisit if multi-plugin synchronous chains introduce latency.

## Acceptance Criteria

### Build gates

- `cargo build --workspace --release`
- `cargo test --workspace --features test-support` green
- `cargo test --workspace --features cli-tests` green **(CI required)**
- `cargo test --workspace --features tmux-tests` green **(local pre-merge)**
- `cargo xtask build-plugins && cargo xtask build-canaries` green
- CTS 14/14 (existing 12 + 2 new fs axes)

### Architectural gates (must all be true)

- `! grep -RE "burndown|usage_data|UsageData" crates/ainb-core/src/ --include='*.rs'` returns zero matches
- `! [ -d crates/ainb-core/src/models/usage/parsers ]` (directory deleted)
- Plugin manifests bumped to `ainb_min_version = "1.2.0"`; CTS canaries stay at `1.1.0` (proves back-compat)
- No `[subscribes]` or `[publishes]` knowledge in host code

### Demo gates (the real ones)

**CLI** (CI gate):
- `ainb usage report --format=json` with both plugins returns non-empty JSON parsing as `UsageData`
- `AINB_DISABLE_PLUGINS=1 ainb usage report` exits 2, stderr matches `requires the burndown plugin`, stdout empty
- Byte-identical match against `tests/baselines/usage_cli_json_report.snap` over frozen fixture
- All 9 plugin-routed subcommands return non-zero data on fixture

**TUI** (local pre-merge):
- tmux test sees burndown rendering real per-project costs within 5s of nav (no test-injected data)
- `AINB_DISABLE_PLUGINS=1` shows placeholder, no analytics
- Removing `dist/plugins/session-reader/` shows `Waiting for session-reader plugin…` empty state — proves data-flow gap is detectable, not silent

## Open Questions

- [ ] Worker scope: at least 5–7 days for a single engineer. Confirm if Stevie wants a swarm spawned anyway for parallelism on independent phases (6a foundation, 6e harness can run parallel).
- [ ] `cache_put` failure semantics when over global LRU limit: drop newest, drop oldest from this plugin, or fail loud? Default to LRU eviction across plugins — confirm.
- [ ] Should `ainb plugin list` show generic capability summary (`uses cache (12MB)`, `subscribes to: sessions.usage_data`) for installed plugins? Operator visibility nice-to-have; deferred unless Stevie wants it in 6.
- [ ] Insta baseline regeneration: when an `ainb-plugin-types-sessions` field changes (semver minor), is regenerating the baseline part of the change PR or a separate one?

---

*This specification was generated through systematic interview of the plan author.*
