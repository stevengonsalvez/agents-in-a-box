//! Reverse-call channel: plugin -> host JSON-RPC client.
//!
//! [`HostClient`] is the outbound side of the plugin's stdio link. It
//! lets a plugin issue JSON-RPC requests / notifications to the host
//! while a host-initiated handler is in flight (e.g. `render` calls
//! `host.snapshot_get(...)` mid-frame).
//!
//! ## Architecture
//!
//! - One outgoing-frames mpsc channel feeds the writer task.
//! - A pending-responses map keyed by request id holds oneshots the
//!   server's reader fulfils when a `{result|error}` frame arrives.
//! - `HostClient` is `Clone` (it's an `Arc` internally) — it's safe to
//!   hand a clone to handlers and spawn helpers off the main task.
//!
//! ## Notifications vs requests
//!
//! - [`HostClient::snapshot_publish`] and [`HostClient::log`] are
//!   notifications — fire-and-forget, no `id` on the wire.
//! - [`HostClient::snapshot_get`] and [`HostClient::action_invoke`]
//!   are requests — they await a oneshot keyed on the request id.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};

use ainb_plugin_protocol::{
    RpcError, framing, methods,
    params::{
        ActionInvokeParams, ActionInvokeResult, LogLevel, LogParams, SnapshotGetParams,
        SnapshotGetResult, SnapshotPublishParams, SnapshotSubscribeParams, SnapshotSubscribeResult,
    },
};

use crate::{Result, SdkError};

/// Outcome of an outbound JSON-RPC request — either a `result` payload
/// or a peer-reported `error` envelope.
pub type RpcOutcome = std::result::Result<serde_json::Value, RpcError>;

/// Pending-responses table shared between the server reader and the
/// host client. Reader fills oneshots; client drains them.
pub(crate) type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<RpcOutcome>>>>;

#[derive(Debug)]
struct Inner {
    frames_tx: mpsc::Sender<Vec<u8>>,
    pending: Pending,
    next_id: AtomicI64,
}

/// Outbound JSON-RPC client used by plugins to call host methods.
///
/// Cheap to clone — wraps an `Arc`. Hand a clone to anything that
/// needs to publish snapshots or invoke actions; only the [`Server`]
/// owns the underlying writer half.
///
/// [`Server`]: crate::server::Server
#[derive(Debug, Clone)]
pub struct HostClient {
    inner: Arc<Inner>,
}

impl HostClient {
    /// Build a client + the matching reader-side handles. Used by
    /// [`Server`](crate::server::Server) on construction; plugin
    /// authors don't call this directly.
    pub(crate) fn new(frames_tx: mpsc::Sender<Vec<u8>>) -> (Self, Pending) {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let inner = Inner {
            frames_tx,
            pending: pending.clone(),
            next_id: AtomicI64::new(1),
        };
        (
            Self {
                inner: Arc::new(inner),
            },
            pending,
        )
    }

