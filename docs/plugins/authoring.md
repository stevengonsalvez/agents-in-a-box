---
title: "ainb plugin authoring"
---

# ainb plugin authoring

Developer-facing reference for shipping a v2 plugin. For end-user install/CLI docs see [./user-guide.md](./user-guide.md). For the wire contract see [./spec-v2.md](./spec-v2.md).

## What you're building

A v2 ainb plugin is a **native executable** that talks JSON-RPC 2.0 to the host over framed stdio. The host treats your binary as a long-lived subprocess: it pipes requests in on stdin, reads responses + reverse-call requests off stdout, and captures stderr as host log output.

A plugin can:

- **Own a TUI screen** — implement `Plugin::render` and paint a `WireBuffer` per frame; the host blits it onto the terminal
- **Own a CLI subcommand tree** — claim a `cli_namespaces` entry in your manifest; the host dispatches `ainb <ns> ...` invocations through `Plugin::cli_dispatch`
- **Publish snapshots** — push data onto a topic via `HostClient::snapshot_publish`; subscribers (other plugins or the host) get `plugin/handle_event` deliveries
- **Subscribe to snapshots** — declare `[subscribes].snapshots` in your manifest; the runtime auto-pushes deliveries via `handle_event`
- **Persist its own state** — write under `~/.agents-in-a-box/plugins/<name>/` (gated by `write_plugin_data`)
- **Read host-managed state** — sessions, Claude/Codex logs (capability-gated)
- **Invoke host actions** — call out to host-owned operations via `host/action/invoke`

The bundled `ainb-plugin-burndown` is the canonical reference: owns the Analytics screen, the `ainb usage` CLI tree, and a statusline segment. `ainb-plugin-session-reader` is the canonical pure-publisher example. `ainb-plugin-witr` is the canonical **subprocess-wrapping** example: it declares `spawn_subprocess`, detects an external binary (`witr`) on `PATH`, gates its UI on a version check, and execs `witr --json <target>` to back its screen and `ainb witr` CLI — the template for surfacing any external tool as a plugin without vendoring its code.

## Scaffold

```bash
mkdir -p ainb-tui/crates/ainb-plugin-mything
cd ainb-tui/crates/ainb-plugin-mything
```

`Cargo.toml`:

```toml
[package]
name        = "ainb-plugin-mything"
version     = "0.1.0"
edition     = { workspace = true }
description = "What it does."

[lib]                                  # split lib + bin lets you unit-test
name = "ainb_plugin_mything"           # the plugin logic without
                                       # spawning a process.
path = "src/lib.rs"

[[bin]]
name = "ainb-plugin-mything"           # must match the manifest's plugin.name
path = "src/main.rs"                   # (host expects this filename in dist/)

[dependencies]
ainb-plugin-sdk-rust    = { path = "../ainb-plugin-sdk-rust" }
ainb-plugin-protocol    = { path = "../ainb-plugin-protocol" }
# Wire types crate(s) for any snapshot topics you publish or subscribe to:
ainb-plugin-types-sessions = { path = "../ainb-plugin-types-sessions" }

async-trait = "0.1"                             # required for #[async_trait] on Plugin

# Common workspace deps — pull what you need:
tokio       = { workspace = true, features = ["macros", "rt-multi-thread", "io-std"] }
serde       = { workspace = true, features = ["derive"] }
serde_json  = { workspace = true }
rmp-serde   = { workspace = true }              # only if publishing msgpack snapshots
anyhow      = { workspace = true }
ratatui     = { workspace = true, default-features = false }
```

Crate type is a normal Rust `lib` + `bin`. **Not** `cdylib` — that was v1.

The plugin crate sits inside the parent workspace; no separate `[workspace.exclude]` ceremony is needed.

## Manifest (`manifest.toml`)

Sits at `crates/ainb-plugin-mything/manifest.toml`. Built copies are staged to `dist/plugins/<name>/manifest.toml` alongside the binary. The host reads it at discovery and again at `plugin/init` for validation.

```toml
[plugin]
name        = "mything"
version     = "0.1.0"
abi_version = 2
description = "Does the thing."

[capabilities]
write_plugin_data = true
event_bus         = true

[provides]
screens        = ["mything"]
cli_namespaces = ["mything"]

[subscribes]
snapshots = []

[lifecycle]
spawn          = "lazy"
idle_reap_secs = 600
```

