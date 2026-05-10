# Plugin Phase 7 — Runtime Redesign

## Why

Phase 0–6 shipped a wasmi-based plugin host. The architecture has eight load-bearing
sins (see `research/2026-05-10_distinguished-engineer-review.md`). The fatal ones:

- **Single-threaded host on the TUI render thread.** Every plugin call (event delivery,
  render, _tick) runs synchronously on the same thread that draws frames. A 200ms
  `_handle_event` blocks the next frame. Stevie's stated principle — *"nonblocking is
  our primary goal in the TUI"* — is structurally violated.
- **`ainb_request_data` documents its own deadlock** (`request.rs:108`). A "primary"
  API that doesn't work for the primary use case. The CLI bypasses it via
  `inject_session_reader_snapshot` (`registry.rs:478`) — the broker hack is the
  design's own admission its bus is broken.
- **Tripwire byte-equality on JSON CLI output proves nothing about the TUI.** The CLI
  uses the broker shim; the TUI uses the bus; they're different code paths. Green CI
  + broken TUI = the most dangerous failure mode.
- **Load-order race + silent error swallowing** (`lib.rs:295` discards `Result`) =
  invisible delivery failures.

This plan is the proper redesign. **Hard cutover, no half measures.** Locked decisions:

- **Subprocess + JSON-RPC 2.0 over stdio** (LSP-style). Each plugin = its own process.
- **Typed cell buffer** for render (same `WireBuffer` shape, transported via JSON-RPC
  instead of wasmi linker).
- **Snapshot store + channels.** Snapshot for read-mostly state (UsageData), channels
  for actions (rescan, invalidate, request).
- **ABI 2.0 — clean break.** Delete wasmi host entirely. Re-ship burndown +
  session-reader as Rust subprocess plugins.
- **Rust-only SDK** at v2.0; polyglot SDKs deferred. **Lazy-spawn on first use.**
  Auto-respawn with exponential backoff; quarantine after 3 fails.
- **DevX tooling ships in Phase 7**: `ainb-plugin-testkit`, `ainb plugin watch`,
  `ainb plugin tail`, `ainb plugin lint`.

## What's NOT in scope

- No polyglot SDKs (TS, Python, Go) — separate epic post-7.
- No hot-reload of the host itself (only plugins).
- No multi-tenant plugin sandboxing (cgroups, namespaces).
- No GUI-mode plugins (web frontends).

## Architecture overview

```
┌─────────────────────────────────────────────────────────────────────┐
│  ainb (host process — TUI render thread)                            │
│                                                                     │
│   render loop (60 fps)                                              │
│     │                                                               │
│     ▼                                                               │
│   try_recv(plugin_renders[plugin_id])  ──── never blocks ────       │
│     │                                                               │
│     └─ paint last good buffer or "loading…"                         │
│                                                                     │
└────────────────────────┬────────────────────────────────────────────┘
                         │  bounded mpsc channels (Send + 'static)
                         │  ┌───────────────────────────────────────┐
                         │  │ Cmd::Render(plugin_id, viewport)      │
                         │  │ Cmd::Event(plugin_id, topic, bytes)   │
                         │  │ Cmd::Action(plugin_id, action, bytes) │
                         │  └───────────────────────────────────────┘
                         │  ┌───────────────────────────────────────┐
                         │  │ Out::Rendered(plugin_id, WireBuffer)  │
                         │  │ Out::Snapshot(topic, bytes)           │
                         │  │ Out::Failed(plugin_id, reason)        │
                         │  └───────────────────────────────────────┘
                         ▼
┌─────────────────────────────────────────────────────────────────────┐
│  ainb-plugin-runtime (host-side, runs on dedicated tokio runtime)   │
│                                                                     │
│   per-plugin tokio task — owns subprocess Child + stdio JSON-RPC    │
│   ┌───────────────┐    ┌───────────────┐                            │
│   │  burndown     │    │ session-reader│                            │
│   │  process      │    │   process     │                            │
│   │               │    │               │                            │
│   │  (Rust bin)   │    │  (Rust bin)   │                            │
│   │  stdio JSONRPC│    │  stdio JSONRPC│                            │
│   └───────────────┘    └───────────────┘                            │
│                                                                     │
│   snapshot store: RwLock<HashMap<Topic, (Bytes, Version)>>          │
│   channel registry: HashMap<Action, Vec<plugin_id>>                 │
│   request ledger:   HashMap<corr_id, oneshot::Sender<Bytes>>        │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Wire protocol — JSON-RPC 2.0 over Content-Length-framed stdio

Method namespace:

```
plugin/init                    → host sends manifest path + capabilities granted
plugin/shutdown                → host requests graceful shutdown
plugin/render                  → request: viewport (w,h); response: WireBuffer
plugin/handle_event            → notification: topic, payload
plugin/cli_dispatch            → request: namespace, argv; response: stdout/stderr/exit

