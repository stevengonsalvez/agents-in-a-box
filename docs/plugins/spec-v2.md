---
title: "ainb plugin contract — v2 (subprocess)"
---

**Status:** stable.
**Host versions covered:** `2.x.y` (additive minor bumps stay in v2).
**Successor:** tracked in [CHANGELOG.md](./CHANGELOG.md). A new contract version (`v3`) ships only when an existing signature changes incompatibly.

A plugin **conforms to v2** if it satisfies every MUST clause below. The `ainb-plugin-cts-v2` crate is the executable form of this document; a plugin author runs it via `cargo test` to get a per-axis pass/fail report.

## 0. Architecture

A v2 plugin is a **native executable** spawned by the host as a child process. Host and plugin exchange JSON-RPC 2.0 over framed stdio. The plugin's stdin is the request stream from the host; its stdout is the response stream + reverse-call requests; its stderr is captured as host log output.

```text
┌──────────────────────────────┐         ┌──────────────────────────────┐
│           ainb host          │         │       plugin process         │
│  (ainb-plugin-runtime)       │◀───────▶│  (ainb-plugin-sdk-rust)      │
│                              │  framed │                              │
│  - per-plugin tokio task     │   JSON  │  - stdio dispatcher          │
│  - request/response ledger   │   RPC   │  - Plugin trait impl         │
│  - subscriber fan-out        │  stdio  │  - HostClient for callbacks  │
└──────────────────────────────┘         └──────────────────────────────┘
```

Versus the deprecated v1 wasm contract: no wasm, no wasmi, no linker-omitted host-fn imports, no `wasm32-wasip1` target. A v2 plugin is a regular `cargo build`'d binary.

## 1. Manifest

A plugin's `manifest.toml` MUST parse as the `Manifest` struct exported by `ainb-plugin-protocol::manifest`. Schema:

```toml
[plugin]
name        = "burndown"     # required; must match the directory name
                             # under `dist/plugins/<name>/` and the binary
                             # filename inside it
version     = "2.0.0"        # required; semver release
abi_version = 2              # required; must equal the host's v2 ABI
description = "..."          # optional, free-form one-liner

[capabilities]               # every flag defaults to `false` / `[]`
read_sessions      = true    # access ~/.claude/agents-in-a-box/sessions/**
write_plugin_data  = true    # write to ~/.agents-in-a-box/plugins/<name>/
event_bus          = true    # subscribe/publish on the snapshot/event bus
network            = []      # bool or hostname allow-list
spawn_subprocess   = false   # spawn auxiliary children from inside plugin
read_claude_logs   = false   # read ~/.claude/projects/**/*.jsonl
read_codex_logs    = false   # read ~/.codex/sessions/**/*.jsonl

[provides]                   # purely informational; defaults are []
screens        = []          # screen IDs the host can route to
commands       = []          # command palette entries
cli_namespaces = []          # top-level `ainb <ns> ...` subcommands
snapshots      = []          # snapshot topics the plugin publishes

[subscribes]
snapshots = []               # topics the runtime auto-pushes via
                             # `plugin/handle_event` notifications

[lifecycle]
spawn          = "lazy"      # "lazy" (default) or "eager"
idle_reap_secs = 600         # 0 disables reaping
```

**MUST:**
- `[plugin].name`, `[plugin].version`, `[plugin].abi_version` are present
- `version` parses as semver; `abi_version` is an integer
- The manifest sits at `dist/plugins/<name>/manifest.toml` on disk

**Capability semantics.** `CapabilityGrant` accepts two forms: `true|false` (boolean grant) and `["host1", "host2"]` (allow-list grant; e.g. `network = ["api.openai.com"]`). The host evaluates `is_granted()` at request dispatch — denied capabilities return JSON-RPC error code `-32001` (CAPABILITY_DENIED).

