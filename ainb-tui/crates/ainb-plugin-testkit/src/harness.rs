//! In-process [`Harness`] that drives a [`Plugin`] over a
//! [`tokio::io::DuplexStream`].
//!
//! The harness mirrors what the host-side
//! [`ainb_plugin_runtime`](https://docs.rs/ainb-plugin-runtime) does in
//! production, minus the subprocess. The SDK [`Server`] runs on a
//! background tokio task; the harness writes raw JSON-RPC frames into
//! one half of the duplex pair and reads frames from the other half.
//!
//! ## Why a single buffered reader?
//!
//! Plugins can emit unsolicited frames at any time — `host/log`
//! notifications, `host/snapshot/publish` notifications, and
//! `host/snapshot/get` requests during a render. The harness owns the
//! only reader, so it must demultiplex:
//!
//! 1. Frames with a `result`/`error` and an `id` matching an
//!    outstanding test-issued request resolve that request.
//! 2. Frames with a `method` (with or without `id`) are recorded as
//!    [`ObservedFrame`]s for assertion APIs to inspect later.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use ainb_plugin_protocol::{
    ProtocolError, RpcError, WireBuffer, framing, methods,
    params::{HandleEventParams, PluginInitParams, RenderParams, RenderResult, Viewport},
};
use ainb_plugin_sdk::{Plugin, Server};

use crate::error::{HarnessError, HarnessResult};

/// Buffer size for the in-memory duplex pipe. Mirrors the value the
/// SDK uses in its own integration tests so framing edge cases match.
const DUPLEX_BUF_BYTES: usize = 64 * 1024;

/// Default timeout for `recv_*` calls. Tests can override via the
/// explicit [`Harness::recv_with_timeout`] entry point.
pub const DEFAULT_RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// One frame the plugin emitted that wasn't a response to an outgoing
/// request — i.e. a plugin-initiated notification or a plugin-initiated
/// request waiting for a host reply.
#[derive(Debug, Clone)]
pub struct ObservedFrame {
    /// JSON-RPC method name (e.g. `host/snapshot/publish`).
    pub method: String,
    /// Raw params value as it came across the wire. Tests usually
    /// `serde_json::from_value` this into a typed struct.
    pub params: Value,
    /// Some(id) if this is a request awaiting a response, None for a
    /// notification.
    pub id: Option<i64>,
}

impl ObservedFrame {
    /// True iff this frame is a notification (no `id`).
    #[must_use]
    pub const fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// True iff this frame is a request awaiting a host reply.
    #[must_use]
    pub const fn is_request(&self) -> bool {
        self.id.is_some()
    }
}

/// Spawn `plugin` on a background tokio task and return a [`Harness`]
/// connected to its stdio.
///
/// Must be called from inside a tokio runtime — `#[tokio::test]` is
/// the canonical caller. The plugin starts running immediately; the
/// first thing the test should do is [`Harness::init`].
pub fn run_plugin<P: Plugin>(plugin: P) -> Harness {
    run_plugin_with_state(plugin).0
}

/// Like [`run_plugin`], but also returns a handle to the plugin state.
///
/// The returned `Arc<Mutex<P>>` is the same one the SDK [`Server`] runs
/// the plugin behind, so the test can lock it and assert the plugin's
/// internal state after an init/render round-trip.
///
/// Use this when the behaviour under test mutates plugin state that has
/// no wire-observable side effect — e.g. config applied in `on_init`
/// that only the data layer reads later. The handle shares the *same*
/// mutex the server uses, so lock it only between calls (never hold it
/// across a `Harness` request, or the server's handler will block).
pub fn run_plugin_with_state<P: Plugin>(plugin: P) -> (Harness, Arc<Mutex<P>>) {
    let plugin = Arc::new(Mutex::new(plugin));
    let server_plugin = Arc::clone(&plugin);
    let (host_side, plugin_side) = tokio::io::duplex(DUPLEX_BUF_BYTES);
    let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
    let server_task = tokio::spawn(async move {
        Server::from_shared(server_plugin).run(plugin_read, plugin_write).await
    });
    let (host_read, host_write) = tokio::io::split(host_side);
    let harness = Harness {
        server_task: Some(server_task),
        plugin_in: Some(host_write),
        plugin_out: BufReader::new(host_read),
        next_id: AtomicI64::new(1),
        observed: VecDeque::new(),
    };
    (harness, plugin)
}

