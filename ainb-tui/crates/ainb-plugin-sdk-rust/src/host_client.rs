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

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};

use ainb_plugin_protocol::{
    framing,
    methods,
    params::{
        ActionInvokeParams, ActionInvokeResult, EventStreamCancelParams,
        EventStreamSubscribeParams, EventStreamSubscribeResult, LogLevel, LogParams,
        SecretStoreGetParams, SecretStoreGetResult, SnapshotGetParams, SnapshotGetResult,
        SnapshotPublishParams, SnapshotSubscribeParams, SnapshotSubscribeResult,
        SpawnManagedSubprocessParams, SpawnManagedSubprocessResult, UnixSocketCloseParams,
        UnixSocketDialParams, UnixSocketDialResult, UnixSocketSendParams,
    },
    RpcError,
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
        self.send_notification(methods::HOST_SNAPSHOT_PUBLISH, &params)
            .await
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
        self.send_request(methods::HOST_SNAPSHOT_SUBSCRIBE, &params)
            .await
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

    /// Open a cancellable streaming subscription on `topic`.
    ///
    /// Capability-gated by the plugin's `event_stream_subscribe` grant.
    /// On success the host returns an opaque, host-minted `stream_id`;
    /// thereafter the host pushes
    /// [`PLUGIN_HANDLE_EVENT`](ainb_plugin_protocol::methods::PLUGIN_HANDLE_EVENT)
    /// notifications under topic `stream:<stream_id>` until the plugin
    /// cancels (see [`Self::event_stream_cancel`]) or the plugin process
    /// leaves the `Running` state. Returns
    /// [`SdkError::Rpc`] carrying `-32001` when the cap is denied or the
    /// topic isn't on the allow-list.
    ///
    /// `since_version` requests a replay from a known position; `None`
    /// starts the stream from the topic's current version.
    pub async fn event_stream_subscribe(
        &self,
        topic: impl Into<String>,
        since_version: Option<u64>,
    ) -> Result<EventStreamSubscribeResult> {
        let params = EventStreamSubscribeParams {
            topic: topic.into(),
            since_version,
        };
        self.send_request(methods::HOST_EVENT_STREAM_SUBSCRIBE, &params)
            .await
    }

    /// Cancel a previously opened event stream. Notification —
    /// fire-and-forget. The host stops emitting `stream:<stream_id>`
    /// events; cancellation is also implicit on plugin shutdown.
    pub async fn event_stream_cancel(&self, stream_id: impl Into<String>) -> Result<()> {
        let params = EventStreamCancelParams {
            stream_id: stream_id.into(),
        };
        self.send_notification(methods::HOST_EVENT_STREAM_CANCEL, &params)
            .await
    }

    /// Ask the host to spawn a host-supervised child process.
    ///
    /// Capability-gated by the plugin's `spawn_managed_subprocess` grant,
    /// which MUST be list-form (a binary allow-list). The child inherits
    /// only the env vars named in `env_allowlist`; every other variable is
    /// stripped. The host owns the child's lifecycle and kills it when
    /// this plugin shuts down / crashes or the host exits.
    ///
    /// Returns [`SdkError::Rpc`] carrying `-32001` when the cap is denied
    /// or `bin` isn't on the allow-list, and `-32003` when the grant is
    /// the (rejected) bool-true form.
    ///
    /// On success the result carries an opaque `handle` the plugin can
    /// compose with [`Self::event_stream_subscribe`] on topic
    /// `managed:<handle>:stdout`, plus the child's `pid`.
    pub async fn spawn_managed_subprocess(
        &self,
        bin: impl Into<String>,
        argv: Vec<String>,
        env_allowlist: Vec<String>,
        cwd: Option<String>,
    ) -> Result<SpawnManagedSubprocessResult> {
        let params = SpawnManagedSubprocessParams {
            bin: bin.into(),
            argv,
            env_allowlist,
            cwd,
        };
        self.send_request(methods::HOST_SPAWN_MANAGED_SUBPROCESS, &params)
            .await
    }

    /// Dial a whitelisted `AF_UNIX` socket through the host.
    ///
    /// Capability-gated by the plugin's `unix_socket_dial` grant, which
    /// MUST be list-form (a socket-path allow-list). The host expands
    /// env vars / `~` and canonicalizes (symlink resolution) before
    /// comparing against the list, so a symlink resolving outside the
    /// whitelist is rejected.
    ///
    /// Returns [`SdkError::Rpc`] carrying `-32001` when the cap is denied
    /// or `path` isn't on the allow-list, and `-32003` when the grant is
    /// the (rejected) bool-true form.
    ///
    /// On success the result carries an opaque `stream_id`; thereafter the
    /// host pushes
    /// [`PLUGIN_HANDLE_EVENT`](ainb_plugin_protocol::methods::PLUGIN_HANDLE_EVENT)
    /// notifications under topic `socket:<stream_id>` carrying
    /// [`UnixSocketEvent`](ainb_plugin_protocol::params::UnixSocketEvent)
    /// frames until the plugin closes (see [`Self::unix_socket_close`]) or
    /// the plugin process leaves the `Running` state.
    pub async fn unix_socket_dial(
        &self,
        path: impl Into<String>,
    ) -> Result<UnixSocketDialResult> {
        let params = UnixSocketDialParams { path: path.into() };
        self.send_request(methods::HOST_UNIX_SOCKET_DIAL, &params)
            .await
    }

    /// Write bytes to a previously dialled unix socket. Notification —
    /// fire-and-forget.
    pub async fn unix_socket_send(
        &self,
        stream_id: impl Into<String>,
        bytes: impl Into<bytes::Bytes>,
    ) -> Result<()> {
        let params = UnixSocketSendParams {
            stream_id: stream_id.into(),
            bytes: bytes.into(),
        };
        self.send_notification(methods::HOST_UNIX_SOCKET_SEND, &params)
            .await
    }

    /// Close a previously dialled unix socket. Notification —
    /// fire-and-forget. The host stops emitting `socket:<stream_id>`
    /// events; closure is also implicit on plugin shutdown.
    pub async fn unix_socket_close(&self, stream_id: impl Into<String>) -> Result<()> {
        let params = UnixSocketCloseParams {
            stream_id: stream_id.into(),
        };
        self.send_notification(methods::HOST_UNIX_SOCKET_CLOSE, &params)
            .await
    }

    /// Read a secret from the platform secret store through the host.
    ///
    /// Capability-gated by the plugin's `secret_store_get` grant. List form
    /// is an allow-list of `service` strings; bool-true is an unconditional
    /// read of any service.
    ///
    /// Returns [`SdkError::Rpc`] carrying `-32001` when the cap is denied or
    /// the `service` isn't on the allow-list, `-32004` when no secret exists
    /// for the `(service, account)` pair, and `-32005` on platforms where
    /// the secret store backend is not implemented (e.g. linux).
    ///
    /// On success the result carries the secret base64-encoded in
    /// `secret_b64`; the plugin decodes it itself.
    pub async fn secret_store_get(
        &self,
        service: impl Into<String>,
        account: impl Into<String>,
    ) -> Result<SecretStoreGetResult> {
        let params = SecretStoreGetParams {
            service: service.into(),
            account: account.into(),
        };
        self.send_request(methods::HOST_SECRET_STORE_GET, &params)
            .await
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
