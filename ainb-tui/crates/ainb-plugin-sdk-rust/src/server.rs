//! [`Server`]: stdio JSON-RPC dispatch loop.
//!
//! `Server::new(plugin).run_stdio()` is the canonical entry point from
//! `#[tokio::main] fn main()`. The server:
//!
//! 1. Spawns a writer task that pumps frames from an mpsc channel out
//!    to `stdout` (Content-Length framed).
//! 2. Builds a [`HostClient`] sharing that channel + a pending-responses
//!    map for plugin-initiated requests.
//! 3. Reads frames from `stdin`, parses each as JSON-RPC, and either:
//!    - dispatches a host->plugin request/notification onto a tokio
//!      task that locks a [`Mutex`] around the plugin and calls into
//!      the [`Plugin`] trait, then writes the response frame, or
//!    - resolves a pending response oneshot for a plugin->host request.
//!
//! Each host->plugin call gets its own task so that a long-running
//! `render` that calls `host.snapshot_get(...)` mid-handler doesn't
//! deadlock the reader. The plugin itself is still serialised by the
//! mutex — only the dispatch is concurrent.
//!
//! ## Errors
//!
//! Handler [`SdkError`]s are mapped to JSON-RPC error envelopes via
//! [`SdkError::to_rpc_error`]. The server itself logs framing/IO
//! failures via `tracing` and exits the loop on EOF.

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

use ainb_plugin_protocol::{
    Manifest, RpcError, framing, methods,
    params::{
        CliDispatchParams, CliDispatchResult, HandleEventParams, HandleKeyParams,
        HandleMouseParams, PluginInitParams, PluginInitResult, PluginShutdownParams,
        PluginShutdownResult, RenderParams, RenderResult,
    },
};

use crate::host_client::{Pending, RpcOutcome};
use crate::plugin::InitContext;
use crate::{HostClient, Plugin, Result, SdkError};

/// Outbound mpsc channel buffer. Tuned for "many small frames". Bigger
/// values give the writer more slack on bursty publishes; smaller
/// values surface peer-side back-pressure faster. 64 was picked to
/// match `tower-lsp`'s default.
const FRAMES_CHANNEL_CAPACITY: usize = 64;

/// JSON-RPC server that hosts a [`Plugin`] over stdio.
pub struct Server<P: Plugin> {
    plugin: Arc<Mutex<P>>,
}

impl<P: Plugin> Server<P> {
    /// Build a server around `plugin`. No I/O yet — call [`Self::run_stdio`]
    /// or [`Self::run`] to start the dispatcher.
    pub fn new(plugin: P) -> Self {
        Self {
            plugin: Arc::new(Mutex::new(plugin)),
        }
    }

    /// Build a server around a *shared* `Arc<Mutex<P>>` so the caller can
    /// retain a handle to the plugin's state while the server runs.
    ///
    /// The runtime always uses [`Self::new`]; this exists for in-process
    /// test harnesses (e.g. `ainb-plugin-testkit`) that want to assert a
    /// plugin's internal state after an init/render round-trip without
    /// threading it back over the wire.
    #[must_use]
    pub const fn from_shared(plugin: Arc<Mutex<P>>) -> Self {
        Self { plugin }
    }

    /// Run the dispatcher reading from real `stdin`, writing to real
    /// `stdout`. Blocks until either side closes.
    pub async fn run_stdio(self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        self.run(stdin, stdout).await
    }

    /// Run the dispatcher over arbitrary async I/O. Used by the
    /// in-process tests with [`tokio::io::DuplexStream`]. The reader
    /// is wrapped in a [`BufReader`] internally — pass the raw stream.
    pub async fn run<R, W>(self, reader: R, writer: W) -> Result<()>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let Self { plugin } = self;

        let (frames_tx, frames_rx) = mpsc::channel::<Vec<u8>>(FRAMES_CHANNEL_CAPACITY);
        let (host_client, pending) = HostClient::new(frames_tx.clone());