See [v2.md §1](./spec-v2.md#1-manifest) for the full schema and capability semantics.

## Implementing the `Plugin` trait

The crate is named `ainb-plugin-sdk-rust` for `Cargo.toml`, but its `[lib].name` is `ainb_plugin_sdk` — that's the path you `use` from Rust code. The trait surface uses ergonomic types (`WireBuffer`, `CliOutput`); the SDK marshals to and from the wire types documented in [`v2.md`](./spec-v2.md) on your behalf.

`src/lib.rs`:

```rust
use ainb_plugin_protocol::params::{HandleEventParams, HandleKeyParams, RenderParams};
use ainb_plugin_protocol::wire_buffer::WireBuffer;
use ainb_plugin_sdk::{CliOutput, HostClient, Plugin, Result};
use async_trait::async_trait;

pub struct MyThing {
    // Your state.
}

#[async_trait]
impl Plugin for MyThing {
    fn manifest(&self) -> &'static str {
        include_str!("../manifest.toml")
    }

    async fn render(&mut self, _host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        let mut buf = WireBuffer::new(params.viewport.width, params.viewport.height);
        // paint cells…
        Ok(buf)
    }

    async fn on_init(
        &mut self,
        host: &HostClient,
        _granted_capabilities: &[String],
    ) -> Result<()> {
        host.log_info("mything: initialised").await?;
        Ok(())
    }

    async fn handle_key(&mut self, _host: &HostClient, _params: HandleKeyParams) -> Result<()> {
        // mutate self.state per keystroke…
        Ok(())
    }

    async fn handle_event(&mut self, _host: &HostClient, _params: HandleEventParams) -> Result<()> {
        // process subscription delivery on params.topic with params.payload…
        Ok(())
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        _namespace: &str,
        _argv: &[String],
    ) -> Result<CliOutput> {
        Ok(CliOutput {
            stdout: b"ok\n".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        })
    }
}
```

`src/main.rs`:

```rust
use ainb_plugin_sdk::Server;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let plugin = ainb_plugin_mything::MyThing { /* … */ };
    Server::new(plugin).run_stdio().await?;
    Ok(())
}
```

The SDK handles Content-Length framing, JSON-RPC envelope encode/decode, method dispatch, error mapping, and the `HostClient` reverse-call channel. Plugins focus on state + behaviour.

### Method method-by-method

| Method | Required? | When fired |
|---|---|---|
| `manifest()` | required (no default) | At construction; SDK reads it once for `plugin/init` |
| `render` | required (no default) | Host requests a paint of the screen at the given viewport size |
| `on_init` | optional (default no-op) | Right after `plugin/init` — open files, kick subscriptions, publish bootstrap. `granted_capabilities` lets you refuse to start with `SdkError::plugin(...)` if a required cap was withheld |
| `handle_key` | optional (default no-op) | A keystroke landed on a screen this plugin owns; preempts `handle_event` |
| `handle_event` | optional (default no-op) | A subscribed snapshot was published — `params.topic` + `params.payload` |
| `cli_dispatch` | optional (default `exit 2`) | `ainb <namespace> ...` was invoked; namespace + argv are passed by reference |
| `on_shutdown` | optional (default no-op) | Host is reaping the plugin; flush state and return |

Pure-publisher plugins (no screen) can satisfy `render` with `Ok(WireBuffer::new(0, 0))` since the host never requests a paint for a plugin that doesn't declare any screens.

Both `handle_key` and `handle_event` are **notifications** — the plugin dispatcher serialises them inline on the read loop to preserve chunk ordering and multi-key sequence semantics.

## `HostClient` — calling back into the host

```rust
host.log_info("…").await;                                  // notification
host.log(LogLevel::Warn, "…", json!({"k":"v"})).await;     // structured
host.snapshot_publish("topic", payload_bytes).await;       // notification
let snap = host.snapshot_get("topic").await?;              // request
host.snapshot_subscribe("topic").await?;                   // request
let bytes = host.action_invoke("action", payload, timeout).await?;
```

All `HostClient` methods are async. `log_info` / `log` / `snapshot_publish` are fire-and-forget notifications; the others await replies from the host.

## Chunked publishing (large snapshots)

When your snapshot is bigger than ~2 MiB of msgpack, you MUST chunk. Pattern (see `crates/ainb-plugin-session-reader/src/plugin.rs::chunk_usage_data` for the reference):

```rust
let chunks = chunk_my_snapshot(data, target_bytes);
for ev in chunks {
    let is_final = ev.is_final;
    host.snapshot_publish("my.topic", rmp_serde::to_vec_named(&ev)?).await;
    if is_final { break; }
}
```

Chunk 0 carries full aggregates + the first slice of the bulk array; chunks 1..N carry only the bulk slice. Subscribers walk the sequence, drop follow-on chunks that arrived without a chunk 0, and finalise on `is_final = true`.

Bump the wire types crate's `WIRE_VERSION` whenever you change the chunk shape. Subscribers MUST check the version and latch a schema-mismatch UI on drift.

## Build + stage loop

```bash
# From the workspace root (ainb-tui/).
cargo build -p ainb-plugin-mything

# Stage every plugin into dist/plugins/<id>/ — this is what the dev TUI
# loads from. On macOS, also re-signs the binary (Cargo's link-time
# signature is path-bound and gets invalidated by the copy step; without
# re-signing, AMFI SIGKILLs at exec with no stderr).
just stage-plugins

# Run.
cargo run --bin ainb -- tui
```

Add an entry for your plugin to `scripts/build-plugins.sh` if it doesn't already enumerate every plugin crate (the current script iterates a hard-coded list — extend it).

Staged layout:

```text
dist/plugins/mything/
├── mything         (your executable, ad-hoc signed on macOS)
└── manifest.toml   (copied from crates/ainb-plugin-mything/manifest.toml)
```

The host discovers plugins by walking `dist/plugins/<id>/manifest.toml` at startup.

## Debugging

### Logs

```bash
# Host JSONL log — includes plugin host.log calls AND plugin stderr drain.
tail -f ~/.agents-in-a-box/logs/agents-in-a-box-*.jsonl | jq .

# Enable trace-level for a specific plugin crate:
RUST_LOG=ainb_plugin_runtime=debug,ainb_plugin_mything=trace cargo run --bin ainb -- tui
```

### Standalone probe

If `dist/plugins/mything/mything` exits 137 in <1 ms on macOS, you hit AMFI silent-kill. Fix: `just stage-plugins` (re-runs the codesign).

```bash
./dist/plugins/mything/mything </dev/null; echo "exit $?"
```

Expected: prints something on stderr ("missing stdin frames", "no init received") within a few seconds, then exits. Exit 137 immediately = AMFI; exit 0 = the SDK quit cleanly on EOF; any other code = your plugin panicked.

### Inspect wire traffic

The runtime logs every inbound/outbound frame at `debug`. Set `RUST_LOG=ainb_plugin_runtime=debug` and grep the JSONL for `host->plugin request` / `plugin->host response` / `inbound notification`.

## Testing

### Unit tests

Plain Rust tests against your `lib`:

```rust
#[tokio::test]
async fn render_paints_header_cell() {
    let mut p = MyThing::default();
    let params = RenderParams { viewport: Viewport::new(20, 5), generation: 0 };
    let host = HostClient::for_tests();          // SDK-provided shim
    let r = p.render(&host, params).await.unwrap();
    assert_eq!(r.buffer.cells[0].1.symbol, "M");
}
```

### Conformance — `ainb-plugin-cts-v2`

The CTS runs your built binary through 14 host-impersonation axes. Tests sit in `crates/ainb-plugin-cts-v2/tests/axes.rs`; each axis spawns the plugin under a synthetic host driver and asserts wire-level behaviour. Add a per-axis canary plugin under `crates/ainb-plugin-cts-v2/tests/canaries/<axis>/main.rs` if your plugin tests a path no canary covers yet.

### Tripwires — end-to-end TUI tests

Tripwire tests in `crates/ainb-core/tests/tripwire_*.rs` drive the real `ainb tui` binary in detached tmux, send keystrokes, capture the pane, and assert on rendered output. They're the only tests that catch "plugin compiled but doesn't render". See `.claude/skills/tmux-ui-tripwire/SKILL.md` for the pattern.

Minimum coverage: one tripwire that proves your plugin's screen renders something specific after a keystroke. See `tripwire_burndown_keys.rs` for a copy-paste template.

## Distribution

Currently in-tree only. Marketplace install (`ainb plugin install <name>`) targets the v1 catalog schema and will be refreshed for v2 — TBD.

## Pitfalls

- **Don't forget to stage.** `cargo build -p ainb-plugin-mything` writes to `target/debug/` but the host loads from `dist/plugins/mything/mything`. Run `just stage-plugins` after every rebuild.
- **macOS AMFI.** Any copied/moved binary needs re-signing or it gets SIGKILL'd silently at exec. `just stage-plugins` handles it; don't skip.
- **Wire-version drift.** When you change the on-wire shape of a snapshot event, bump `WIRE_VERSION` in the types crate. Subscribers will latch `schema_mismatch` until they rebuild against the new constant.
- **FIFO ordering.** The plugin SDK dispatches `plugin/handle_event` notifications inline on its read loop precisely so that chunked publishes apply in order. Don't try to spawn background tasks to process events — you'll re-order them.
- **Capability default is deny.** Forget to add `network = true` to your manifest and `host.action_invoke("fetch", ...)` returns `RpcError { code: -32001 }`. Always grep the host log for `capability denied` first when debugging a stuck call.
- **Idle reap.** `lifecycle.idle_reap_secs = 600` means the host will SIGTERM your plugin after 10 minutes of no requests. Set `0` to disable, or implement a periodic self-publish if the plugin needs to stay warm.