host/snapshot/get              → request: topic; response: bytes or null
host/snapshot/publish          → notification: topic, bytes
host/snapshot/subscribe        → notification: topic (host pushes plugin/handle_event on update)
host/action/invoke             → request: action, payload; response: bytes (with timeout)
host/log                       → notification: level, message
host/fs/read_dir               → request: path; response: entries (capability-gated)
host/fs/read_file              → request: path; response: bytes (capability-gated)
host/network/fetch             → request: url; response: bytes (capability-gated)
```

All requests carry an `id`; responses correlate. JSON-RPC 2.0 spec — no proprietary
extensions. Bidirectional: plugin can call host methods (host/*) and vice versa.

### Lifecycle

1. **Discovery** at host startup: read `~/.agents-in-a-box/plugins/<name>/manifest.toml`.
2. **No spawn yet.** Each plugin is "registered" but no process exists.
3. **Lazy spawn** when first needed:
   - User opens analytics screen → host spawns `burndown` binary.
   - CLI dispatch `ainb usage report` → host spawns `burndown` if not running.
   - Snapshot subscribe from another plugin → spawns the publisher.
4. **Health check**: 5s after spawn, host calls `plugin/init`. If no response in 10s,
   kill + retry.
5. **Crash recovery**: process exits non-zero or pipe closes → respawn with
   exponential backoff (1s, 4s, 16s). After 3 consecutive failures within 60s, mark
   plugin **quarantined** — UI shows fallback, retry only on user-initiated reload.
6. **Idle reaping**: plugin not used for 10 min and not subscribed to live topics →
   send `plugin/shutdown`, kill if no exit in 5s.

### Capability model — install-time + runtime

- **Install-time** (`ainb plugin install`): parse manifest. For each declared cap,
  validate the binary's request handlers don't call host methods that need an unheld
  cap. (Static check via wire-recorded sample run, OR a `--dry-run` invocation that
  asserts handler registration matches manifest.)
- **Runtime**: host enforces. If a plugin without `read_sessions` calls
  `host/fs/read_file` on `~/.claude/projects/...`, host returns
  `{ error: { code: -32001, message: "capability denied: read_sessions" } }`.

## Phases

| phase | what | wave | deps | files |
|-------|------|------|------|-------|
| 7a | Wire protocol + SDK + runtime crate (no host integration yet) | 1 | — | `crates/ainb-plugin-protocol/` (NEW), `crates/ainb-plugin-sdk-rust/` (NEW), `crates/ainb-plugin-runtime/` (NEW) |
| 7b | Host integration (replace wasmi with runtime) | 2 | 7a | `crates/ainb-core/src/app/state.rs`, `crates/ainb-core/src/main.rs`, `crates/ainb-core/src/cli/registry.rs`, DELETE `crates/ainb-plugin-host/`, `crates/ainb-plugin-api/`, `crates/ainb-plugin-cts/` |
| 7c-burndown | Re-ship burndown as subprocess plugin | 3 | 7a, 7b | `crates/ainb-plugin-burndown/` (REWRITE) |
| 7c-session-reader | Re-ship session-reader as subprocess plugin | 3 | 7a, 7b | `crates/ainb-plugin-session-reader/` (REWRITE) |
| 7d-testkit | `ainb-plugin-testkit` | 3 | 7a | `crates/ainb-plugin-testkit/` (NEW) |
| 7d-cli | `ainb plugin watch/tail/lint` | 4 | 7b, 7c | `crates/ainb-core/src/cli/plugin.rs` |
| 7e | CTS rewrite (subprocess canaries, contract tests) | 4 | 7c | `crates/ainb-plugin-cts-v2/` (NEW), `tests/canaries/` |
| 7f | Validation: tripwire 5/5 + non-blocking proof + crash recovery | 5 | 7c, 7e | `crates/ainb-core/tests/tripwire.rs`, NEW `tripwire_nonblocking.rs`, NEW `tripwire_crash_recovery.rs` |

---

## Phase 7a — Wire protocol + SDK + runtime crate

### Overview

Three new crates. **Zero host changes.** All gates green at end of phase: workspace
builds, new crates have unit tests, no behaviour change for users.

### 7a.1 — `crates/ainb-plugin-protocol/`

**Purpose**: source-of-truth wire types. Used by SDK (plugin side), runtime (host
side), and `ainb-plugin-testkit` (test side).

```rust
// lib.rs
pub mod manifest;     // Manifest v2 schema (TOML <-> Rust)
pub mod methods;      // JSON-RPC method names as &'static str constants
pub mod params;       // Request/response param structs (serde-derived)
pub mod errors;       // JSON-RPC error codes (capability denied, timeout, ...)
pub mod framing;      // Content-Length stdio framing (encode + decode)
pub mod wire_buffer;  // WireBuffer { width, height, cells: Vec<Cell> } — same shape
                      //   as ainb-plugin-api had, but moved here. cells use Vec<(K,V)>
                      //   for byte-determinism per memory ref.