    fn next_id(&self) -> i64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    async fn send_notification<P: Serialize + Send + Sync>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<()> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let bytes = serde_json::to_vec(&body)?;
        let frame = framing::encode(&bytes);
        self.inner
            .frames_tx
            .send(frame)
            .await
            .map_err(|_| SdkError::plugin("frames channel closed — server is shutting down"))
    }

    /// Send a JSON-RPC request and await the response.
    async fn send_request<P, R>(&self, method: &str, params: &P) -> Result<R>
    where
        P: Serialize + Send + Sync,
        R: DeserializeOwned,
    {
        let id = self.next_id();
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);

        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let bytes = serde_json::to_vec(&body)?;
        let frame = framing::encode(&bytes);

        if self.inner.frames_tx.send(frame).await.is_err() {
            // Drain the pending entry — writer is gone, the oneshot will never fire.
            self.inner.pending.lock().await.remove(&id);
            return Err(SdkError::plugin(
                "frames channel closed — server is shutting down",
            ));
        }

        match rx.await {
            Ok(Ok(value)) => {
                let parsed: R = serde_json::from_value(value)?;
                Ok(parsed)
            }
            Ok(Err(err)) => Err(SdkError::Rpc(Box::new(err))),
            Err(_canceled) => Err(SdkError::plugin(
                "host_client response oneshot canceled — server stopped",
            )),
        }
    }

    /// Fetch the latest snapshot the host knows for a topic.
    ///
    /// `payload` is `None` if the topic has no current snapshot.
    pub async fn snapshot_get(&self, topic: impl Into<String>) -> Result<SnapshotGetResult> {
        let params = SnapshotGetParams {
            topic: topic.into(),
        };
        self.send_request(methods::HOST_SNAPSHOT_GET, &params).await
    }

    /// Publish a snapshot for a topic. Notification — returns immediately.
    pub async fn snapshot_publish(
        &self,
        topic: impl Into<String>,
        payload: impl Into<bytes::Bytes>,
    ) -> Result<()> {
        let params = SnapshotPublishParams {
            topic: topic.into(),
            payload: payload.into(),
        };
        self.send_notification(methods::HOST_SNAPSHOT_PUBLISH, &params).await
    }

    /// Subscribe to snapshot updates for a topic. The host will start
    /// pushing
    /// [`PLUGIN_HANDLE_EVENT`](ainb_plugin_protocol::methods::PLUGIN_HANDLE_EVENT)
    /// notifications whenever the topic changes.
    pub async fn snapshot_subscribe(
        &self,
        topic: impl Into<String>,
    ) -> Result<SnapshotSubscribeResult> {
        let params = SnapshotSubscribeParams {
            topic: topic.into(),
        };
        self.send_request(methods::HOST_SNAPSHOT_SUBSCRIBE, &params).await
    }

    /// Invoke a remote action with a timeout. `timeout` of [`Duration::ZERO`]
    /// means "no timeout" on the wire.
    pub async fn action_invoke(
        &self,
        action: impl Into<String>,
        payload: impl Into<bytes::Bytes>,
        timeout: Duration,
    ) -> Result<bytes::Bytes> {
        let params = ActionInvokeParams {
            action: action.into(),
            payload: payload.into(),
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        };
        let result: ActionInvokeResult =
            self.send_request(methods::HOST_ACTION_INVOKE, &params).await?;
        Ok(result.payload)
    }

    /// Emit a structured log line through the host. Notification.
    pub async fn log(
        &self,
        level: LogLevel,
        message: impl Into<String>,
        fields: Option<serde_json::Value>,
    ) -> Result<()> {
        let params = LogParams {
            level,
            message: message.into(),
            fields,
        };
        self.send_notification(methods::HOST_LOG, &params).await
    }

    /// Convenience: log at `info` with no structured fields.
    pub async fn log_info(&self, message: impl Into<String>) -> Result<()> {
        self.log(LogLevel::Info, message, None).await
    }

    /// Resolve a pending response, called by the server reader when a
    /// `{result|error}` frame for `id` arrives.
    pub(crate) async fn resolve(pending: &Pending, id: i64, outcome: RpcOutcome) {
        let entry = pending.lock().await.remove(&id);
        if let Some(tx) = entry {
            // Receiver dropped means the caller gave up — log and move on.
            let _ = tx.send(outcome);
        } else {
            tracing::warn!(id, "host_client: response for unknown request id");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn notification_serializes_without_id() {
        let (tx, mut rx) = mpsc::channel(8);
        let (client, _pending) = HostClient::new(tx);
        client.log_info("hi").await.unwrap();
        let frame = rx.recv().await.unwrap();
        // Decode frame body and check shape.
        let body = decode_body(&frame);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "host/log");
        assert!(v.get("id").is_none(), "notification must not have id");
        assert_eq!(v["params"]["level"], "info");
    }

    #[tokio::test]
    async fn request_assigns_monotonic_id() {
        let (tx, _rx) = mpsc::channel(8);
        let (client, _pending) = HostClient::new(tx);
        let id1 = client.next_id();
        let id2 = client.next_id();
        assert!(id2 > id1);
    }

    #[tokio::test]
    async fn request_response_round_trip() {
        let (tx, mut rx) = mpsc::channel(8);
        let (client, pending) = HostClient::new(tx);

        // Spawn a fake host that reads one frame, parses the id, and
        // resolves it.
        let pending_clone = pending.clone();
        tokio::spawn(async move {
            let frame = rx.recv().await.unwrap();
            let body = decode_body(&frame);
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let id = v["id"].as_i64().unwrap();
            HostClient::resolve(
                &pending_clone,
                id,
                Ok(serde_json::json!({"payload": null, "version": 0})),
            )
            .await;
        });

        let res = client.snapshot_get("t").await.unwrap();
        assert_eq!(res.version, 0);
        assert!(res.payload.is_none());
    }

    #[tokio::test]
    async fn request_error_propagates() {
        let (tx, mut rx) = mpsc::channel(8);
        let (client, pending) = HostClient::new(tx);

        let pending_clone = pending.clone();
        tokio::spawn(async move {
            let frame = rx.recv().await.unwrap();
            let body = decode_body(&frame);
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let id = v["id"].as_i64().unwrap();
            HostClient::resolve(
                &pending_clone,
                id,
                Err(RpcError::capability_denied("read_sessions")),
            )
            .await;
        });

        let err = client.snapshot_get("t").await.unwrap_err();
        match err {
            SdkError::Rpc(rpc) => assert_eq!(rpc.code, -32001),
            other => panic!("expected Rpc error, got {other:?}"),
        }
    }

    fn decode_body(frame: &[u8]) -> Vec<u8> {
        // Find CRLFCRLF.
        let needle = b"\r\n\r\n";
        let i = frame
            .windows(needle.len())
            .position(|w| w == needle)
            .expect("frame missing CRLFCRLF");
        frame[i + needle.len()..].to_vec()
    }
}
