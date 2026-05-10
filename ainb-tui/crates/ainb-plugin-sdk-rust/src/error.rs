//! SDK-side error type and [`Result`] alias.
//!
//! [`SdkError`] is the error type plugin handlers return. The
//! [`Server`](crate::Server) maps it to a JSON-RPC error envelope on
//! the wire — see [`Self::to_rpc_error`].
//!
//! Two flavours are convertible:
//!
//! - [`ainb_plugin_protocol::ProtocolError`] for transport / decode
//!   failures bubbling up from the framing layer.
//! - [`ainb_plugin_protocol::RpcError`] for handler-authored error
//!   responses (capability denied, invalid params, ...).

use ainb_plugin_protocol::{ProtocolError, RpcError};
use thiserror::Error;

/// SDK result alias used throughout the public API.
pub type Result<T> = std::result::Result<T, SdkError>;

/// Errors raised inside the plugin process.
#[derive(Debug, Error)]
pub enum SdkError {
    /// Wire-protocol error (transport / decode / peer-reported).
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    /// Handler returned a JSON-RPC error envelope. The server forwards
    /// this verbatim to the host as the response payload.
    #[error("rpc {code}: {message}", code = .0.code, message = .0.message)]
    Rpc(Box<RpcError>),

    /// Handler-authored free-form failure. Mapped to a generic
    /// `INVALID_PARAMS`-coded `RpcError` on the wire.
    #[error("plugin: {0}")]
    Plugin(String),

    /// JSON serialization failed inside the SDK. Almost always a bug in
    /// a handler that returned an unserializable value.
    #[error("json: {0}")]
    Json(String),
}

impl From<RpcError> for SdkError {
    fn from(value: RpcError) -> Self {
        Self::Rpc(Box::new(value))
    }
}

impl From<serde_json::Error> for SdkError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e.to_string())
    }
}

impl SdkError {
    /// Build an [`SdkError::Plugin`] from a string-like value.
    pub fn plugin(msg: impl Into<String>) -> Self {
        Self::Plugin(msg.into())
    }

    /// Convert to a wire [`RpcError`] for serialization in a JSON-RPC
    /// error response. Transport errors collapse to a generic invalid
    /// params reply — they should never reach the wire under normal
    /// operation since the framing layer handles them.
    pub fn to_rpc_error(&self) -> RpcError {
        use ainb_plugin_protocol::errors::INVALID_PARAMS;
        match self {
            Self::Rpc(e) => (**e).clone(),
            Self::Protocol(ProtocolError::Server { code, message }) => {
                RpcError::new(*code, message.clone())
            }
            Self::Protocol(other) => RpcError::new(INVALID_PARAMS, other.to_string()),
            Self::Plugin(msg) => RpcError::new(INVALID_PARAMS, msg.clone()),
            Self::Json(msg) => RpcError::new(INVALID_PARAMS, format!("json: {msg}")),
        }
    }
}
