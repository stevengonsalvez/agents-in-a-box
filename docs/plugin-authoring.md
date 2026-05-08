# ainb plugin authoring

Developer-facing reference for shipping a plugin. For end-user docs see
[docs/plugins.md](./plugins.md).

## What you're building

An ainb plugin is a `cdylib` compiled to `wasm32-wasip1` that exports
four `extern "C"` entrypoints (`_init`, `_render`, `_handle_event`,
`_shutdown`) plus a host-allocator (`_alloc`). The host loads it via
[wasmi](https://crates.io/crates/wasmi) inside a sandboxed runtime
that hands the plugin only the capabilities its `plugin.toml`
declares.

A plugin can:

* Own a TUI screen — paint a `WireBuffer` per frame via
  `ainb_render_buffer`. The host paints that buffer onto the terminal.
* Own a CLI subcommand tree — receive `PluginEvent::Command { name,
  args }` from the host's CLI dispatcher; print to stdout/stderr the
  same way as a normal binary.
* Subscribe to and publish on the host event bus.
* Read host-managed state (sessions, claude/codex logs) within
  capability boundaries.
* Persist its own state under `~/.agents-in-a-box/plugins/data/<id>/`.

The bundled `ainb-plugin-burndown` crate is the reference
implementation: it owns the Analytics screen, the `ainb usage` CLI
tree, and a statusline segment.

## Scaffold

```bash
mkdir -p crates/ainb-plugin-mything
cd crates/ainb-plugin-mything
cargo init --lib
```

`Cargo.toml`:

```toml
[package]
name = "ainb-plugin-mything"
version = "0.1.0"
edition = "2021"

# Plugin crates live in [workspace.exclude] of the main workspace
# because they target wasm32-wasip1 and would otherwise break host
# `cargo build`. Declare your own [workspace] root:
[workspace]

[lib]
crate-type = ["cdylib"]

[dependencies]
ainb-plugin-api = { path = "../ainb-plugin-api", version = "0.1.0" }
rmp-serde = "1"
serde     = { version = "1", features = ["derive"] }
serde_json = "1"
ratatui   = "0.26"
```

Add the crate path to the parent workspace's `[workspace.exclude]`:

```toml
# ainb-tui/Cargo.toml
[workspace]
exclude = [
    # …
    "crates/ainb-plugin-mything",
]
```

## Manifest (`plugin.toml`)

Sits next to your `plugin.wasm` in the install cache. The host validates
it against the `[plugin].ainb_min_version` semver requirement before
instantiation.

```toml
[plugin]
name             = "mything"        # must match `[package].name` minus the `ainb-plugin-` prefix
version          = "0.1.0"          # semver — copy from Cargo.toml
ainb_min_version = "1.1.0"          # minimum host ABI version
description      = "what mything does"
author           = "you"
license          = "MIT"
homepage         = "https://example.com/mything"

[capabilities]
read_sessions     = false
read_claude_logs  = false
read_codex_logs   = false
write_plugin_data = true   # data/<id>/ writable storage
event_bus         = false  # publish/subscribe across plugins
spawn_subprocess  = false  # exec child processes
network           = []     # explicit allowlist of host:port
filesystem        = []     # explicit allowlist of glob patterns

[provides]
screens    = ["mything"]
commands   = ["/mything"]
sidebar    = []
statusline = []
providers  = []
```

The capability table is **enforced**. The host gates host-fn calls by
the plugin's declared caps; calling `ainb_data_write` without
`write_plugin_data = true` traps the wasmi instance and the host marks
the plugin degraded.

## ABI surface (host_version 1.1.0)

Five exports the host calls into:

| Export | Signature | When called | Returns |
|---|---|---|---|
| `_init` | `() -> i32` | Once at load. Allocate state, set `STATE`/`READY`. | `0` ok, ≠0 degrade. |
| `_render` | `() -> i32` | Once per frame for screen-owning plugins. Paint a `WireBuffer` and hand it to `ainb_render_buffer`. | `0` ok. |
| `_handle_event` | `(ptr, len) -> i32` | Whenever the host dispatches a `PluginEvent` (msgpack-encoded). | `0` ok. |
| `_shutdown` | `() -> ()` | On host shutdown. Best-effort cleanup. | — |
| `_alloc` | `(size) -> i32` | Host-side allocator: returns a guest-memory pointer the host can write event payloads into before calling `_handle_event`. | guest ptr |

All exports run single-threaded inside wasmi. Use a static-mut+ready-flag
pattern (or `OnceLock` on later Rust toolchains) for plugin-global state.

`PluginEvent` shape is a serde-tagged enum living in `ainb-plugin-api`:

```rust
pub enum PluginEvent {
    Custom { topic: String, payload: serde_json::Value },
    Command { name: String, args: Vec<String> },
    // …
}
```

Decode with `rmp_serde::from_slice` from the `(ptr, len)` slice.

## Host functions (host_version 1.1.0)

Functions the plugin can call into. All live in the `ainb` wasm module
unless noted; the WASI preview1 stubs live in `wasi_snapshot_preview1`.

| Module | Function | Capability gate | Purpose |
|---|---|---|---|
| `ainb` | `ainb_log(level, ptr, len)` | none | Forward a log line to the host's tracing subscriber. |
| `ainb` | `ainb_now_ms() -> u64` | none | Wall-clock millis since UNIX epoch. |
| `ainb` | `ainb_render_buffer(target, ptr, len)` | none | Hand the host a serialised `WireBuffer` to paint. `target=0` = screen. |
| `ainb` | `ainb_fs_read(ptr, len)` | `filesystem` | Read a file under one of the declared glob patterns. |
| `ainb` | `ainb_fs_glob(ptr, len)` | `filesystem` | Glob pattern → list of matching paths. |
| `ainb` | `ainb_data_read(key_ptr, key_len)` | `write_plugin_data` | Read from `data/<id>/`. |
| `ainb` | `ainb_data_write(key_ptr, key_len, val_ptr, val_len)` | `write_plugin_data` | Write to `data/<id>/`. |
| `ainb` | `ainb_event_publish(topic_ptr, …)` | `event_bus` | Publish on the host event bus. |
| `ainb` | `ainb_event_subscribe(topic_ptr, …)` | `event_bus` | Subscribe to a topic. |
| `ainb` | `ainb_session_get(ptr, len)` | `read_sessions` | Read one session by id. |
| `ainb` | `ainb_sessions_list()` | `read_sessions` | List all sessions. |
| `ainb` | `ainb_http_request(req_ptr, req_len)` | `network` | HTTPS request against the declared host:port allowlist. |

All `(ptr, len)` arguments point into guest memory; the host copies in/out
across the boundary. Unsuccessful calls return non-zero error codes and
populate `last_error` on the host's `HostState` (visible in tracing).

The WASI preview1 surface (`fd_write`, `fd_close`, `clock_time_get`,
`environ_get`, `proc_exit`, etc.) is stubbed for compatibility — Rust's
`std` panic handler links them, but the host doesn't actually read or
write your stdio through them. Use `ainb_log` for diagnostics instead.

## Wire types

* `WireBuffer { width, height, cells: Vec<WireCell> }` — your render
  output. Cells encode a printable symbol + 8-bit ANSI fg/bg + a
  modifier byte.
* `Manifest`, `CapabilitiesTable`, `ProvidesTable` — `plugin.toml` shape;
  re-exported from `ainb-plugin-api`.
* `RenderTarget` — `Screen = 0`, `Sidebar = 1`, `Statusline = 2`.

## Build + test loop

```bash
# From your plugin crate dir.
cargo build --target wasm32-wasip1 --release

# Stage into the host's dist/plugins/<id>/ layout the dev TUI loads from.
mkdir -p ../../dist/plugins/mything
cp target/wasm32-wasip1/release/ainb_plugin_mything.wasm \
   ../../dist/plugins/mything/plugin.wasm
cp plugin.toml ../../dist/plugins/mything/plugin.toml

# Drive the dev TUI from the host workspace root.
cd ../../
AINB_PLUGIN_ROOT=$PWD/dist cargo run --bin ainb -- tui
```

For unit tests, link `ainb-plugin-api` for the host target — the type
definitions compile on both wasm and host, so you can write Rust tests
that exercise your render and event-handling logic without spinning up
wasmi.

For end-to-end checks the in-tree pattern is to drive the plugin
through `PluginHost::dispatch_event_bytes` + `PluginHost::render_plugin`
+ `PluginHost::take_render` and assert on the resulting `WireBuffer`
cell-by-cell. See `crates/ainb-core/tests/tripwire.rs` and
`crates/ainb-core/tests/snapshot_baselines.rs` for working examples.

## Marketplace schema

A marketplace is a JSON catalog at one of:

* `<repo>/.ainb-plugin/marketplace.json` (preferred)
* `<repo>/.claude-plugin/marketplace.json` (Claude Code compat)

```json
{
  "name": "ainb-plugins",
  "plugins": [
    {
      "name": "mything",
      "version": "0.1.0",
      "repo": "https://github.com/you/your-repo",
      "git_ref": "v0.1.0",
      "manifest_path": "crates/ainb-plugin-mything/plugin.toml",
      "ainb_min_version": "1.1.0",
      "release_url": "https://github.com/{owner}/{repo}/releases/download/{tag}/{plugin}.wasm"
    }
  ]
}
```

Required fields: `name`, `version`, `repo`, `git_ref`, `manifest_path`,
`ainb_min_version`. Anything else is rejected by the schema validator.

`release_url` is optional; when omitted the host falls back to GitHub
Releases at the default template
`https://github.com/{owner}/{repo}/releases/download/{tag}/{plugin}.wasm`.

Substitutions:

* `{owner}` / `{repo}` — derived from `repo` (last two URL segments,
  `.git` suffix stripped).
* `{tag}` — `git_ref` verbatim.
* `{plugin}` — entry's `name`.

For local development the installer also accepts `file://` URLs and
bare paths, which is what the integration tests
(`crates/ainb-core/tests/plugin_install_flow.rs`) use.

## Distribution

1. Tag a release (`git tag v0.1.0 && git push --tags`).
2. Attach the compiled `plugin.wasm` to the GitHub Release.
3. Update your marketplace's `marketplace.json` with the new version.
4. Users run `ainb plugin update <name>` — they re-approve any new
   capabilities you've added.

For the in-repo first-party catalog (`toolkit/.ainb-plugin/marketplace.json`)
the equivalent flow is to bump `version` + `git_ref` in the same PR.

## Versioning

* Plugin `version` is plugin-specific semver — owned by you.
* `ainb_min_version` is the host ABI floor — bump only when you start
  using a host-fn that the older host didn't ship. The current host ABI
  is `1.1.0`.
* The host's `host_version` is the contract version of the ABI surface
  table (above). When the host adds a new host-fn, it bumps minor; when
  it changes a signature, it bumps major and refuses to load plugins
  declaring an `ainb_min_version` whose major doesn't match.

## Pitfalls

* **Static-mut + parallel tests** — wasmi runs your plugin
  single-threaded but a host test that loads your plugin twice in the
  same process will share `STATE`. Tests shouldn't `Drop` and re-load
  in the same binary unless you re-init.
* **`std::env` from the plugin** — wasmi 0.40 ships no real WASI;
  `std::env::var` traps. Use `ainb_data_read`/`ainb_data_write` or
  `ainb_fs_read` instead.
* **Big plugins** — opt-level=z + LTO + strip in `[profile.release]`
  keeps the wasm small. `crates/ainb-plugin-burndown` ships at ~1MB.
* **Cargo lock churn** — plugin crates ship as separate sub-workspaces;
  changing their dependency tree won't bump the host's `Cargo.lock`.
* **Capability hygiene** — declare the minimum needed. The
  `update`-time re-prompt fires whenever you add one, which means a
  needlessly broad install asks all your existing users to re-approve.