```

**Tests**: round-trip every param struct through serde_json + framing encode/decode.
Reject malformed Content-Length headers. Reject methods unknown to the spec.

**Files**: NEW crate, ~600 LOC total.

### 7a.2 — `crates/ainb-plugin-sdk-rust/`

**Purpose**: ergonomic Rust API for plugin authors. Plugin author writes `fn main()`
of a binary that:

```rust
use ainb_plugin_sdk::{Plugin, Server, Result};

#[derive(Default)]
struct BurndownPlugin { /* state */ }

impl Plugin for BurndownPlugin {
    fn manifest() -> &'static str { include_str!("../manifest.toml") }
    async fn handle_event(&mut self, topic: &str, payload: &[u8]) -> Result<()> { /* ... */ }
    async fn render(&mut self, viewport: Viewport) -> Result<WireBuffer> { /* ... */ }
    async fn cli_dispatch(&mut self, ns: &str, argv: &[String]) -> Result<CliOutput> { /* ... */ }
}

#[tokio::main]
async fn main() -> Result<()> {
    Server::new(BurndownPlugin::default()).run_stdio().await
}
```

The `Server` handles framing, method dispatch, error mapping, and host method client
(`host_client.snapshot_get(topic).await`).

**Tests**: in-process echo test (Server reads from a `tokio::io::DuplexStream`,
writes to another, asserting round-trip).

**Files**: NEW crate, ~1500 LOC total.

### 7a.3 — `crates/ainb-plugin-runtime/`

**Purpose**: host-side counterpart. Spawns plugin processes, owns the JSON-RPC client
per plugin, hosts the snapshot store + channel registry + request ledger.

```rust
pub struct Runtime { /* tokio handle, plugin_tasks, snapshot_store, ... */ }

impl Runtime {
    pub fn new() -> (Self, RuntimeHandle) { /* ... */ }
    pub fn discover(&self, root: &Path) -> Result<Vec<RegisteredPlugin>> { /* ... */ }
}