**Subscriptions.** `[subscribes].snapshots` is the declarative subscription path: the runtime auto-pushes `plugin/handle_event` for every publish on listed topics, with no imperative subscribe call required at startup. Plugins may also call `host/snapshot/subscribe` at runtime for the same effect.

**Lifecycle:** `eager` plugins are spawned at host startup; `lazy` plugins spawn on first inbound request. Required for pure-publisher plugins (`session-reader` is `eager` so it can boot the chunked-publish stream before any subscriber calls into it).

## 2. Framing

The wire is LSP-style Content-Length framing.

```text
Content-Length: <decimal-bytes>\r\n
\r\n
<body bytes — exactly Content-Length>
```

Constraints (from `ainb-plugin-protocol::framing`):
- Header block ≤ 8 KiB
- Body ≤ 16 MiB (`MAX_BODY_BYTES`)
- Only `Content-Length` header is recognised; CRLF (`\r\n`) terminator is mandatory
- Decode returns "clean EOF" only when no header byte has been read yet

The body is a single JSON-RPC 2.0 envelope.

## 3. JSON-RPC dialect

Standard JSON-RPC 2.0 (`{"jsonrpc":"2.0", ...}`). Requests carry an `id`; notifications omit it. The plugin process is the server for `plugin/*` methods AND the client for `host/*` methods — it multiplexes both directions on its stdio.

**Error codes:**

| Code | Constant | Meaning |
|---|---|---|
| `-32601` | `METHOD_NOT_FOUND` | JSON-RPC 2.0 — unknown method |
| `-32602` | `INVALID_PARAMS` | JSON-RPC 2.0 — params couldn't be deserialised |
| `-32001` | `CAPABILITY_DENIED` | Plugin requested a host action its manifest doesn't grant |
| `-32002` | `ACTION_TIMEOUT` | `host/action/invoke` exceeded caller-supplied timeout |
| `-32003` | `MANIFEST_VALIDATION` | Manifest schema validation failed at init |

Wire shape:

```json
{
  "jsonrpc": "2.0",
  "id": 7,
  "error": { "code": -32001, "message": "capability denied: network" }
}
```

## 4. Methods

### 4.1 Host → plugin

| Method | Kind | Request | Response |
|---|---|---|---|
| `plugin/init` | Request | `PluginInitParams` | `PluginInitResult` |
| `plugin/shutdown` | Request | `PluginShutdownParams` | `PluginShutdownResult` |
| `plugin/render` | Request | `RenderParams` | `RenderResult` (contains `WireBuffer`) |
| `plugin/handle_event` | Notification | `HandleEventParams` | — |
| `plugin/handle_key` | Notification | `HandleKeyParams` | — |
| `plugin/cli_dispatch` | Request | `CliDispatchParams` | `CliDispatchResult` |

`plugin/init` MUST be the first method called; the plugin MUST reply with its decoded manifest + its declared ABI version. `plugin/shutdown` is the last method; the plugin SHOULD exit within the host's shutdown grace window (currently 5 s).

Lifecycle ordering for a typical screen-owning plugin:

```text
host:  plugin/init     ─▶
plugin:                ◀─ PluginInitResult { manifest, abi_version }
host:  plugin/render   ─▶                                          (per frame)
plugin:                ◀─ RenderResult { buffer }
host:  plugin/handle_key (notif)  ─▶                               (per keystroke)
host:  plugin/handle_event (notif) ─▶                              (per subscription delivery)
…
host:  plugin/shutdown ─▶
plugin:                ◀─ PluginShutdownResult
plugin: exit 0
```

**Notifications never block the dispatcher.** The plugin SDK runs `handle_event` and `handle_key` inline on its read loop so chunk/key ordering is preserved. The host runtime maintains a separate **priority key channel** so `handle_key` notifications preempt any queued `handle_event` chunks (added 2026-05-15 to fix Esc starvation during chunked publishes).

### 4.2 Plugin → host

