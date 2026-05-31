//! JSON-RPC 2.0 envelope helpers.
//!
//! The runtime needs to:
//! 1. Build outbound requests with monotonic ids and (de)serialise
//!    them through the framing layer.
//! 2. Build outbound notifications (no id, no response expected).
//! 3. Classify *inbound* frames into one of:
//!    - response carrying a `result`/`error` for an outstanding id
//!    - request from the plugin asking the host to do something
//!    - notification from the plugin (host/snapshot/publish, host/log)

use ainb_plugin_protocol::errors::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic id allocator for outbound requests.
#[derive(Debug, Clone, Default)]
pub struct IdCounter {
    next: Arc<AtomicU64>,
}

impl IdCounter {
    /// Construct an [`IdCounter`] starting at 1.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Yield the next id.
    pub fn allocate(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

/// Outbound JSON-RPC 2.0 request envelope.
#[derive(Debug, Serialize)]
pub struct RequestEnvelope<'a> {
    /// Constant `"2.0"`.
    pub jsonrpc: &'a str,
    /// Outstanding correlation id.
    pub id: u64,
    /// Method name (one of [`ainb_plugin_protocol::methods`]).
    pub method: &'a str,
    /// Pre-serialised params value.
    pub params: Value,
}

/// Outbound JSON-RPC 2.0 notification envelope (no id).
#[derive(Debug, Serialize)]
pub struct NotificationEnvelope<'a> {
    /// Constant `"2.0"`.
    pub jsonrpc: &'a str,
    /// Method name.
    pub method: &'a str,
    /// Params value.
    pub params: Value,
}

/// Outbound JSON-RPC 2.0 response envelope (success arm).
#[derive(Debug, Serialize)]
pub struct ResponseEnvelope<'a> {
    /// Constant `"2.0"`.
    pub jsonrpc: &'a str,
    /// Echo of the inbound request id.
    pub id: u64,
    /// Result payload.
    pub result: Value,
}

/// Outbound JSON-RPC 2.0 error envelope.
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope<'a> {
    /// Constant `"2.0"`.
    pub jsonrpc: &'a str,
    /// Echo of the inbound request id (`Some`) or `null` when the
    /// request couldn't be parsed.
    pub id: Option<u64>,
    /// Error payload.
    pub error: RpcError,
}

/// Inbound envelope shape after parsing.
#[derive(Debug)]
pub enum Inbound {
    /// Response to a request we sent.
    Response {
        /// Correlation id.
        id: u64,
        /// Result on success, `Err(RpcError)` if the peer returned an error envelope.
        result: Result<Value, RpcError>,
    },
    /// Plugin invoked a host method that expects a response.
    Request {
        /// Correlation id from the plugin.
        id: u64,
        /// Method name.
        method: String,
        /// Raw params.
        params: Value,
    },
    /// Plugin emitted a notification (no response).
    Notification {
        /// Method name.
        method: String,
        /// Raw params.
        params: Value,
    },
}

/// Loose intermediate shape used while sniffing the envelope.
#[derive(Debug, Deserialize)]
struct Raw {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

/// Parse an inbound JSON-RPC frame body into one of [`Inbound`]'s arms.
pub fn parse_inbound(body: &[u8]) -> Result<Inbound, ainb_plugin_protocol::errors::ProtocolError> {
    let raw: Raw = serde_json::from_slice(body)
        .map_err(|e| ainb_plugin_protocol::errors::ProtocolError::Decode(e.to_string()))?;

    if raw.jsonrpc.as_deref() != Some("2.0") {
        return Err(ainb_plugin_protocol::errors::ProtocolError::Decode(
            "missing or wrong jsonrpc version".into(),
        ));
    }

    if let Some(method) = raw.method {
        let params = raw.params.unwrap_or(Value::Null);
        return match raw.id {
            Some(id) => Ok(Inbound::Request { id, method, params }),
            None => Ok(Inbound::Notification { method, params }),
        };
    }

    let id = raw.id.ok_or_else(|| {
        ainb_plugin_protocol::errors::ProtocolError::Decode("response envelope missing id".into())
    })?;
    if let Some(err) = raw.error {
        return Ok(Inbound::Response {
            id,
            result: Err(err),
        });
    }
    let result = raw.result.unwrap_or(Value::Null);
    Ok(Inbound::Response {
        id,
        result: Ok(result),
    })
}

/// Build a request body and return the serialised JSON bytes.
pub fn build_request(id: u64, method: &str, params: Value) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&RequestEnvelope {
        jsonrpc: "2.0",
        id,
        method,
        params,
    })
}

/// Build a notification body.
pub fn build_notification(method: &str, params: Value) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&NotificationEnvelope {
        jsonrpc: "2.0",
        method,
        params,
    })
}

/// Build a success response body.
pub fn build_response(id: u64, result: Value) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&ResponseEnvelope {
        jsonrpc: "2.0",
        id,
        result,
    })
}

/// Build an error response body.
pub fn build_error_response(
    id: Option<u64>,
    error: RpcError,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&ErrorEnvelope {
        jsonrpc: "2.0",
        id,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_response_ok() {
        let body = br#"{"jsonrpc":"2.0","id":7,"result":{"name":"x","version":"1"}}"#;
        match parse_inbound(body).unwrap() {
            Inbound::Response { id, result } => {
                assert_eq!(id, 7);
                let v = result.unwrap();
                assert_eq!(v["name"], "x");
            }
            other => panic!("wrong arm: {other:?}"),
        }
    }

    #[test]
    fn parse_response_error() {
        let body =
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"method not found: x"}}"#;
        match parse_inbound(body).unwrap() {
            Inbound::Response { id, result } => {
                assert_eq!(id, 1);
                assert!(result.is_err());
                let e = result.unwrap_err();
                assert_eq!(e.code, -32601);
            }
            other => panic!("wrong arm: {other:?}"),
        }
    }

    #[test]
    fn parse_request() {
        let body =
            br#"{"jsonrpc":"2.0","id":3,"method":"host/snapshot/get","params":{"topic":"t"}}"#;
        match parse_inbound(body).unwrap() {
            Inbound::Request { id, method, .. } => {
                assert_eq!(id, 3);
                assert_eq!(method, "host/snapshot/get");
            }
            other => panic!("wrong arm: {other:?}"),
        }
    }

    #[test]
    fn parse_notification() {
        let body =
            br#"{"jsonrpc":"2.0","method":"host/log","params":{"level":"info","message":"hi"}}"#;
        match parse_inbound(body).unwrap() {
            Inbound::Notification { method, .. } => assert_eq!(method, "host/log"),
            other => panic!("wrong arm: {other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_jsonrpc_version() {
        let body = br#"{"jsonrpc":"1.0","id":1,"result":null}"#;
        assert!(parse_inbound(body).is_err());
    }

    #[test]
    fn build_round_trip() {
        let bytes = build_request(1, "plugin/init", serde_json::json!({"x":1})).unwrap();
        match parse_inbound(&bytes).unwrap() {
            Inbound::Request { id, method, params } => {
                assert_eq!(id, 1);
                assert_eq!(method, "plugin/init");
                assert_eq!(params["x"], 1);
            }
            other => panic!("wrong arm: {other:?}"),
        }
    }

    #[test]
    fn id_counter_monotonic() {
        let c = IdCounter::new();
        assert_eq!(c.allocate(), 1);
        assert_eq!(c.allocate(), 2);
        assert_eq!(c.allocate(), 3);
    }
}