// RuntimeHandle is Clone + Send — used by the TUI thread.
impl RuntimeHandle {
    pub fn render(&self, plugin_id: &str, viewport: Viewport) -> oneshot::Receiver<RenderOutcome>;
    pub fn dispatch_cli(&self, plugin_id: &str, ns: &str, argv: Vec<String>) -> oneshot::Receiver<CliOutcome>;
    pub fn try_recv_render(&self, plugin_id: &str) -> Option<WireBuffer>; // non-blocking try
    pub fn publish_snapshot(&self, topic: &str, payload: Bytes);
    pub fn snapshot_get(&self, topic: &str) -> Option<Bytes>;
    pub fn invoke_action(&self, plugin_id: &str, action: &str, payload: Bytes, timeout: Duration) -> oneshot::Receiver<Bytes>;
}
```

**Tests**: spawn a fixture plugin (a tiny binary in `crates/ainb-plugin-runtime/tests/fixtures/`),
call render/cli/snapshot/action, assert correct response. Inject crashes (kill -9 the
fixture) and assert respawn + quarantine semantics.

**Files**: NEW crate, ~2500 LOC total. Tokio runtime. Lifecycle FSM per plugin.

### Test gates (7a)

- `cargo build -p ainb-plugin-protocol -p ainb-plugin-sdk-rust -p ainb-plugin-runtime` — green
- `cargo test -p ainb-plugin-protocol` — round-trip + framing tests pass
- `cargo test -p ainb-plugin-sdk-rust` — in-process echo
- `cargo test -p ainb-plugin-runtime` — fixture plugin spawn + render + crash recovery
- Workspace still builds end-to-end (existing wasmi crates untouched)

### Risks

- **Tokio + ratatui interaction**: ratatui is sync. Runtime owns its own tokio runtime,
  TUI thread uses `try_recv` only. Mitigation: `RuntimeHandle` is the only thing the
  TUI sees, no `.await` from the render thread.
- **Process leak on host crash**: if ainb panics, plugin processes orphan. Mitigation:
  use `prctl(PR_SET_PDEATHSIG)` on Linux, `setpgid` + signal trap on macOS. Document
  in 7b.

---

## Phase 7b — Host integration (replace wasmi)

### Overview

Wire `ainb-plugin-runtime` into the host. Delete the entire wasmi stack. Plugins
don't exist yet (next phase) — host will come up empty until 7c lands.

### Changes

- `crates/ainb-core/Cargo.toml`: drop `ainb-plugin-host`, `ainb-plugin-api` deps. Add
  `ainb-plugin-runtime`, `ainb-plugin-protocol`.
- `crates/ainb-core/src/main.rs`: replace `init_plugin_host()` with `Runtime::new()`.
  Pass `RuntimeHandle` into `App`.
- `crates/ainb-core/src/app/state.rs`: `tick_plugin_renders` → `tick_plugin_runtime`.
  Calls `try_recv_render` (non-blocking). No more `pump_events`. No blocking on the
  render thread, period.
- `crates/ainb-core/src/cli/registry.rs`: `dispatch_usage_via_plugin` → calls
  `RuntimeHandle::dispatch_cli` and awaits the oneshot receiver. Delete
  `inject_session_reader_snapshot` — no more broker hack. CLI plugin path is the
  same code path as TUI (both go through Runtime).
- DELETE `crates/ainb-plugin-host/` (entire wasmi host crate).
- DELETE `crates/ainb-plugin-api/` (replaced by `ainb-plugin-protocol`).
- DELETE `crates/ainb-plugin-cts/` (replaced by `ainb-plugin-cts-v2` in 7e).

### Test gates (7b)

- `cargo build --release -p ainb` — green (workspace compiles without burndown +
  session-reader plugins, since they haven't been re-shipped yet).
- `cargo test -p ainb` — passes; usage CLI tests will fail (no plugins yet) — those
  get marked `#[ignore]` with a `phase-7c` cookie. Other tests pass.
- TUI launches without panic. Analytics screen shows "no plugin registered" placeholder.
- **Regression gate**: assert `app.tick_plugin_runtime` never calls `.await` on the
  render thread. Static lint: a build.rs that greps `state.rs::tick_plugin_runtime`
  for `.await` and fails compilation.

### Risks

- **Workspace breakage**: deleting 3 crates simultaneously is wide. Mitigation: do
  it as one atomic commit. Push to feat/plugin-phase-7 branch, gate merge on green
  CI.
- **Functional regression** (plugins gone): users on `feat/plugin` see analytics
  break. Mitigation: don't merge to main until 7c lands. Phase 7 ships as one PR
  (or stacked PRs) when the whole stack is green.