        let writer_handle = spawn_writer_task(writer, frames_rx);

        let read_result = read_loop(reader, plugin, host_client, pending, frames_tx).await;

        // Closing the channel signals the writer to drain + exit.
        drop(read_result.frames_tx);
        let writer_outcome = writer_handle.await;

        // Surface the first error we hit. Reader error wins because it's
        // the loop body; writer errors are usually downstream of it.
        match read_result.outcome {
            Ok(()) => match writer_outcome {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(join_err) => Err(SdkError::plugin(format!(
                    "writer task panicked: {join_err}"
                ))),
            },
            Err(e) => Err(e),
        }
    }
}

struct ReadLoopOutcome {
    outcome: Result<()>,
    frames_tx: mpsc::Sender<Vec<u8>>,
}

/// Run the reader/dispatcher loop until the input pipe closes or hits
/// an unrecoverable framing error.
async fn read_loop<P, R>(
    reader: R,
    plugin: Arc<Mutex<P>>,
    host_client: HostClient,
    pending: Pending,
    frames_tx: mpsc::Sender<Vec<u8>>,
) -> ReadLoopOutcome
where
    P: Plugin,
    R: AsyncRead + Send + Unpin + 'static,
{
    let mut reader = BufReader::new(reader);
    let mut dispatch_tasks: Vec<JoinHandle<()>> = Vec::new();

    let outcome = loop {
        let frame = match read_frame_async(&mut reader).await {
            Ok(Some(b)) => b,
            Ok(None) => break Ok(()),
            Err(e) => break Err(SdkError::Protocol(e)),
        };

        let value: Value = match serde_json::from_slice(&frame) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "invalid JSON in incoming frame — dropping");
                continue;
            }
        };

        if let Some(method) = value.get("method").and_then(Value::as_str) {
            // host -> plugin request or notification.
            //
            // `plugin/handle_event` and `plugin/handle_key` MUST run in
            // receive order — chunked publishes (e.g.
            // `sessions.usage_data`) rely on the consumer seeing
            // `chunk_index = 0` before any follow-on chunk, and key
            // sequences (`1`, `2`, `Tab`, `Esc`) would lose their
            // semantics if dispatched out of order. Spawning each
            // notification onto its own task lets tokio re-order them
            // through the per-plugin mutex, so we serve these inline
            // and only spawn for other methods.
            if method == methods::PLUGIN_HANDLE_EVENT || method == methods::PLUGIN_HANDLE_KEY {
                dispatch_incoming(
                    plugin.clone(),
                    host_client.clone(),
                    frames_tx.clone(),
                    value,
                )
                .await;
            } else {
                let plugin = plugin.clone();
                let host_client = host_client.clone();
                let frames_tx = frames_tx.clone();
                dispatch_tasks.push(tokio::spawn(async move {
                    dispatch_incoming(plugin, host_client, frames_tx, value).await;
                }));
            }
        } else if let Some(id) = value.get("id").and_then(Value::as_i64) {
            // Response to a plugin-initiated request.
            let outcome = parse_response(&value);
            HostClient::resolve(&pending, id, outcome).await;
        } else {
            tracing::warn!(?value, "ignoring frame with neither method nor id");
        }

        // Reap any finished dispatchers so we don't leak join handles
        // for the lifetime of the process.
        dispatch_tasks.retain(|h| !h.is_finished());
    };

    // Drain remaining dispatch tasks so we don't drop pending writes.
    for h in dispatch_tasks {
        let _ = h.await;
    }

    ReadLoopOutcome { outcome, frames_tx }
}