| Method | Kind | Request | Response |
|---|---|---|---|
| `host/snapshot/get` | Request | `SnapshotGetParams` | `SnapshotGetResult` |
| `host/snapshot/publish` | Notification | `SnapshotPublishParams` | — |
| `host/snapshot/subscribe` | Request | `SnapshotSubscribeParams` | `SnapshotSubscribeResult` |
| `host/action/invoke` | Request (with timeout) | `ActionInvokeParams` | `ActionInvokeResult` |
| `host/log` | Notification | `LogParams` | — |
| `host/fs/read_dir` | Request | `FsReadDirParams` | `FsReadDirResult` |
| `host/fs/read_file` | Request | `FsReadFileParams` | `FsReadFileResult` |
| `host/network/fetch` | Request | `NetworkFetchParams` | `NetworkFetchResult` |

`host/network/fetch` is gated by `[capabilities].network` (boolean or host allow-list). `host/fs/*` is gated by `read_sessions` / `read_claude_logs` / `read_codex_logs` depending on the path root.

## 5. Wire types

### 5.1 `WireBuffer`

Returned by `plugin/render`. Sparse: only painted cells are listed.

```rust
pub struct WireBuffer {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<(Coord, Cell)>,
}

pub struct Coord  { pub x: u16, pub y: u16 }
pub struct Cell   {
    pub symbol: String,
    pub fg:       Option<Color>,
    pub bg:       Option<Color>,
    pub modifier: u16,         // ratatui-style style bitfield
}
pub struct Color  { pub r: u8, pub g: u8, pub b: u8 }
```

Coordinates outside `width × height` are silently clipped by the host.

### 5.2 `KeyEvent`

```rust
pub struct KeyEvent {
    pub code: KeyCode,
    pub mods: u8,   // KEY_MOD_SHIFT | KEY_MOD_CTRL | KEY_MOD_ALT | KEY_MOD_SUPER
    pub kind: KeyKind,
}

pub enum KeyCode {
    Char { ch: char },
    Enter, Tab, BackTab, Esc, Backspace, Delete,
    Up, Down, Left, Right, Home, End, PageUp, PageDown,
    F { n: u8 },
}
pub enum KeyKind { Press, Repeat, Release }   // default: Press
```

Wire tag is `type` with `snake_case` rename. Example payload for `Esc`:

```json
{ "code": {"type":"esc"}, "mods": 0, "kind": "press" }
```

### 5.3 Binary payloads

Every `bytes::Bytes` field on the wire is **base64-encoded**. The deserialiser accepts the legacy `[u8]`-array form for backward compatibility, but writers MUST emit base64.

### 5.4 Snapshot wire versions

Plugin types crates (e.g. `ainb-plugin-types-sessions`) carry their own `pub const WIRE_VERSION: u32`. Subscribers MUST check `event.version == WIRE_VERSION` and refuse mismatches with a graceful schema-mismatch UI rather than panicking. The current values:

| Crate | Constant | Value |
|---|---|---|
| `ainb-plugin-types-sessions` | `WIRE_VERSION` | `3` |
| Runtime manifest ABI | `ABI_VERSION` | `2` |

## 6. Chunked publishes

The snapshot store retains only the most recent publish per topic. For datasets larger than `~2 MiB` of msgpack, publishers MUST chunk to avoid blowing the frame cap or starving subscribers.

`UsageDataEvent` is the canonical example:

```rust
pub struct UsageDataEvent {
    pub version: u32,         // == WIRE_VERSION
    pub published_ns: u64,    // monotonic, identifies the publish batch
    pub partial: bool,        // false until the snapshot is complete
    pub chunk_index: u32,     // 0 .. N
    pub is_final: bool,       // true only on the last chunk
    pub data: UsageData,      // aggregates on chunk 0, calls-only on 1..N
}
```

Contract:
- chunk 0 carries full aggregates + the first slice of `calls`
- chunks 1..N carry only `calls`; aggregates are `UsageData::default()`
- only the final chunk has `is_final = true`
- a single-chunk publish has `chunk_index = 0, is_final = true`
- chunk size target is implementation-defined; current `session-reader` uses ~2 MiB msgpack so the encoded frame stays under ~2.7 MiB after base64 inflation

