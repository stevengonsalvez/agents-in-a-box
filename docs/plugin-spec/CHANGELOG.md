# Plugin contract changelog

Tracks how the plugin contract (`docs/plugin-spec/v2.md`) evolves.

## Versioning policy

- **Adding** a manifest field, JSON-RPC method, capability flag, or wire-type field with `#[serde(default)]` stays inside the current contract version. Existing plugins keep passing `ainb-plugin-cts-v2`.
- **Renaming**, **removing**, or **changing the signature** of any method or field is a breaking change. The contract bumps (`v2 → v3`); `ainb-plugin-cts-v<n+1>` ships alongside, and the previous CTS stays supported until the host drops every plugin still pinned to the older contract.
- The runtime's `ABI_VERSION` integer in `crates/ainb-plugin-runtime/src/plugin_task.rs` is the wire-level handshake version stamped on `plugin/init`. Bump only when the contract version bumps.
- Snapshot wire types (e.g. `ainb-plugin-types-sessions`) carry their own `WIRE_VERSION` constants — those evolve independently from the contract version.

## v2 — 2026-05-14

Subprocess plugin contract. Plugins are native executables exchanging JSON-RPC 2.0 over LSP-style Content-Length framed stdio. Replaces the v1 wasm contract entirely.

Surface area (full reference in [v2.md](./v2.md)):

- **Six host → plugin methods**: `plugin/init`, `plugin/shutdown`, `plugin/render`, `plugin/handle_event`, `plugin/handle_key`, `plugin/cli_dispatch`.
- **Eight plugin → host methods**: `host/snapshot/get`, `host/snapshot/publish`, `host/snapshot/subscribe`, `host/action/invoke`, `host/log`, `host/fs/read_dir`, `host/fs/read_file`, `host/network/fetch`.
- **Manifest schema**: `[plugin]`, `[capabilities]`, `[provides]`, `[subscribes]`, `[lifecycle]`. ABI version `2`.
- **Seven capability flags**: `read_sessions`, `write_plugin_data`, `event_bus`, `network` (bool or allow-list), `spawn_subprocess`, `read_claude_logs`, `read_codex_logs`. Default-deny via `#[serde(default)]`.
- **Lifecycle**: `eager` vs `lazy` spawn, idle-reap, three-strike quarantine.
- **Resource budgets**: 16 MiB frame body, 8 KiB header block, 5 s shutdown grace.
- **Chunked publishes**: `WIRE_VERSION` + `chunk_index` + `is_final` ordering contract; subscribers MUST drop follow-on chunks that arrived without chunk 0.
- **Priority key channel** (2026-05-15): host runtime drains `plugin/handle_key` notifications ahead of `plugin/handle_event` to prevent FIFO starvation during chunked publishes.

`ainb-plugin-cts-v2` ships with 14 conformance axes. In-tree dogfood plugins: `burndown` (analytics screen + `ainb usage` CLI) and `session-reader` (chunked usage_data publisher).

## v1 — historical, removed 2026-05-15

Earlier contract using `wasm32-wasip1` cdylibs + a wasmi host runtime + linker-omitted host-fn imports for capability gating. Replaced by v2 (subprocess) because the wasm sandbox added implementation cost without buying any safety property the OS process boundary doesn't already provide for ainb's threat model. The v1 spec lived at `docs/plugin-spec/v1.md`; no in-tree plugins targeted it long enough to require a deprecation path.