/// Pump a single frame through tokio's async `BufReader`.
async fn read_frame_async<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::result::Result<Option<Vec<u8>>, ainb_plugin_protocol::ProtocolError> {
    use ainb_plugin_protocol::ProtocolError;

    const HEADER_BUDGET: usize = 8 * 1024;
    const MAX_BODY_BYTES: usize = framing::MAX_BODY_BYTES;

    let mut content_length: Option<usize> = None;
    let mut header_bytes_seen = 0usize;
    let mut any_byte_read = false;

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.map_err(ProtocolError::Transport)?;

        if n == 0 {
            if any_byte_read {
                return Err(ProtocolError::Decode(
                    "unexpected EOF inside header block".into(),
                ));
            }
            return Ok(None);
        }
        any_byte_read = true;
        header_bytes_seen += n;
        if header_bytes_seen > HEADER_BUDGET {
            return Err(ProtocolError::Decode(format!(
                "header block exceeded {HEADER_BUDGET} bytes"
            )));
        }
        if !line.ends_with("\r\n") {
            return Err(ProtocolError::Decode(format!(
                "header line not CRLF-terminated: {line:?}"
            )));
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length
                .ok_or_else(|| ProtocolError::Decode("missing Content-Length header".into()))?;
            if len > MAX_BODY_BYTES {
                return Err(ProtocolError::Decode(format!(
                    "Content-Length {len} exceeds MAX_BODY_BYTES {MAX_BODY_BYTES}"
                )));
            }
            let mut body = vec![0u8; len];
            tokio::io::AsyncReadExt::read_exact(reader, &mut body)
                .await
                .map_err(ProtocolError::Transport)?;
            return Ok(Some(body));
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(ProtocolError::Decode(format!(
                "malformed header line: {trimmed:?}"
            )));
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("Content-Length") {
            let parsed: usize = value.parse().map_err(|_| {
                ProtocolError::Decode(format!("non-numeric Content-Length: {value:?}"))
            })?;
            content_length = Some(parsed);
        } else {
            return Err(ProtocolError::Decode(format!("unsupported header: {name}")));
        }
    }
}

/// Spawn the writer task that pumps outgoing frames to the underlying
/// `AsyncWrite`. Exits cleanly when the channel is closed.
fn spawn_writer_task<W>(
    mut writer: W,
    mut frames_rx: mpsc::Receiver<Vec<u8>>,
) -> JoinHandle<Result<()>>
where
    W: AsyncWrite + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        while let Some(frame) = frames_rx.recv().await {
            writer.write_all(&frame).await.map_err(|e| SdkError::Protocol(e.into()))?;
            writer.flush().await.map_err(|e| SdkError::Protocol(e.into()))?;
        }
        Ok(())
    })
}

/// Parse a response frame `{id, result|error}` into an [`RpcOutcome`].
#[allow(clippy::option_if_let_else)]
fn parse_response(value: &Value) -> RpcOutcome {
    if let Some(err) = value.get("error") {
        let rpc: RpcError = serde_json::from_value(err.clone()).unwrap_or_else(|e| {
            RpcError::new(
                ainb_plugin_protocol::errors::INVALID_PARAMS,
                format!("malformed error envelope: {e}"),
            )
        });
        Err(rpc)
    } else if let Some(result) = value.get("result") {
        Ok(result.clone())
    } else {
        Err(RpcError::new(
            ainb_plugin_protocol::errors::INVALID_PARAMS,
            "response missing both result and error",
        ))
    }
}

/// Dispatch a single incoming `{method, params, id?}` frame.
///
/// Locks the plugin mutex around the handler call so handlers see
/// serialised access. If the request had an `id`, writes either a
/// `result` or `error` frame back through `frames_tx`.
async fn dispatch_incoming<P: Plugin>(
    plugin: Arc<Mutex<P>>,
    host: HostClient,
    frames_tx: mpsc::Sender<Vec<u8>>,
    value: Value,
) {
    let method = value.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
    let id = value.get("id").and_then(Value::as_i64);
    let params = value.get("params").cloned().unwrap_or(Value::Null);

    let outcome = handle_method(&plugin, &host, &method, params).await;

    let Some(id) = id else {
        // Notification — no response. Log handler errors so they're not
        // lost completely.
        if let Err(e) = outcome {
            tracing::warn!(method = %method, error = %e, "notification handler errored");
        }
        return;
    };

    let body = match outcome {
        Ok(value) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": value,
        }),
        Err(e) => {
            let rpc = e.to_rpc_error();
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": rpc,
            })
        }
    };

    let bytes = match serde_json::to_vec(&body) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize response — dropping");
            return;
        }
    };
    let frame = framing::encode(&bytes);
    if frames_tx.send(frame).await.is_err() {
        tracing::warn!("writer task gone — dropping response frame");
    }
}