Subscribers MUST drop a follow-on chunk (index > 0) for which they never observed a chunk 0 — partial data without aggregates is worse than no data. A subsequent chunk 0 discards any in-flight accumulator.

## 7. Lifecycle states

The runtime's per-plugin task transitions:

| State | Entry | Exit |
|---|---|---|
| `Idle` | Registration | First request → `Spawning` |
| `Spawning` | `Command::spawn` issued | `plugin/init` reply → `Running` |
| `Running` | `plugin/init` ack | Crash → `Backoff`; idle reap → `ShuttingDown` |
| `Backoff` | Process died | Backoff timer expires → respawn → `Spawning`; or 3 crashes in window → `Quarantined` |
| `Quarantined` | Three failures inside the failure window | Explicit `Reload` only |
| `ShuttingDown` | Idle reap fired | Process exit → drop task |

## 8. Resource budgets

| Budget | Value | Source |
|---|---|---|
| Maximum frame body | 16 MiB | `framing::MAX_BODY_BYTES` |
| Maximum header block | 8 KiB | `framing` decoder |
| Idle reap default | 600 s | `Lifecycle::idle_reap_secs` default |
| Shutdown grace | 5 s | runtime `shutdown_grace` config |
| `host/action/invoke` timeout | caller-supplied | `ActionInvokeParams.timeout_ms` |

## 9. Capability gates

| Capability | Gates |
|---|---|
| `read_sessions` | `host/fs/read_dir` and `host/fs/read_file` under `~/.agents-in-a-box/sessions/**` |
| `read_claude_logs` | `host/fs/*` under `~/.claude/projects/**` |
| `read_codex_logs` | `host/fs/*` under `~/.codex/sessions/**` |
| `write_plugin_data` | Any write under `~/.agents-in-a-box/plugins/<name>/` |
| `event_bus` | All `host/snapshot/*` methods |
| `network` | `host/network/fetch` — boolean gate, optionally restricted to listed hostnames |
| `spawn_subprocess` | Any host action that exec's a child on the plugin's behalf |

Denied capability → `RpcError { code: -32001, message: "capability denied: <cap>" }`.

## 10. Conformance test suite

`ainb-plugin-cts-v2` covers 14 axes: manifest round-trip, framing, method dispatch, capability gating, render determinism, snapshot pub/sub, action timeout, log filtering, fs path guard, graceful shutdown, crash recovery, quarantine, CLI dispatch capture, chunked publish ordering. Each axis has a canary plugin at `crates/ainb-plugin-cts-v2/tests/canaries/<axis>/` and a host-side `#[test]` in `tests/axes.rs`.

Plugins MUST pass every axis their manifest's capability + provides surface implies. Axes for surface a plugin doesn't expose are skipped.

> **Author tooling.** Two crates back this: `ainb-plugin-cts-v2` is the subprocess conformance runner described above (canary plugins + host-side `tests/axes.rs`), and `ainb-plugin-testkit` is an in-process harness that drives a `Plugin` trait impl over a `tokio::io::DuplexStream` pair — exercising the full JSON-RPC + Content-Length framing path without spawning a subprocess or staging a binary on disk. Use `testkit` for fast unit-style assertions while authoring; use `cts-v2` for the authoritative per-axis pass/fail gate.

## 11. Stability promise

The host MUST NOT introduce a breaking change to any signature in this document inside the v2 series. Permitted additive changes that stay in v2:

- New methods (host or plugin)
- New optional manifest fields (with `#[serde(default)]`)
- New optional fields on existing wire types
- New error codes
- New capability flags (default-deny semantics)

Any rename, removal, signature change, or default-change is a v3 break. The v3 spec ships alongside `ainb-plugin-cts-v3`; the v2 runner stays supported until the host drops every plugin still pinned to it.
