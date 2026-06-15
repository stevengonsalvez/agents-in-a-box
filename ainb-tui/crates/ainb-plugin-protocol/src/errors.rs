//! JSON-RPC 2.0 error codes and the protocol error type.
//!
//! Codes -32600..=-32603 are reserved by the JSON-RPC 2.0 spec; we
//! re-use `-32601 method not found` and `-32602 invalid params`. Codes
//! in the `-32000..=-32099` server-error range carry ainb-specific
//! semantics (capability denied, action timeout, manifest validation).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Method name not registered with the dispatcher.
///
/// Spec: JSON-RPC 2.0 reserved code.
pub const METHOD_NOT_FOUND: i32 = -32601;

/// Params structure or types could not be decoded.
///
/// Spec: JSON-RPC 2.0 reserved code.
pub const INVALID_PARAMS: i32 = -32602;

/// Internal host error — an operation failed for a reason that is not the
/// caller's fault (e.g. a state file the host could not write).
///
/// Spec: JSON-RPC 2.0 reserved code.
pub const INTERNAL_ERROR: i32 = -32603;

/// Plugin or host attempted a call outside the granted capability set.
pub const CAPABILITY_DENIED: i32 = -32001;

/// `host/action/invoke` exceeded its caller-supplied timeout.
pub const ACTION_TIMEOUT: i32 = -32002;

/// Manifest failed schema validation at install time or `plugin/init`.
pub const MANIFEST_VALIDATION: i32 = -32003;

/// `host/secret_store_get` found no secret for the requested
/// `(scope, key)` pair in the platform secret store.
pub const SECRET_NOT_FOUND: i32 = -32004;

/// The requested host capability is not implemented on this platform
/// (e.g. the linux Secret Service backend, deferred to a later phase).
pub const NOT_IMPLEMENTED: i32 = -32005;

/// The platform secret store is locked (e.g. a macOS Keychain the user
/// has not unlocked). The plugin may retry after the user unlocks it.
pub const SECRET_BACKEND_LOCKED: i32 = -32006;

/// Access to the platform secret store was denied (e.g. the user
/// dismissed the Keychain authorization prompt).
pub const SECRET_ACCESS_DENIED: i32 = -32007;

/// Wire shape of a JSON-RPC 2.0 error object.
///
/// Carried inside a `{"jsonrpc":"2.0","id":..,"error":{..}}` envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// Numeric error code. See the module-level constants for the
    /// codes ainb defines.
    pub code: i32,
    /// Short human-readable description.
    pub message: String,
    /// Optional structured payload — call-site context, paths, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcError {
    /// Build a generic error with `code` + `message` and no `data`.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attach an arbitrary JSON payload as `data`.
    #[must_use]
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Constructor for [`METHOD_NOT_FOUND`] with the missing method name in the message.
    #[must_use]
    pub fn method_not_found(method: impl Into<String>) -> Self {
        let m = method.into();
        Self::new(METHOD_NOT_FOUND, format!("method not found: {m}"))
    }

    /// Constructor for [`CAPABILITY_DENIED`] naming the capability the caller lacks.
    #[must_use]
    pub fn capability_denied(cap: impl Into<String>) -> Self {
        let c = cap.into();
        Self::new(CAPABILITY_DENIED, format!("capability denied: {c}"))
    }

    /// Constructor for [`ACTION_TIMEOUT`] naming the action that timed out.
    #[must_use]
    pub fn action_timeout(action: impl Into<String>) -> Self {
        let a = action.into();
        Self::new(ACTION_TIMEOUT, format!("action timed out: {a}"))
    }

    /// Constructor for [`MANIFEST_VALIDATION`] with a free-form reason string.
    #[must_use]
    pub fn manifest_validation(reason: impl Into<String>) -> Self {
        Self::new(MANIFEST_VALIDATION, reason)
    }

    /// Constructor for [`INVALID_PARAMS`] with a free-form reason string.
    #[must_use]
    pub fn invalid_params(reason: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, reason)
    }

    /// Constructor for [`INTERNAL_ERROR`] with a free-form reason string.
    #[must_use]
    pub fn internal(reason: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, reason)
    }

    /// Constructor for [`SECRET_NOT_FOUND`] naming the `(scope, key)`
    /// pair the platform secret store had no entry for.
    #[must_use]
    pub fn secret_not_found(scope: impl Into<String>, key: impl Into<String>) -> Self {
        let (s, k) = (scope.into(), key.into());
        Self::new(
            SECRET_NOT_FOUND,
            format!("secret not found: scope={s} key={k}"),
        )
    }

    /// Constructor for [`SECRET_BACKEND_LOCKED`] naming the `(scope, key)`
    /// the locked backend refused to read.
    #[must_use]
    pub fn secret_backend_locked(scope: impl Into<String>, key: impl Into<String>) -> Self {
        let (s, k) = (scope.into(), key.into());
        Self::new(
            SECRET_BACKEND_LOCKED,
            format!("secret backend locked: scope={s} key={k}"),
        )
    }

    /// Constructor for [`SECRET_ACCESS_DENIED`] naming the `(scope, key)`
    /// the user/OS denied access to.
    #[must_use]
    pub fn secret_access_denied(scope: impl Into<String>, key: impl Into<String>) -> Self {
        let (s, k) = (scope.into(), key.into());
        Self::new(
            SECRET_ACCESS_DENIED,
            format!("secret access denied: scope={s} key={k}"),
        )
    }

    /// Constructor for [`NOT_IMPLEMENTED`] with a free-form reason string.
    #[must_use]
    pub fn not_implemented(reason: impl Into<String>) -> Self {
        Self::new(NOT_IMPLEMENTED, reason)
    }
}

