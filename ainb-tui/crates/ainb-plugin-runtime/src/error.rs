//! Crate-wide error type.
//!
//! Wraps the protocol-level [`ProtocolError`] (wire decode/transport)
//! and adds host-side failures (process spawn, plugin crashed, plugin
//! quarantined, ledger timeout).

use ainb_plugin_protocol::errors::ProtocolError;
use thiserror::Error;

use crate::types::PluginId;

/// All failures that can surface from the runtime.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Underlying I/O error spawning or talking to a child process.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Protocol-level wire failure (framing, decode, peer error envelope).
    #[error("protocol: {0}")]
    Protocol(#[from] ProtocolError),

    /// Plugin id was never discovered / registered.
    #[error("unknown plugin: {0}")]
    UnknownPlugin(PluginId),

    /// Plugin is quarantined (3 failures inside the failure window).
    #[error("plugin quarantined: {0}")]
    Quarantined(PluginId),

    /// Plugin process exited or pipe closed before the request completed.
    #[error("plugin exited mid-request: {0}")]
    ProcessExited(PluginId),

    /// Caller-provided timeout elapsed before a response arrived.
    #[error("request timeout: {0}")]
    Timeout(String),

    /// JSON serialise/deserialise of a request param or response result.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// Manifest TOML failed to parse during discovery.
    #[error("manifest: {0}")]
    Manifest(String),

    /// Caller asked for an action that no plugin advertises.
    #[error("unknown action: {0}")]
    UnknownAction(String),

    /// The plugin sent a wire response with shape we couldn't reconcile.
    #[error("wire: {0}")]
    Wire(String),

    /// Runtime is shutting down; no new work accepted.
    #[error("runtime shutting down")]
    ShuttingDown,
}

impl From<toml::de::Error> for RuntimeError {
    fn from(e: toml::de::Error) -> Self {
        Self::Manifest(e.to_string())
    }
}
