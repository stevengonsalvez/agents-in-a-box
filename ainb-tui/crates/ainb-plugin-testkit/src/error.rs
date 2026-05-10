//! Testkit-side error type.
//!
//! Distinct from [`ainb_plugin_sdk::SdkError`] because the harness
//! owns its own failure modes (timeouts, missing responses, framing
//! decode failures on the host side) that aren't plugin errors.

use ainb_plugin_protocol::{ProtocolError, RpcError};

/// Result alias used throughout the testkit public API.
pub type HarnessResult<T> = std::result::Result<T, HarnessError>;

/// Failures the harness can surface to a test.
///
/// All variants are designed to short-circuit a `#[tokio::test]` via
/// `?` — the harness intentionally does not panic on its own, so test
/// authors decide whether to `unwrap()` or to assert a specific
/// [`HarnessError`] shape.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// JSON serialise / deserialise failed in test code or in a frame
    /// the harness produced.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// Stdio framing failed — usually means the plugin hung up
    /// mid-frame or wrote a malformed Content-Length header.
    #[error("framing error: {0}")]
    Protocol(#[from] ProtocolError),

    /// The plugin returned a JSON-RPC `error` envelope to a request
    /// the test issued.
    #[error("rpc error from plugin: {0:?}")]
    Rpc(Box<RpcError>),

    /// I/O failure on the in-memory duplex pipe. Almost always means
    /// the server task panicked or dropped the writer half.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// `recv_*` exhausted the supplied timeout without seeing a
    /// matching frame.
    #[error("timed out after {0:?} waiting for {1}")]
    Timeout(std::time::Duration, String),

    /// The harness saw EOF on the plugin->host pipe while still
    /// expecting a response — the server task usually exited.
    #[error("plugin closed pipe before responding to id={0}")]
    UnexpectedEof(i64),

    /// Catch-all for assertion failures inside the harness.
    #[error("assertion failed: {0}")]
    Assertion(String),
}

impl From<RpcError> for HarnessError {
    fn from(e: RpcError) -> Self {
        Self::Rpc(Box::new(e))
    }
}
