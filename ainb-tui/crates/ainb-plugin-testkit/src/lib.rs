//! In-process testing harness for ainb plugins.
//!
//! Plugin authors write a [`Plugin`](ainb_plugin_sdk::Plugin) impl, hand
//! it to [`run_plugin`], and drive it from test code without spawning a
//! subprocess. The harness wires the plugin into the SDK's stdio
//! [`Server`](ainb_plugin_sdk::Server) over a [`tokio::io::DuplexStream`]
//! pair, so the entire JSON-RPC + Content-Length framing path is
//! exercised exactly as it would be in production — only the OS
//! pipe is replaced.
//!
//! ## Why
//!
//! Subprocess plugin tests need a built binary on disk, an env var for
//! the path, and a working multi-process tokio runtime. That is fine for
//! end-to-end gates, but it is the wrong shape for a plugin author who
//! just wants to assert "render with viewport (80, 24) returns a 1×1
//! buffer". The testkit replaces all of that with a single function call.
//!
//! ## Quick smoke test in <30 lines
//!
//! ```no_run
//! use ainb_plugin_testkit::{run_plugin, Viewport};
//! use ainb_plugin_sdk::{HostClient, Plugin, Result};
//! use ainb_plugin_sdk::{RenderParams, WireBuffer, Cell, Coord};
//! use async_trait::async_trait;
//!
//! struct MyPlugin;
//!
//! #[async_trait]
//! impl Plugin for MyPlugin {
//!     fn manifest(&self) -> &'static str {
//!         "[plugin]\nname = \"my\"\nversion = \"0.1.0\"\nabi_version = 2\n"
//!     }
//!     async fn render(&mut self, _h: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
//!         let mut b = WireBuffer::new(1, 1);
//!         b.push(Coord::new(0, 0), Cell::new("X"));
//!         Ok(b)
//!     }
//! }
//!
//! # tokio_test_runtime(async {
//! let mut h = run_plugin(MyPlugin);
//! h.init().await.unwrap();
//! h.assert_render_round_trip(Viewport::new(1, 1)).await;
//! h.close().await;
//! # });
//! # fn tokio_test_runtime<F: std::future::Future>(_: F) {}
//! ```
//!
//! ## Architectural gates
//!
//! - **No `wasmi`** — subprocess-only world.
//! - **No `ainb-plugin-api`** — the legacy in-tree wasm trait crate is
//!   gone for testkit's purposes.
//! - **Wire types come from [`ainb_plugin_protocol`]** (via the SDK
//!   re-exports). Test code that needs `Manifest`, `RpcError`,
//!   `WireBuffer`, etc. imports them from this crate so it never has to
//!   know whether they live in protocol or sdk.
//!
//! ## Module map
//!
//! - [`harness`] — the [`Harness`] struct + [`run_plugin`] entry point
//! - [`error`]   — testkit-specific [`HarnessError`]
//! - re-exports — wire types from `ainb_plugin_protocol`

#![doc(html_root_url = "https://docs.rs/ainb-plugin-testkit/0.1.0")]

pub mod error;
pub mod harness;

pub use error::{HarnessError, HarnessResult};
pub use harness::{run_plugin, Harness, ObservedFrame};

// Re-export wire types from the protocol so test code only depends on
// this crate. Mirrors the SDK's re-export surface so test authors don't
// have to learn whether a type lives in protocol vs sdk.
pub use ainb_plugin_protocol::{
    errors::{
        ACTION_TIMEOUT, CAPABILITY_DENIED, INVALID_PARAMS, MANIFEST_VALIDATION, METHOD_NOT_FOUND,
        ProtocolError, RpcError,
    },
    manifest::Manifest,
    methods,
    params::{
        ActionInvokeParams, ActionInvokeResult, CliDispatchParams, CliDispatchResult, FsDirEntry,
        FsReadDirParams, FsReadDirResult, FsReadFileParams, FsReadFileResult, HandleEventParams,
        LogLevel, LogParams, NetworkFetchParams, NetworkFetchResult, PluginInitParams,
        PluginInitResult, PluginShutdownParams, PluginShutdownResult, RenderParams, RenderResult,
        SnapshotGetParams, SnapshotGetResult, SnapshotPublishParams, SnapshotSubscribeParams,
        SnapshotSubscribeResult, Viewport,
    },
    wire_buffer::{Cell, Color, Coord, WireBuffer},
};
