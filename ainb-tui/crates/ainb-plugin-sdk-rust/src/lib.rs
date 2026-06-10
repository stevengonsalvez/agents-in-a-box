//! Rust SDK for ainb plugins.
//!
//! Plugin authors implement the [`Plugin`] trait, then call
//! [`Server::new(plugin).run_stdio()`] from `#[tokio::main]`. The SDK
//! handles Content-Length stdio framing, JSON-RPC method dispatch,
//! error mapping, and the reverse-call channel back to the host
//! ([`HostClient`]).
//!
//! ## Module map
//!
//! - [`error`]       — SDK [`SdkError`] and the [`Result`] alias
//! - [`plugin`]      — the [`Plugin`] trait every plugin implements
//! - [`host_client`] — [`HostClient`] is the plugin's outbound JSON-RPC client
//! - [`server`]      — [`Server`] runs the dispatcher loop over an [`AsyncRead`]/[`AsyncWrite`] pair
//! - [`parent_watch`] — macOS parent-death backstop that self-exits an orphaned plugin
//!
//! [`AsyncRead`]: tokio::io::AsyncRead
//! [`AsyncWrite`]: tokio::io::AsyncWrite
//!
//! ## Wire types
//!
//! Wire types are re-exported from [`ainb_plugin_protocol`] — the SDK
//! never duplicates them. A plugin that needs `WireBuffer`, `RpcError`,
//! `LogLevel`, etc. imports them from this crate.

pub mod error;
pub mod host_client;
pub mod parent_watch;
pub mod plugin;
pub mod server;

pub use error::{Result, SdkError};
pub use host_client::HostClient;
pub use plugin::{CliOutput, Plugin};
pub use server::Server;

// Re-export wire types so plugin authors only need this crate.
pub use ainb_plugin_protocol::{
    errors::{
        ACTION_TIMEOUT, CAPABILITY_DENIED, INVALID_PARAMS, MANIFEST_VALIDATION, METHOD_NOT_FOUND,
        ProtocolError, RpcError,
    },
    manifest::Manifest,
    methods,
    params::{
        ActionInvokeParams, ActionInvokeResult, CliDispatchParams, CliDispatchResult,
        EventStreamCancelParams, EventStreamSubscribeParams, EventStreamSubscribeResult,
        FsDirEntry, FsReadDirParams, FsReadDirResult, FsReadFileParams, FsReadFileResult,
        HandleEventParams, HandleKeyParams, KEY_MOD_ALT, KEY_MOD_CTRL, KEY_MOD_SHIFT,
        KEY_MOD_SUPER, KeyCode, KeyEvent, KeyKind, LogLevel, LogParams, NetworkFetchParams,
        NetworkFetchResult, PluginInitParams, PluginInitResult, PluginShutdownParams,
        PluginShutdownResult, RenderParams, RenderResult, SecretStoreGetParams,
        SecretStoreGetResult, SnapshotGetParams, SnapshotGetResult, SnapshotPublishParams,
        SnapshotSubscribeParams, SnapshotSubscribeResult, SpawnManagedSubprocessParams,
        SpawnManagedSubprocessResult, UnixSocketCloseParams, UnixSocketDialParams,
        UnixSocketDialResult, UnixSocketEvent, UnixSocketEventKind, UnixSocketSendParams, Viewport,
    },
    topics,
    wire_buffer::{Cell, Color, Coord, WireBuffer},
};
