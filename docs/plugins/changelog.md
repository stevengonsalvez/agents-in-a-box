---
title: "Plugin contract changelog"
---

Tracks how the plugin contract (`./spec-v2.md`) evolves.

## Versioning policy

- **Adding** a manifest field, JSON-RPC method, capability flag, or wire-type field with `#[serde(default)]` stays inside the current contract version. Existing plugins keep passing `ainb-plugin-cts-v2`.
- **Renaming**, **removing**, or **changing the signature** of any method or field is a breaking change. The contract bumps (`v2 → v3`); `ainb-plugin-cts-v<n+1>` ships alongside, and the previous CTS stays supported until the host drops every plugin still pinned to the older contract.
- The runtime's `ABI_VERSION` integer in `crates/ainb-plugin-runtime/src/plugin_task.rs` is the wire-level handshake version stamped on `plugin/init`. Bump only when the contract version bumps.
- Snapshot wire types (e.g. `ainb-plugin-types-sessions`) carry their own `WIRE_VERSION` constants: those evolve independently from the contract version.

## v2 additions, protocol crate 0.2.0: 2026-09-14

Additive to the v2 contract (existing plugins keep passing CTS). The Rust crate goes to 0.2.0 because two public struct shapes changed.

- **`[subscribes].latest_state`** (#1040): marks subscribed topics that carry the latest state. Only a subscription outside it keeps a plugin from idle reap. Must be a subset of `snapshots`; `[subscribes]` now refuses unknown keys.
- **`PluginInitParams.host`** (#1040): `{ kind, pid }` of the surface hosting the plugin, so a plugin that talks to the hangar daemon can name its host.
- **`event_bus` list form** (#1038): the allow-list covers exactly the topics it names (`*` suffix for a prefix). The blanket `true` covers every topic except `fleet.` ones, and so does every wildcard entry, `*` and `fleet.*` included (#1101): a `fleet.` topic is reached only by its exact name. The `event_stream_subscribe` allow-list uses the same matcher.
- **Idle reap**: a plugin whose screen is on display is not reaped; a reap answers requests still in flight with a runtime error.

## v2: 2026-05-14

Subprocess plugin contract. Plugins are native executables exchanging JSON-RPC 2.0 over LSP-style Content-Length framed stdio. Replaces the v1 wasm contract entirely.

Surface area (full reference in [v2.md](/plugins/spec-v2)):

- **Six host → plugin methods**: `plugin/init`, `plugin/shutdown`, `plugin/render`, `plugin/handle_event`, `plugin/handle_key`, `plugin/cli_dispatch`.
- **Eight plugin → host methods**: `host/snapshot/get`, `host/snapshot/publish`, `host/snapshot/subscribe`, `host/action/invoke`, `host/log`, `host/fs/read_dir`, `host/fs/read_file`, `host/network/fetch`.
- **Manifest schema**: `[plugin]`, `[capabilities]`, `[provides]`, `[subscribes]`, `[lifecycle]`. ABI version `2`.
- **Seven capability flags**: `read_sessions`, `write_plugin_data`, `event_bus`, `network` (bool or allow-list), `spawn_subprocess`, `read_claude_logs`, `read_codex_logs`. Default-deny via `#[serde(default)]`.
- **Lifecycle**: `eager` vs `lazy` spawn, idle-reap, three-strike quarantine.
- **Resource budgets**: 16 MiB frame body, 8 KiB header block, 5 s shutdown grace.
- **Chunked publishes**: `WIRE_VERSION` + `chunk_index` + `is_final` ordering contract; subscribers MUST drop follow-on chunks that arrived without chunk 0.
- **Priority key channel** (2026-05-15): host runtime drains `plugin/handle_key` notifications ahead of `plugin/handle_event` to prevent FIFO starvation during chunked publishes.

`ainb-plugin-cts-v2` ships with 21 conformance axes. In-tree dogfood plugins: `burndown` (analytics screen + `ainb usage` CLI), `session-reader` (chunked usage_data publisher), and `witr` (`ainb witr` CLI + `/witr` slash wrapping the external `witr` binary via `spawn_subprocess`; its process-causality screen is a host-embedded `witr -i` TTY). See [Plugins → In-tree plugins](/plugins/overview#reference-plugins) for per-plugin pages.

## v1: historical, removed 2026-05-15

Earlier contract using `wasm32-wasip1` cdylibs + a wasmi host runtime + linker-omitted host-fn imports for capability gating. Replaced by v2 (subprocess) because the wasm sandbox added implementation cost without buying any safety property the OS process boundary doesn't already provide for ainb's threat model. The v1 spec lived at `./spec-v1.md`; no in-tree plugins targeted it long enough to require a deprecation path.