---

## Phase 7c — Migrate burndown + session-reader to subprocess

### 7c-burndown

`crates/ainb-plugin-burndown/` becomes a **binary crate** (`[[bin]] name = "ainb-plugin-burndown"`).
No more cdylib, no more `wasm32-wasip1`, no more `_init`/`_handle_event`/`_render`/`_alloc`/`_shutdown`/`_tick` exports.

- `src/main.rs`: ~30 lines — `Server::new(BurndownPlugin::default()).run_stdio().await`.
- `src/plugin.rs`: implements `Plugin` trait from SDK. State, render, CLI dispatch.
  Translates events from `host/snapshot/{publish,get}` into the plugin's UI state.
- `src/ui.rs`: unchanged from today (paints into `WireBuffer`).
- `src/cli.rs`: unchanged business logic; entry point shifts from wasm exports to
  `Plugin::cli_dispatch`.
- DELETE: `src/abi.rs`, all `#[no_mangle] extern "C" fn _init/_handle_event/...`.

`manifest.toml` v2:

```toml
[plugin]
name = "burndown"
version = "2.0.0"
abi_version = 2  # NEW — gates host compatibility
description = "Daily/weekly/project usage burndown panels"

[capabilities]
read_sessions = true
write_plugin_data = true
event_bus = true       # for snapshot/get + actions
network = []
spawn_subprocess = false

[provides]
screens = ["analytics"]
commands = ["/usage", "/burndown"]
cli_namespaces = ["usage"]

[subscribes]
snapshots = ["sessions.usage_data"]   # auto-route host/snapshot updates to handle_event

[lifecycle]
spawn = "lazy"          # spawn on first use
idle_reap_secs = 600    # kill if idle 10min and no live subscriptions
```

**Test gates**: `cargo test -p ainb-plugin-burndown` (unit + integration via testkit).

### 7c-session-reader

Same shape. `src/main.rs` thin, `src/plugin.rs` implements `Plugin`. Migrate scanner
+ cache code (which is already pure). Publishes via `host_client.snapshot_publish("sessions.usage_data", bytes)`.

**Test gates**: `cargo test -p ainb-plugin-session-reader`.

### Risks

- **Performance regression**: subprocess + JSON-RPC is slower than in-process wasmi.
  Mitigation: profile burndown CLI dispatch. Snapshot store is in-host RwLock — only
  publish + render cross a process boundary. Estimate: <5ms additional latency per
  CLI call. Acceptable.
- **Schema migration**: `UsageDataEvent` wire format reused as-is from
  `ainb-plugin-types-sessions`. Crate stays — it's just transported via JSON-RPC body
  bytes now instead of msgpack-over-event-bus.

---

## Phase 7d — DevX tooling

### 7d-testkit — `crates/ainb-plugin-testkit/`

Mock host. Plugin author writes:

```rust
use ainb_plugin_testkit::TestHost;

#[tokio::test]
async fn handles_usage_data() {
    let mut host = TestHost::new();
    host.publish_snapshot("sessions.usage_data", fixture_usage_data_bytes()).await;
    host.tick(&mut burndown_plugin).await;
    let buf = host.render(&mut burndown_plugin, Viewport::new(80, 24)).await;
    assert!(buf.contains_text("Total Calls"));
}
```

`TestHost` is in-process. No subprocess, no JSON-RPC framing — just direct method
calls against a `Plugin` implementer. Snapshot store + channel registry + request
ledger are real, just non-tokio (single-threaded).

**Test gate**: `cargo test -p ainb-plugin-testkit` — self-tests of the harness.

### 7d-cli — `ainb plugin watch/tail/lint`

- **`ainb plugin watch <plugin-name>`**: file watcher (`notify` crate) on the plugin's
  src dir. On change → `cargo build`, then send `plugin/shutdown` to the running
  instance, lazy-spawn picks up the new binary on next use. Iteration loop: 30s → 2s.
- **`ainb plugin tail`**: subscribes to host's tracing layer with plugin-id filter.
  Streams formatted JSON lines per event (handler entry, snapshot publish, action
  invoke, errors). No more silent failures.
