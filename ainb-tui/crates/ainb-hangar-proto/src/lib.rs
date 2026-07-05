//! Hangar wire protocol: pure JSON-RPC 2.0 envelope types.
//!
//! This crate defines the request / response / error envelopes the Hangar
//! daemon speaks over stdio and the plugin SDK consumes. It is **pure data** —
//! `serde` + `serde_json` and nothing else (no `tokio`, `sqlx`, `ratatui`, or
//! any host crate), per the plugin-SDK no-host-deps discipline. P3 builds the
//! JSON-RPC dispatcher on top of these types.
//!
//! The shapes follow [JSON-RPC 2.0](https://www.jsonrpc.org/specification):
//! every envelope carries `"jsonrpc":"2.0"`; a request adds `{id, method,
//! params}`, a response adds `{id, result|error}`, and an error object is
//! `{code, message, data?}`.

use serde::{Deserialize, Serialize};

pub mod auth;
pub mod events;
pub mod lifecycle;
pub mod methods;
pub mod pr_status;
pub mod settings;
pub mod snapshots;

/// Re-export the notification routing vocabulary (tcp T5) so proto-only consumers
/// (ainb-web, the hangar plugin) reach [`Channel`] / [`ChannelSet`] without a
/// direct `ainb-hangar-core` dependency — the same crate the wire `AttentionRow`
/// and `AttentionRaised` event carry their resolved channels as.
pub use ainb_hangar_core::channel::{Channel, ChannelSet};

/// The JSON-RPC protocol version string carried by every envelope.
pub const JSONRPC_VERSION: &str = "2.0";

/// The default `jsonrpc` member value (`"2.0"`) for [`RpcRequest`] /
/// [`RpcResponse`].
///
/// Used as the serde `default` so a peer that omits the member still decodes
/// into a spec-compliant 2.0 envelope, and as the canonical value when
/// constructing envelopes in code.
pub fn jsonrpc_version() -> String {
    JSONRPC_VERSION.to_string()
}

/// A JSON-RPC request id.
///
/// JSON-RPC 2.0 permits a string, a number, or null. We model the first two
/// explicitly and treat the `null`/absent notification id case as out of scope
/// for v1 (the Hangar transport always correlates requests with responses).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    /// A numeric id.
    Number(i64),
    /// A string id.
    Text(String),
}

/// A JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcRequest {
    /// The JSON-RPC protocol version; always `"2.0"`. Defaults to `"2.0"`
    /// when a lenient peer omits it on decode.
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// Correlates the request with its [`RpcResponse`].
    pub id: RpcId,
    /// The method name (e.g. `"hangar/ping"`).
    pub method: String,
    /// Method parameters; [`serde_json::Value::Null`] when there are none.
    pub params: serde_json::Value,
}

/// A JSON-RPC 2.0 response envelope.
///
/// Exactly one of `result` / `error` is populated, matching the JSON-RPC
/// contract; both are `Option` and skipped when absent so the serialized form
/// carries only the relevant half.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcResponse {
    /// The JSON-RPC protocol version; always `"2.0"`. Defaults to `"2.0"`
    /// when a lenient peer omits it on decode.
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    /// Echoes the originating [`RpcRequest::id`].
    pub id: RpcId,
    /// The successful result payload, when the call succeeded.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result: Option<serde_json::Value>,
    /// The error envelope, when the call failed.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<RpcError>,
}

/// A JSON-RPC 2.0 error object: `{code, message, data?}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// The numeric error code (JSON-RPC reserves `-32768..=-32000`).
    pub code: i32,
    /// A short, human-readable description of the error.
    pub message: String,
    /// Optional structured detail.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub data: Option<serde_json::Value>,
}