/// Errors raised while encoding/decoding the wire protocol.
///
/// Distinct from [`RpcError`]: these are local failures that prevent
/// a frame from ever leaving (or being consumed by) this process. An
/// `RpcError` is a peer-reported failure travelling on the wire.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// Underlying I/O failure on the stdio pipe.
    #[error("transport: {0}")]
    Transport(#[from] std::io::Error),

    /// Frame body could not be parsed as the expected JSON shape.
    #[error("decode: {0}")]
    Decode(String),

    /// Peer returned a JSON-RPC error envelope.
    #[error("server: {code} {message}")]
    Server {
        /// JSON-RPC error code from the peer.
        code: i32,
        /// Human-readable message from the peer.
        message: String,
    },
}

impl From<RpcError> for ProtocolError {
    fn from(e: RpcError) -> Self {
        Self::Server {
            code: e.code,
            message: e.message,
        }
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(e: serde_json::Error) -> Self {
        Self::Decode(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_error_round_trip() {
        let e = RpcError::capability_denied("read_sessions")
            .with_data(serde_json::json!({"path": "/tmp/x"}));
        let s = serde_json::to_string(&e).unwrap();
        let back: RpcError = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn rpc_error_omits_null_data() {
        let e = RpcError::new(METHOD_NOT_FOUND, "x");
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains("data"));
    }

    #[test]
    fn codes_are_unique() {
        let codes = [
            METHOD_NOT_FOUND,
            INVALID_PARAMS,
            CAPABILITY_DENIED,
            ACTION_TIMEOUT,
            MANIFEST_VALIDATION,
            SECRET_NOT_FOUND,
            NOT_IMPLEMENTED,
            SECRET_BACKEND_LOCKED,
            SECRET_ACCESS_DENIED,
        ];
        let mut sorted = codes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len());
    }

    #[test]
    fn secret_store_error_codes_have_expected_values() {
        // Locked by the P5.2 contract.
        assert_eq!(SECRET_NOT_FOUND, -32004);
        assert_eq!(NOT_IMPLEMENTED, -32005);
        assert_eq!(SECRET_BACKEND_LOCKED, -32006);
        assert_eq!(SECRET_ACCESS_DENIED, -32007);
    }

    #[test]
    fn secret_not_found_constructor_carries_code() {
        let e = RpcError::secret_not_found("workspace:abc", "anthropic_api_key");
        assert_eq!(e.code, SECRET_NOT_FOUND);
        assert!(e.message.contains("workspace:abc"));
        assert!(e.message.contains("anthropic_api_key"));
    }

    #[test]
    fn secret_backend_locked_constructor_carries_code() {
        let e = RpcError::secret_backend_locked("global", "openai_api_key");
        assert_eq!(e.code, SECRET_BACKEND_LOCKED);
        assert!(e.message.contains("global"));
        assert!(e.message.contains("openai_api_key"));
    }

    #[test]
    fn secret_access_denied_constructor_carries_code() {
        let e = RpcError::secret_access_denied("global", "k");
        assert_eq!(e.code, SECRET_ACCESS_DENIED);
        assert!(e.message.contains("global"));
    }

    #[test]
    fn not_implemented_constructor_carries_code() {
        let e = RpcError::not_implemented("linux Secret Service deferred to P5");
        assert_eq!(e.code, NOT_IMPLEMENTED);
        assert!(e.message.contains("linux"));
    }
}