/// Decode `params` for `method`, call the matching [`Plugin`] handler,
/// and re-encode the result as a JSON [`Value`] (or an [`SdkError`]).
async fn handle_method<P: Plugin>(
    plugin: &Mutex<P>,
    host: &HostClient,
    method: &str,
    params: Value,
) -> Result<Value> {
    match method {
        methods::PLUGIN_INIT => {
            let p: PluginInitParams = decode_params(params)?;
            let manifest_str = {
                let mut guard = plugin.lock().await;
                let ctx = InitContext {
                    granted_capabilities: &p.granted_capabilities,
                    config: &p.config,
                };
                guard.on_init(host, ctx).await?;
                guard.manifest()
            };
            let manifest: Manifest = toml::from_str(manifest_str).map_err(|e| {
                SdkError::Rpc(Box::new(RpcError::manifest_validation(format!(
                    "manifest TOML invalid: {e}"
                ))))
            })?;
            let result = PluginInitResult {
                name: manifest.plugin.name,
                version: manifest.plugin.version,
            };
            Ok(serde_json::to_value(result)?)
        }
        methods::PLUGIN_SHUTDOWN => {
            let _: PluginShutdownParams = decode_params(params)?;
            plugin.lock().await.on_shutdown(host).await?;
            Ok(serde_json::to_value(PluginShutdownResult::default())?)
        }
        methods::PLUGIN_RENDER => {
            let p: RenderParams = decode_params(params)?;
            let buf = plugin.lock().await.render(host, p).await?;
            Ok(serde_json::to_value(RenderResult { buffer: buf })?)
        }
        methods::PLUGIN_HANDLE_EVENT => {
            let p: HandleEventParams = decode_params(params)?;
            plugin.lock().await.handle_event(host, p).await?;
            Ok(Value::Null)
        }
        methods::PLUGIN_HANDLE_KEY => {
            let p: HandleKeyParams = decode_params(params)?;
            plugin.lock().await.handle_key(host, p).await?;
            Ok(Value::Null)
        }
        methods::PLUGIN_HANDLE_MOUSE => {
            let p: HandleMouseParams = decode_params(params)?;
            plugin.lock().await.handle_mouse(host, p).await?;
            Ok(Value::Null)
        }
        methods::PLUGIN_CLI_DISPATCH => {
            let p: CliDispatchParams = decode_params(params)?;
            let out = plugin.lock().await.cli_dispatch(host, &p.namespace, &p.argv).await?;
            let result = CliDispatchResult {
                stdout: out.stdout.into(),
                stderr: out.stderr.into(),
                exit_code: out.exit_code,
            };
            Ok(serde_json::to_value(result)?)
        }
        other => Err(SdkError::Rpc(Box::new(RpcError::method_not_found(other)))),
    }
}

