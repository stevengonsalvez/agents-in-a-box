---
title: "Plugin user guide"
---

# Plugin user guide

User-facing reference for the `ainb plugin` family of commands. New to plugins? Read [overview.md](./overview.md) first. Writing one? [authoring.md](./authoring.md). Wire contract: [spec-v2.md](./spec-v2.md).

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

See [./spec-v2.md §1](./spec-v2.md#1-manifest) for full semantics.

## Configuration

### Enable/disable plugins

The host supports four knobs for selecting which plugins load, evaluated in this precedence order (most specific wins):

| Knob | Where | Behavior |
|---|---|---|
| `AINB_DISABLE_PLUGINS=1` | env | Kill switch — skip discovery entirely. Runtime comes up plugin-free. |
| `AINB_ONLY_PLUGINS=a,b` | env | Allowlist — load ONLY the named plugins; everything else skipped. |
| `AINB_DISABLE_PLUGIN=a,b` | env | Denylist — load everything EXCEPT the named plugins. Ignored when `AINB_ONLY_PLUGINS` is set. |
| `[plugins].enabled = [...]` | `config.toml` | Persistent allowlist. Same shape as `AINB_ONLY_PLUGINS` but survives across runs. Env vars override config. |
| `[plugins].disabled = [...]` | `config.toml` | Persistent denylist. Ignored when `[plugins].enabled` is non-empty. |

Examples:

```bash
# All-or-nothing kill switch (existing behavior).
AINB_DISABLE_PLUGINS=1 ainb tui

# Skip burndown but keep session-reader running.
AINB_DISABLE_PLUGIN=burndown ainb tui

# Load only session-reader (useful for a headless data pipeline).
AINB_ONLY_PLUGINS=session-reader ainb tui

# Multiple names — comma-separated, whitespace tolerated.
AINB_ONLY_PLUGINS="burndown, session-reader" ainb tui
```

Persistent config (`~/.agents-in-a-box/config/config.toml`):

```toml
[plugins]
# Either list (or neither) — when both are set, `enabled` wins.
enabled = ["session-reader"]
# disabled = ["burndown"]
```

When `AINB_DISABLE_PLUGINS=1`, the Analytics screen falls back to a "plugin: rendering…" placeholder. When only specific plugins are disabled via allow/deny, screens owned by disabled plugins show the same placeholder; other screens are unaffected.

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
