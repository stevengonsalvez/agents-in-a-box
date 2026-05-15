# ainb plugins

User-facing reference for the `ainb plugin` family of commands. If you're authoring a plugin, jump to [docs/plugin-authoring.md](./plugin-authoring.md). For the wire contract, [docs/plugin-spec/v2.md](./plugin-spec/v2.md).

## What is a plugin?

A plugin is a self-contained capsule that adds a screen, CLI subcommand, sidebar entry, or statusline segment to ainb without recompiling the host. A v2 plugin is a **native executable** the host spawns as a child process; the two talk JSON-RPC 2.0 over framed stdio. Plugins only see the host capabilities they declare in their manifest, and they cannot reach the network, filesystem, or subprocess launcher unless you grant those capabilities.

The first plugin shipped in-tree is **burndown**, which owns the Analytics screen and the `ainb usage` CLI subcommand tree. The pure-publisher **session-reader** is its data backend: it scans `~/.claude/projects/**` and `~/.codex/sessions/**` and chunked-publishes usage snapshots on the `sessions.usage_data` topic for burndown to render.

## Where things live

The host discovers plugins from a flat staging directory:

```text
dist/plugins/
├── burndown/
│   ├── burndown            (native executable, ad-hoc signed on macOS)
│   └── manifest.toml
└── session-reader/
    ├── session-reader
    └── manifest.toml
```

That layout is what `just stage-plugins` produces from in-tree crates, and what the host walks on startup. The `AINB_PLUGIN_ROOT` env var overrides it (defaults to `<workspace-root>/dist/plugins`).

Plugin-writable state lives under `~/.agents-in-a-box/plugins/<name>/` (override with `AINB_HOME`), gated by the `write_plugin_data` capability.

## Status of the install / marketplace flow

The Phase 4 marketplace + installer surface (`ainb plugin marketplace add | install | update | remove | search`) shipped against the v1 wasm packaging and is currently **stubbed**. Running any of those commands today returns:

> `error: the marketplace + installer flow is being re-cut against the subprocess ABI 2.0 (Phase 7c). Re-run after 7c lands.`

Until that lands, in-tree plugins are the only path. Build + stage:

```bash
cargo build --workspace
just stage-plugins
./target/debug/ainb tui
```

`AINB_DISABLE_PLUGINS=1 ainb tui` boots the host with the runtime disabled — useful when bisecting a plugin-induced regression.

## Commands that work today

### `ainb plugin list`

Enumerates plugins the runtime discovered at startup.

```bash
ainb plugin list
ainb plugin list --format json
```

Walks `RuntimeHandle::registered_plugins()` and prints `<id> <binary-path>` per row. JSON form is `{"plugins": [{"id": "...", "binary": "..."}]}`.

### `ainb plugin lint <id-or-path>`

Validates a staged plugin against the v2 contract without spawning it. `arg` is either a plugin id (resolves via `AINB_PLUGIN_ROOT`) or an on-disk path to a staging directory.

```bash
ainb plugin lint burndown
ainb plugin lint ./dist/plugins/burndown
ainb plugin lint burndown --format json
```

Checks manifest schema, binary presence, codesign status on macOS, and declared-vs-implemented method surface.

### `ainb plugin watch <id> [--duration <secs>]`

Tracks a plugin's lifecycle transitions in real time. Polls the runtime every 100 ms and prints state changes (`Idle → Spawning → Running → Backoff → Quarantined → ShuttingDown`) with timestamps. Useful for debugging a flaky crash-loop or a manifest the runtime rejects at init.

```bash
ainb plugin watch burndown
ainb plugin watch burndown --duration 30
```

Default duration is short enough that an unattended `watch` in CI doesn't hang.

### `ainb plugin tail <id> [--level <lvl>] [--since <ts>] [--duration <secs>]`

Streams the plugin's JSONL log entries (its `host.log()` calls plus the captured stderr drain) from `~/.agents-in-a-box/logs/agents-in-a-box-*.jsonl`. Filters on the structured `plugin` field so you only see the target plugin's lines.

```bash
ainb plugin tail burndown
ainb plugin tail burndown --level debug
ainb plugin tail session-reader --since 2026-05-15T10:00:00Z --duration 60
```

`level` defaults to `debug`. The `since` filter accepts RFC 3339.

## Capability model

Every plugin declares the host capabilities it needs in its `manifest.toml`:

```toml
[capabilities]
read_sessions       = true     # ~/.agents-in-a-box/sessions/**
read_claude_logs    = true     # ~/.claude/projects/**/*.jsonl
read_codex_logs     = false    # ~/.codex/sessions/**/*.jsonl
write_plugin_data   = true     # ~/.agents-in-a-box/plugins/<name>/ writable
event_bus           = true     # publish/subscribe across plugins
spawn_subprocess    = false    # exec child processes
network             = []       # bool or hostname allow-list
```

Default for every flag is **deny** (`false` / `[]`). The runtime rejects host-fn calls against a capability the manifest doesn't grant with JSON-RPC error code `-32001` (`CAPABILITY_DENIED`).

When the install flow returns, capability prompts will reappear at install time. Until then, capabilities are read straight from the on-disk manifest at runtime discovery — there is no separate `installed.toml` lockfile in the subprocess world.

See [docs/plugin-spec/v2.md §1](./plugin-spec/v2.md#1-manifest) for full semantics.

## Configuration

### Disable plugins

```bash
AINB_DISABLE_PLUGINS=1 ainb tui
```

Boots the host with the plugin runtime disabled — no discovery, no spawn. The Analytics screen falls back to a "plugin: rendering…" placeholder when its owner plugin isn't loaded.

### Override the plugin search root

```bash
AINB_PLUGIN_ROOT=/path/to/dist/plugins ainb tui
```

Tells the runtime to discover plugins from this directory instead of the workspace's `dist/plugins/`. Used by `just stage-plugins` for in-tree dev and by tests for isolated CI environments.

### Override the home directory

```bash
AINB_HOME=/some/path ainb tui
```

Redirects every host-managed path (`logs/`, `plugins/<name>/`, `sessions/`) under `/some/path/` instead of `~/.agents-in-a-box/`.

## Troubleshooting

- **"the marketplace + installer flow is being re-cut against the subprocess ABI 2.0"** — install flow not yet ported. Use in-tree crates + `just stage-plugins` until Phase 7c lands.
- **`ainb plugin list` returns "(no plugins registered)"** — the runtime found no manifests under `dist/plugins/`. Did you run `just stage-plugins`? Is `AINB_PLUGIN_ROOT` pointing at the right directory?
- **Plugin exits 137 in <1 ms on macOS, no stderr** — AMFI silent-kill. The binary's codesign got invalidated by a copy. Re-run `just stage-plugins`.
- **Analytics shows "plugin: rendering…" but never updates** — the burndown plugin crashed or session-reader never published. Run `ainb plugin watch burndown` AND `ainb plugin tail session-reader --level debug` in two terminals to see which one failed.
- **`schema_mismatch` banner on Analytics** — the publisher and subscriber disagree on `WIRE_VERSION`. Rebuild both plugins from the same checkout (`cargo build --workspace` + `just stage-plugins`).
- **Keystrokes feel sluggish during burndown refresh** — should be fixed by the priority-key channel landed 2026-05-15 (PR #125). If you see it on a build older than that, rebase onto `feat/plugin`.
- **Capability errors (`-32001`)** — the plugin asked for a host action its manifest doesn't grant. The host JSONL log entry includes which capability and which method.