fn decode_params<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|e| SdkError::Rpc(Box::new(RpcError::invalid_params(e.to_string()))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ainb_plugin_protocol::params::{LogLevel, Viewport};
    use ainb_plugin_protocol::wire_buffer::{Cell, Coord, WireBuffer};

    struct EchoPlugin {
        renders: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Plugin for EchoPlugin {
        fn manifest(&self) -> &'static str {
            "[plugin]\nname = \"echo\"\nversion = \"0.1.0\"\nabi_version = 2\n"
        }
        async fn render(&mut self, _host: &HostClient, _p: RenderParams) -> Result<WireBuffer> {
            self.renders.fetch_add(1, Ordering::SeqCst);
            let mut b = WireBuffer::new(2, 1);
            b.push(Coord::new(0, 0), Cell::new("h"));
            b.push(Coord::new(1, 0), Cell::new("i"));
            Ok(b)
        }
        async fn handle_event(&mut self, host: &HostClient, p: HandleEventParams) -> Result<()> {
            // Echo the payload back as a notification log.
            host.log(
                LogLevel::Info,
                format!("got topic {} ({} bytes)", p.topic, p.payload.len()),
                None,
            )
            .await
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn frame(method: &str, id: Option<i64>, params: Value) -> Vec<u8> {
        let mut body = serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params});
        if let Some(id) = id {
            body["id"] = serde_json::json!(id);
        }
        let bytes = serde_json::to_vec(&body).unwrap();
        framing::encode(&bytes)
    }

    async fn drain_frames(reader: &mut BufReader<tokio::io::DuplexStream>) -> Vec<Value> {
        let mut out = Vec::new();
        while let Ok(Some(body)) = read_frame_async(reader).await {
            let v: Value = serde_json::from_slice(&body).unwrap();
            out.push(v);
        }
        out
    }

    #[tokio::test]
    async fn init_render_shutdown_round_trip() {
        let (mut host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);

        let renders = Arc::new(AtomicUsize::new(0));
        let plugin = EchoPlugin {
            renders: renders.clone(),
        };
        let server_task = tokio::spawn(Server::new(plugin).run(plugin_read, plugin_write));

        // init
        host_side
            .write_all(&frame(
                methods::PLUGIN_INIT,
                Some(1),
                serde_json::json!({
                    "manifest_path": "/x/manifest.toml",
                    "granted_capabilities": [],
                    "abi_version": 2,
                }),
            ))
            .await
            .unwrap();
        // render
        host_side
            .write_all(&frame(
                methods::PLUGIN_RENDER,
                Some(2),
                serde_json::json!({
                    "viewport": { "width": 80, "height": 24 },
                    "generation": 0
                }),
            ))
            .await
            .unwrap();
        // shutdown
        host_side
            .write_all(&frame(
                methods::PLUGIN_SHUTDOWN,
                Some(3),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        host_side.shutdown().await.unwrap();

        let mut host_read = BufReader::new(host_side);
        let frames = drain_frames(&mut host_read).await;
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0]["id"], 1);
        assert_eq!(frames[0]["result"]["name"], "echo");
        assert_eq!(frames[1]["id"], 2);
        assert!(frames[1]["result"]["buffer"].is_object());
        assert_eq!(frames[2]["id"], 3);

        let _ = server_task.await.unwrap();
        assert_eq!(renders.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_method_returns_method_not_found() {
        let (mut host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let renders = Arc::new(AtomicUsize::new(0));
        let plugin = EchoPlugin { renders };
        let server_task = tokio::spawn(Server::new(plugin).run(plugin_read, plugin_write));

        host_side.write_all(&frame("plugin/nope", Some(1), Value::Null)).await.unwrap();
        host_side.shutdown().await.unwrap();

        let mut host_read = BufReader::new(host_side);
        let frames = drain_frames(&mut host_read).await;
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["id"], 1);
        assert_eq!(
            frames[0]["error"]["code"],
            ainb_plugin_protocol::errors::METHOD_NOT_FOUND
        );

        let _ = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_params_mapped_to_invalid_params_error() {
        let (mut host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let renders = Arc::new(AtomicUsize::new(0));
        let server_task =
            tokio::spawn(Server::new(EchoPlugin { renders }).run(plugin_read, plugin_write));

        // Render with missing required fields.
        host_side
            .write_all(&frame(
                methods::PLUGIN_RENDER,
                Some(1),
                serde_json::json!({"nope": true}),
            ))
            .await
            .unwrap();
        host_side.shutdown().await.unwrap();

        let mut host_read = BufReader::new(host_side);
        let frames = drain_frames(&mut host_read).await;
        assert_eq!(frames[0]["id"], 1);
        assert_eq!(
            frames[0]["error"]["code"],
            ainb_plugin_protocol::errors::INVALID_PARAMS
        );

        let _ = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn handle_event_notification_triggers_outbound_log() {
        let (mut host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let renders = Arc::new(AtomicUsize::new(0));
        let server_task =
            tokio::spawn(Server::new(EchoPlugin { renders }).run(plugin_read, plugin_write));

        // Notification: no id.
        host_side
            .write_all(&frame(
                methods::PLUGIN_HANDLE_EVENT,
                None,
                serde_json::json!({
                    "topic": "t",
                    "payload": [104, 105],
                }),
            ))
            .await
            .unwrap();
        host_side.shutdown().await.unwrap();

        let mut host_read = BufReader::new(host_side);
        let frames = drain_frames(&mut host_read).await;
        // No response for the notification; we expect an outbound log.
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["method"], methods::HOST_LOG);
        assert_eq!(frames[0]["params"]["level"], "info");
        assert!(frames[0].get("id").is_none(), "log is a notification");

        let _ = server_task.await.unwrap();
    }

    #[tokio::test]
    async fn render_can_call_snapshot_get_mid_handler() {
        // Plugin that calls host.snapshot_get during render.
        struct Reverse;
        #[async_trait]
        impl Plugin for Reverse {
            fn manifest(&self) -> &'static str {
                "[plugin]\nname = \"reverse\"\nversion = \"0.0.0\"\nabi_version = 2\n"
            }
            async fn render(&mut self, host: &HostClient, _: RenderParams) -> Result<WireBuffer> {
                let snap = host.snapshot_get("topic.x").await?;
                let mut b = WireBuffer::new(1, 1);
                let label = format!("v{}", snap.version);
                b.push(Coord::new(0, 0), Cell::new(label));
                Ok(b)
            }
        }

        let (host_side, plugin_side) = tokio::io::duplex(64 * 1024);
        let (plugin_read, plugin_write) = tokio::io::split(plugin_side);
        let server_task = tokio::spawn(Server::new(Reverse).run(plugin_read, plugin_write));

        let (host_read_half, mut host_write_half) = tokio::io::split(host_side);
        let mut host_read = BufReader::new(host_read_half);

        // Send render.
        host_write_half
            .write_all(&frame(
                methods::PLUGIN_RENDER,
                Some(1),
                serde_json::json!({"viewport": {"width": 1, "height": 1}, "generation": 0}),
            ))
            .await
            .unwrap();

        // Expect the plugin to issue a snapshot/get request.
        let body = read_frame_async(&mut host_read).await.unwrap().unwrap();
        let req: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(req["method"], methods::HOST_SNAPSHOT_GET);
        assert_eq!(req["params"]["topic"], "topic.x");
        let plugin_req_id = req["id"].as_i64().unwrap();

        // Reply with version=7, no payload.
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": plugin_req_id,
            "result": {"payload": null, "version": 7},
        });
        let response_bytes = serde_json::to_vec(&response).unwrap();
        host_write_half.write_all(&framing::encode(&response_bytes)).await.unwrap();

        // Now the render response should arrive.
        let body = read_frame_async(&mut host_read).await.unwrap().unwrap();
        let render_resp: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(render_resp["id"], 1);
        assert!(render_resp["result"]["buffer"].is_object());

        host_write_half.shutdown().await.unwrap();
        let _ = server_task.await.unwrap();
    }

    // Silences `unused: Viewport` on this use path when the test
    // helpers are pruned in non-test compilation.
    #[allow(dead_code)]
    fn _viewport_in_scope() -> Viewport {
        Viewport::new(1, 1)
    }
}