/// In-process JSON-RPC harness for a single [`Plugin`].
///
/// Drop the harness to stop the plugin without the clean
/// `plugin/shutdown` round-trip; prefer [`Harness::close`] in tests
/// that want a clean exit.
pub struct Harness {
    server_task: Option<JoinHandle<ainb_plugin_sdk::Result<()>>>,
    plugin_in: Option<WriteHalf<DuplexStream>>,
    plugin_out: BufReader<ReadHalf<DuplexStream>>,
    next_id: AtomicI64,
    observed: VecDeque<ObservedFrame>,
}

impl Harness {
    fn alloc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a request `{method, id, params}` and return the matching
    /// `result`. Plugin-initiated frames seen on the way are buffered
    /// in [`observed`](Self::observed_frames) so later assertions can
    /// inspect them.
    pub async fn send_request<M, P>(&mut self, method: M, params: P) -> HarnessResult<Value>
    where
        M: AsRef<str>,
        P: serde::Serialize,
    {
        let id = self.alloc_id();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method.as_ref(),
            "params": serde_json::to_value(params)?,
        });
        self.write_frame(&body).await?;
        self.recv_response_for(id, DEFAULT_RECV_TIMEOUT).await
    }

    /// Send a notification (no `id`). Notifications never produce a
    /// reply — fire-and-forget.
    pub async fn send_notification<M, P>(&mut self, method: M, params: P) -> HarnessResult<()>
    where
        M: AsRef<str>,
        P: serde::Serialize,
    {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method.as_ref(),
            "params": serde_json::to_value(params)?,
        });
        self.write_frame(&body).await
    }

    /// Send a `plugin/handle_event` notification with `topic` and a
    /// raw byte `payload`. Convenience wrapper around
    /// [`Self::send_notification`].
    pub async fn send_event<T: Into<String>>(
        &mut self,
        topic: T,
        payload: impl Into<bytes::Bytes>,
    ) -> HarnessResult<()> {
        let params = HandleEventParams {
            topic: topic.into(),
            payload: payload.into(),
        };
        self.send_notification(methods::PLUGIN_HANDLE_EVENT, params).await
    }

    /// Reply to a plugin-issued request with a `result` value.
    ///
    /// Use this when a plugin you are testing issues
    /// `host.snapshot_get(...)` mid-handler — pull the request out of
    /// [`Self::recv_request`], fabricate the reply, and call this.
    pub async fn respond<R: serde::Serialize>(&mut self, id: i64, result: R) -> HarnessResult<()> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": serde_json::to_value(result)?,
        });
        self.write_frame(&body).await
    }

    /// Reply to a plugin-issued request with an `error` envelope.
    pub async fn respond_error(&mut self, id: i64, error: RpcError) -> HarnessResult<()> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": error,
        });
        self.write_frame(&body).await
    }

    /// Wait up to `timeout` for the next plugin-initiated frame
    /// (request or notification). Returns immediately if there is
    /// already one buffered.
    pub async fn recv_observed(&mut self, timeout: Duration) -> HarnessResult<ObservedFrame> {
        if let Some(frame) = self.observed.pop_front() {
            return Ok(frame);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| HarnessError::Timeout(timeout, "plugin-initiated frame".into()))?;
            tokio::select! {
                () = tokio::time::sleep(remaining) => {
                    return Err(HarnessError::Timeout(
                        timeout,
                        "plugin-initiated frame".into(),
                    ));
                }
                frame = read_one_frame(&mut self.plugin_out) => {
                    match frame? {
                        Some(value) => match classify(&value) {
                            FrameKind::PluginInitiated(obs) => return Ok(obs),
                            FrameKind::Response { .. } => {
                                // Stash response for whoever is waiting on it.
                                // For the simple recv_observed path we just drop
                                // — there is no test pattern that calls this
                                // while a response is outstanding.
                                tracing::warn!(
                                    "harness recv_observed dropped stray response: {value}"
                                );
                            }
                            FrameKind::Garbage => {
                                tracing::warn!("harness ignored garbage frame: {value}");
                            }
                        },
                        None => {
                            return Err(HarnessError::Timeout(
                                timeout,
                                "plugin-initiated frame (EOF)".into(),
                            ));
                        }
                    }
                }
            }
        }
    }

    /// Wait up to `timeout` for the next plugin-initiated *request*
    /// (notifications are buffered and skipped). Use this when a
    /// plugin under test issues `host.snapshot_get(...)` and you need
    /// to feed it a fake reply.
    pub async fn recv_request(&mut self, timeout: Duration) -> HarnessResult<ObservedFrame> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| HarnessError::Timeout(timeout, "plugin-initiated request".into()))?;
            let frame = self.recv_observed(remaining).await?;
            if frame.is_request() {
                return Ok(frame);
            }
            // Notification — keep it in the log for later assertions.
            self.observed.push_back(frame);
        }
    }

    /// Borrow the buffered list of plugin-initiated frames seen so
    /// far. Useful for after-the-fact assertions.
    #[must_use]
    pub const fn observed_frames(&self) -> &VecDeque<ObservedFrame> {
        &self.observed
    }

    /// Drop everything in the observed log. Tests that loop over
    /// multiple renders can call this between iterations.
    pub fn clear_observed(&mut self) {
        self.observed.clear();
    }

    /// Pump all currently-readable frames into the observed log
    /// without blocking past `quiet_for`. Lets a test wait for the
    /// plugin to be "settled" before asserting publish topics.
    pub async fn drain_until_quiet(&mut self, quiet_for: Duration) -> HarnessResult<()> {
        loop {
            match tokio::time::timeout(quiet_for, read_one_frame(&mut self.plugin_out)).await {
                Ok(Ok(Some(value))) => match classify(&value) {
                    FrameKind::PluginInitiated(obs) => self.observed.push_back(obs),
                    FrameKind::Response { .. } | FrameKind::Garbage => {
                        tracing::warn!("drain_until_quiet ignored frame: {value}");
                    }
                },
                Ok(Ok(None)) | Err(_) => return Ok(()), // EOF or quiet long enough
                Ok(Err(e)) => return Err(e),
            }
        }
    }

    /// Send a `plugin/init` request with the canonical empty params
    /// (no granted capabilities, ABI v2, `config = null`). Returns the
    /// `PluginInitResult` echoed back from the plugin's manifest.
    pub async fn init(&mut self) -> HarnessResult<Value> {
        self.init_with_config(serde_json::Value::Null).await
    }

    /// Send a `plugin/init` request injecting a resolved per-plugin
    /// `config` table (the `[plugins.<name>]` value the host resolves and
    /// forwards on `PluginInitParams.config`). Mirrors what the runtime
    /// does so a test can prove `on_init` consumes the injected config.
    pub async fn init_with_config(&mut self, config: Value) -> HarnessResult<Value> {
        let params = PluginInitParams {
            manifest_path: "/in-process/manifest.toml".into(),
            granted_capabilities: Vec::new(),
            abi_version: 2,
            config,
        };
        self.send_request(methods::PLUGIN_INIT, params).await
    }

    /// Send `plugin/render` with `viewport`, deserialise the buffer,
    /// and return it. Doesn't perform any equality assertions —
    /// callers that want byte-stable comparison build that on top.
    pub async fn render(&mut self, viewport: Viewport) -> HarnessResult<WireBuffer> {
        let params = RenderParams {
            viewport,
            generation: 0,
        };
        let value = self.send_request(methods::PLUGIN_RENDER, params).await?;
        let result: RenderResult = serde_json::from_value(value)?;
        Ok(result.buffer)
    }

    /// Replay assertion: render the plugin twice with `viewport` and
    /// require the buffers to be byte-identical.
    ///
    /// Catches accidental nondeterminism — e.g. a plugin that mixes
    /// `Instant::now()` into its rendered text.
    pub async fn assert_render_round_trip(&mut self, viewport: Viewport) -> HarnessResult<()> {
        let first = self.render(viewport).await?;
        let second = self.render(viewport).await?;
        if first != second {
            return Err(HarnessError::Assertion(format!(
                "render at viewport {viewport:?} was non-deterministic across two calls"
            )));
        }
        Ok(())
    }

    /// Replay assertion: drain pending frames briefly, then assert
    /// that the plugin published a snapshot for `topic` at least once
    /// in this session.
    ///
    /// Drains for a 100ms quiet window so plugins that publish during
    /// `on_init` or after a `handle_event` are covered without
    /// requiring the test to manually drain.
    pub async fn assert_publishes_topic(&mut self, topic: &str) -> HarnessResult<()> {
        self.drain_until_quiet(Duration::from_millis(100)).await?;
        let found = self.observed.iter().any(|f| {
            f.method == methods::HOST_SNAPSHOT_PUBLISH
                && f.params.get("topic").and_then(Value::as_str) == Some(topic)
        });
        if !found {
            return Err(HarnessError::Assertion(format!(
                "expected at least one host/snapshot/publish for topic={topic:?} — saw {} other frames",
                self.observed.len()
            )));
        }
        Ok(())
    }

    /// Send a clean `plugin/shutdown` notification, close the writer
    /// half so the server's read loop exits, and await the background
    /// task.
    pub async fn close(mut self) -> HarnessResult<()> {
        // Notification, not request — matches what the runtime does.
        let _ = self.send_notification(methods::PLUGIN_SHUTDOWN, serde_json::json!({})).await;
        if let Some(mut writer) = self.plugin_in.take() {
            let _ = writer.shutdown().await;
        }
        if let Some(task) = self.server_task.take() {
            match task.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(HarnessError::Assertion(format!(
                    "plugin server task ended with error: {e}"
                ))),
                Err(join_err) => Err(HarnessError::Assertion(format!(
                    "plugin server task panicked: {join_err}"
                ))),
            }
        } else {
            Ok(())
        }
    }

    async fn write_frame(&mut self, body: &Value) -> HarnessResult<()> {
        let writer = self
            .plugin_in
            .as_mut()
            .ok_or_else(|| HarnessError::Assertion("harness writer already closed".into()))?;
        let bytes = serde_json::to_vec(body)?;
        writer.write_all(&framing::encode(&bytes)).await?;
        writer.flush().await?;
        Ok(())
    }

    async fn recv_response_for(&mut self, id: i64, timeout: Duration) -> HarnessResult<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| HarnessError::Timeout(timeout, format!("response for id={id}")))?;
            let read = tokio::time::timeout(remaining, read_one_frame(&mut self.plugin_out)).await;
            let value = match read {
                Ok(Ok(Some(v))) => v,
                Ok(Ok(None)) => return Err(HarnessError::UnexpectedEof(id)),
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    return Err(HarnessError::Timeout(
                        timeout,
                        format!("response for id={id}"),
                    ));
                }
            };
            match classify(&value) {
                FrameKind::Response { id: rid, outcome } if rid == id => match outcome {
                    Ok(v) => return Ok(v),
                    Err(rpc) => return Err(HarnessError::Rpc(Box::new(rpc))),
                },
                FrameKind::Response { id: rid, .. } => {
                    tracing::warn!("harness ignored stale response for id={rid}");
                }
                FrameKind::PluginInitiated(obs) => self.observed.push_back(obs),
                FrameKind::Garbage => {
                    tracing::warn!("harness ignored garbage frame: {value}");
                }
            }
        }
    }

    /// Recv with caller-supplied timeout — escape hatch for tests
    /// that need to override [`DEFAULT_RECV_TIMEOUT`].
    pub async fn recv_with_timeout(&mut self, timeout: Duration) -> HarnessResult<ObservedFrame> {
        self.recv_observed(timeout).await
    }
}