- **`ainb plugin lint <path>`**: runs the binary with `--inspect` (SDK provides this
  flag). Plugin reports its handler set + cap usage. Lint cross-references against
  manifest. Fails with actionable error: `"plugin uses host/fs/read_file but
  manifest [capabilities].read_sessions = false"`.

**Test gates**: end-to-end test in `tests/plugin_cli.rs` — `watch` triggers rebuild,
`tail` captures known events, `lint` flags a manifest mismatch.

---

## Phase 7e — CTS rewrite

`crates/ainb-plugin-cts-v2/` — conformance test suite over the JSON-RPC spec. 14
axes mapped from the v1 CTS:

- A1 — manifest_v2_round_trip
- A2 — framing_content_length_decode (oversized header rejected, malformed rejected)
- A3 — method_dispatch (unknown method → -32601)
- A4 — capability_denied (call without cap → -32001 + actionable message)
- A5 — render_buffer_byte_determinism (same input → same WireBuffer bytes)
- A6 — snapshot_get_after_publish (round-trip)
- A7 — snapshot_subscribe_event_delivery
- A8 — action_invoke_timeout (no response in N → -32002)
- A9 — host_log_level_filtering
- A10 — fs_path_guard (capability allows specific paths only)
- A11 — graceful_shutdown
- A12 — crash_recovery (kill mid-render → respawn → next render works)
- A13 — quarantine (3 crashes / 60s → quarantine flag)
- A14 — cli_dispatch_stdout_capture

Each axis runs against a tiny canary plugin in `tests/canaries/<axis>/` (Rust
binary, ~50 LOC each). Canaries replace the WAT-based wasmi canaries.

**Test gate**: `cargo test -p ainb-plugin-cts-v2` 14/14.

---

## Phase 7f — Validation gates

Three new tripwires + the existing tripwire 4/4 reproduced.

### 7f.1 — `tripwire_nonblocking.rs`

Spawn a deliberately slow fixture plugin. Assert TUI render frame deadline is met
(<16ms per frame at 60fps) even when the plugin's `render` takes 200ms.

### 7f.2 — `tripwire_crash_recovery.rs`

Spawn burndown. Send SIGKILL via `nix::sys::signal`. Assert host marks plugin failed,
respawns, next render succeeds. Repeat 3x within 60s — assert quarantined.

### 7f.3 — `tripwire_real_data_in_tui.rs`

The gate that the v1 architecture failed: launch the full TUI in tmux, navigate to
analytics, capture the pane, **assert "Total Calls" or a project name renders**.
Not "Usage Analytics" header — actual data. Closes the gap that let "Waiting for
session-reader plugin..." pass v1 validation.

### 7f.4 — Tripwire 4/4 reproduced

The Phase 6f byte-equality CLI gate, but transported through the new runtime.
Same `usage_cli_report_fixture.snap` baseline — proves no behaviour regression
through the rewrite.

**Test gates**: all 8 tripwire tests pass (4 reproduced + 3 new + non-blocking).

---

## Open questions

None. All locked via Phase 7 interview round 1 + 2.

## Migration notes

- Existing `feat/plugin` users: nothing breaks until Phase 7 merges. Plan ships as
  one feat/plugin-phase-7 branch, all phases stacked, single merge to feat/plugin
  when 7f is green.
- Plugin authors (Stevie + future): re-port burndown + session-reader during 7c.
  Document migration recipe in `docs/plugin-spec/v2.md`.

## References

- Original plan: `plans/plugin-mvp-phases-0-5.md`
- Phase 6 plan: (no separate doc; lived in beads under `agents-in-a-box-121`)
- Distinguished-engineer review: see conversation log 2026-05-10
- Memory references applied:
  - `feedback_interview_before_swarm_kickoff` — interview before plan
  - `feedback_no_postpone_polish` — DevX tooling ships in 7, not deferred
  - `feedback_plugin_completion_real_tmux_integration` — 7f.3 asserts real data
  - `reference_msgpack_byte_determinism_vec_over_hashmap` — WireBuffer.cells is Vec<(K,V)>
  - `reference_env_lock_for_parallel_tests` — testkit + tripwire env handling