/// Inbound frame discriminator used by both `send_request` and
/// `recv_observed`. The `Garbage` variant exists so we can log+skip
/// instead of panicking on a malformed frame the SDK would already
/// have logged anyway.
enum FrameKind {
    Response {
        id: i64,
        outcome: Result<Value, RpcError>,
    },
    PluginInitiated(ObservedFrame),
    Garbage,
}

#[allow(clippy::option_if_let_else)]
fn classify(value: &Value) -> FrameKind {
    let id = value.get("id").and_then(Value::as_i64);
    let method = value.get("method").and_then(Value::as_str);
    match (method, id) {
        (Some(m), id_opt) => FrameKind::PluginInitiated(ObservedFrame {
            method: m.to_string(),
            params: value.get("params").cloned().unwrap_or(Value::Null),
            id: id_opt,
        }),
        (None, Some(id)) => {
            let outcome = if let Some(err) = value.get("error") {
                let parsed: Result<RpcError, _> = serde_json::from_value(err.clone());
                Err(parsed.unwrap_or_else(|e| {
                    RpcError::new(
                        ainb_plugin_protocol::errors::INVALID_PARAMS,
                        format!("malformed error envelope: {e}"),
                    )
                }))
            } else if let Some(result) = value.get("result") {
                Ok(result.clone())
            } else {
                Err(RpcError::new(
                    ainb_plugin_protocol::errors::INVALID_PARAMS,
                    "response missing both result and error",
                ))
            };
            FrameKind::Response { id, outcome }
        }
        _ => FrameKind::Garbage,
    }
}

/// Read a single Content-Length framed body from the duplex pipe.
///
/// Returns `Ok(None)` only on a clean EOF *between* frames — an EOF
/// inside the header block or body is surfaced as a
/// [`ProtocolError::Decode`].
async fn read_one_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> HarnessResult<Option<Value>> {
    use tokio::io::AsyncBufReadExt;

    const HEADER_BUDGET: usize = 8 * 1024;

    let mut content_length: Option<usize> = None;
    let mut header_bytes_seen = 0usize;
    let mut any_byte_read = false;

    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .map_err(|e| HarnessError::Protocol(ProtocolError::Transport(e)))?;

        if n == 0 {
            if any_byte_read {
                return Err(HarnessError::Protocol(ProtocolError::Decode(
                    "unexpected EOF inside header block".into(),
                )));
            }
            return Ok(None);
        }
        any_byte_read = true;
        header_bytes_seen += n;
        if header_bytes_seen > HEADER_BUDGET {
            return Err(HarnessError::Protocol(ProtocolError::Decode(format!(
                "header block exceeded {HEADER_BUDGET} bytes"
            ))));
        }
        if !line.ends_with("\r\n") {
            return Err(HarnessError::Protocol(ProtocolError::Decode(format!(
                "header line not CRLF-terminated: {line:?}"
            ))));
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length.ok_or_else(|| {
                HarnessError::Protocol(ProtocolError::Decode(
                    "missing Content-Length header".into(),
                ))
            })?;
            if len > framing::MAX_BODY_BYTES {
                return Err(HarnessError::Protocol(ProtocolError::Decode(format!(
                    "Content-Length {len} exceeds MAX_BODY_BYTES {}",
                    framing::MAX_BODY_BYTES
                ))));
            }
            let mut body = vec![0u8; len];
            reader
                .read_exact(&mut body)
                .await
                .map_err(|e| HarnessError::Protocol(ProtocolError::Transport(e)))?;
            let value: Value = serde_json::from_slice(&body)?;
            return Ok(Some(value));
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(HarnessError::Protocol(ProtocolError::Decode(format!(
                "malformed header line: {trimmed:?}"
            ))));
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("Content-Length") {
            let parsed: usize = value.parse().map_err(|_| {
                HarnessError::Protocol(ProtocolError::Decode(format!(
                    "non-numeric Content-Length: {value:?}"
                )))
            })?;
            content_length = Some(parsed);
        } else {
            return Err(HarnessError::Protocol(ProtocolError::Decode(format!(
                "unsupported header: {name}"
            ))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_protocol::wire_buffer::{Cell, Coord};
    use ainb_plugin_sdk::{HostClient, InitContext, Result as SdkResult};
    use async_trait::async_trait;

    /// Tiny in-tree fixture mirroring `ainb-plugin-runtime`'s subprocess
    /// fixture: render returns a 1×1 `X`, `on_init` publishes a `hello`
    /// snapshot to `fixture.greeting`. Used to prove the harness
    /// drives a real Plugin trait impl correctly.
    struct TestkitFixture;

    #[async_trait]
    impl Plugin for TestkitFixture {
        fn manifest(&self) -> &'static str {
            "[plugin]\nname = \"fixture\"\nversion = \"0.1.0\"\nabi_version = 2\n"
        }
        async fn on_init(&mut self, host: &HostClient, _ctx: InitContext<'_>) -> SdkResult<()> {
            host.snapshot_publish("fixture.greeting", bytes::Bytes::from_static(b"hello"))
                .await
        }
        async fn render(
            &mut self,
            _host: &HostClient,
            _params: RenderParams,
        ) -> SdkResult<WireBuffer> {
            let mut buf = WireBuffer::new(1, 1);
            buf.push(Coord::new(0, 0), Cell::new("X"));
            Ok(buf)
        }
    }

    #[tokio::test]
    async fn init_render_shutdown_round_trip() {
        let mut h = run_plugin(TestkitFixture);
        let init = h.init().await.expect("init ok");
        assert_eq!(init["name"], "fixture");
        assert_eq!(init["version"], "0.1.0");

        let buf = h.render(Viewport::new(80, 24)).await.expect("render ok");
        assert_eq!(buf.width, 1);
        assert_eq!(buf.height, 1);

        h.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn assert_publishes_topic_picks_up_init_snapshot() {
        let mut h = run_plugin(TestkitFixture);
        h.init().await.expect("init ok");
        h.assert_publishes_topic("fixture.greeting")
            .await
            .expect("on_init publish observed");
        h.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn assert_render_round_trip_passes_for_pure_render() {
        let mut h = run_plugin(TestkitFixture);
        h.init().await.expect("init ok");
        h.assert_render_round_trip(Viewport::new(10, 4))
            .await
            .expect("render is deterministic");
        h.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn unknown_method_surfaces_as_rpc_error() {
        let mut h = run_plugin(TestkitFixture);
        h.init().await.expect("init ok");
        let err = h
            .send_request("plugin/no-such-method", serde_json::json!({}))
            .await
            .expect_err("expected RPC error for bogus method");
        match err {
            HarnessError::Rpc(rpc) => {
                assert_eq!(rpc.code, ainb_plugin_protocol::errors::METHOD_NOT_FOUND);
            }
            other => panic!("expected Rpc, got {other:?}"),
        }
        h.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn send_event_routes_to_handle_event_default_no_op() {
        // The default handle_event implementation in the SDK is a
        // no-op — we just need to prove the notification frame
        // travels and the plugin doesn't crash.
        let mut h = run_plugin(TestkitFixture);
        h.init().await.expect("init ok");
        h.send_event("any.topic", bytes::Bytes::from_static(b"v"))
            .await
            .expect("notification accepted");
        // Render after the event still works → loop didn't deadlock.
        h.render(Viewport::new(1, 1)).await.expect("render ok");
        h.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn render_round_trip_assertion_fails_on_nondeterminism() {
        struct Wobbly {
            counter: u32,
        }
        #[async_trait]
        impl Plugin for Wobbly {
            fn manifest(&self) -> &'static str {
                "[plugin]\nname = \"wobbly\"\nversion = \"0.0.0\"\nabi_version = 2\n"
            }
            async fn render(&mut self, _h: &HostClient, _p: RenderParams) -> SdkResult<WireBuffer> {
                self.counter += 1;
                let mut b = WireBuffer::new(1, 1);
                b.push(
                    Coord::new(0, 0),
                    Cell::new(format!("{}", self.counter % 10)),
                );
                Ok(b)
            }
        }

        let mut h = run_plugin(Wobbly { counter: 0 });
        h.init().await.expect("init ok");
        let err = h
            .assert_render_round_trip(Viewport::new(1, 1))
            .await
            .expect_err("non-deterministic render should fail");
        match err {
            HarnessError::Assertion(msg) => assert!(msg.contains("non-deterministic")),
            other => panic!("expected Assertion, got {other:?}"),
        }
        h.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn assert_publishes_topic_misses_when_no_publish_happens() {
        struct Quiet;
        #[async_trait]
        impl Plugin for Quiet {
            fn manifest(&self) -> &'static str {
                "[plugin]\nname = \"quiet\"\nversion = \"0.0.0\"\nabi_version = 2\n"
            }
            async fn render(&mut self, _h: &HostClient, _p: RenderParams) -> SdkResult<WireBuffer> {
                Ok(WireBuffer::new(1, 1))
            }
        }
        let mut h = run_plugin(Quiet);
        h.init().await.expect("init ok");
        let err = h
            .assert_publishes_topic("never.published")
            .await
            .expect_err("no publish should fail assertion");
        match err {
            HarnessError::Assertion(msg) => assert!(msg.contains("never.published")),
            other => panic!("expected Assertion, got {other:?}"),
        }
        h.close().await.expect("clean close");
    }

    #[tokio::test]
    async fn recv_request_round_trips_plugin_initiated_call() {
        // Plugin calls host.snapshot_get during render, expects a fake
        // reply from the harness.
        struct NeedsSnapshot;
        #[async_trait]
        impl Plugin for NeedsSnapshot {
            fn manifest(&self) -> &'static str {
                "[plugin]\nname = \"needs\"\nversion = \"0.0.0\"\nabi_version = 2\n"
            }
            async fn render(
                &mut self,
                host: &HostClient,
                _: RenderParams,
            ) -> SdkResult<WireBuffer> {
                let snap = host.snapshot_get("topic.x").await?;
                let mut b = WireBuffer::new(1, 1);
                b.push(Coord::new(0, 0), Cell::new(format!("v{}", snap.version)));
                Ok(b)
            }
        }

        let mut h = run_plugin(NeedsSnapshot);
        h.init().await.expect("init ok");

        // Kick off render in a child task so we can answer the
        // snapshot/get request the plugin issues mid-handler.
        let render_send = tokio::spawn({
            // We can't move &mut self into another task — instead,
            // drive the request/response interleave directly:
            async {}
        });
        drop(render_send);

        // Send render manually so we control the loop.
        let id = h.alloc_id();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": methods::PLUGIN_RENDER,
            "params": serde_json::to_value(RenderParams {
                viewport: Viewport::new(1, 1),
                generation: 0,
            }).unwrap(),
        });
        h.write_frame(&body).await.unwrap();

        // Pick up the plugin->host snapshot/get request.
        let req = h.recv_request(Duration::from_secs(2)).await.expect("plugin issued a request");
        assert_eq!(req.method, methods::HOST_SNAPSHOT_GET);
        assert_eq!(req.params["topic"], "topic.x");
        let req_id = req.id.expect("plugin request has id");
        h.respond(req_id, serde_json::json!({"payload": null, "version": 9}))
            .await
            .unwrap();

        // Now the render response arrives.
        let resp = h.recv_response_for(id, Duration::from_secs(2)).await.expect("render response");
        let result: RenderResult = serde_json::from_value(resp).unwrap();
        assert_eq!(result.buffer.width, 1);

        h.close().await.expect("clean close");
    }
}
