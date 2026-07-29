// ABOUTME: The bridge's client onto the hangar control plane (spec P8 / D18).
//
// The converged control plane makes the daemon the single source of truth for
// every session's open input requests. The bridge uses this two ways:
//   * OUTBOUND — read the open attention inbox (`attention/list`, fleet-wide) so
//     a new ASK / escalation can be pushed proactively to the phone.
//   * INBOUND  — route a "reply N" / free-text answer through `attention/answer`
//     so the daemon performs the ONE verified last-mile send (INV-2). The bridge
//     never send-keys an answer itself.
//
// The transport mirrors the daemon's server: dial `{hangar_home}/hangar.sock`,
// send the mandatory `auth/hello` first frame, then Content-Length-framed
// JSON-RPC. Ordinary calls use a fresh connection. Fleet subscription retains
// its authenticated socket so replay and live revisions share one ordered
// stream. Shapes come from the pure `ainb-hangar-proto` crate.

use std::path::PathBuf;
use std::time::Duration;

use ainb_hangar_proto::auth;
use ainb_hangar_proto::events::AttentionRow;
use ainb_hangar_proto::snapshots::{
    AnswerParams, AnswerResult, AtcRegisterParams, AtcRegisterResult, AtcUnregisterParams,
    AtcUnregisterResult, AttentionListParams, AttentionListResult,
};
use ainb_hangar_proto::{RpcId, RpcRequest, RpcResponse, methods};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// Upper bound on a single dial + round-trip. Local unix socket, so generous;
/// only guards against a wedged daemon.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// A failure talking to the hangar daemon. Non-fatal to the bridge: outbound
/// degrades to "no items to push", inbound reply-routing falls back to the
/// existing tmux relay.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// The hangar home / socket path could not be resolved.
    #[error("hangar home not resolvable")]
    NoHome,
    /// The daemon token file is missing/unreadable (daemon not running yet).
    #[error("daemon token unreadable: {0}")]
    Token(String),
    /// Connecting to the socket failed (daemon not listening).
    #[error("connect {path}: {source}")]
    Connect {
        /// The socket path we tried to dial.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A socket read/write failed mid-exchange.
    #[error("daemon io: {0}")]
    Io(String),
    /// The exchange did not complete within [`RPC_TIMEOUT`].
    #[error("daemon timed out after {0:?}")]
    Timeout(Duration),
    /// The daemon answered with a JSON-RPC error (e.g. `auth/hello` rejected).
    #[error("daemon rpc error {code}: {message}")]
    Rpc {
        /// The JSON-RPC error code.
        code: i32,
        /// The human-readable message.
        message: String,
    },
    /// The daemon's result payload did not match the expected shape.
    #[error("decoding daemon reply: {0}")]
    Decode(String),
}

/// The daemon unix socket path (`{hangar_home}/hangar.sock`).
#[must_use]
pub fn socket_path() -> Option<PathBuf> {
    Some(ainb_hangar_core::hangar_home()?.join("hangar.sock"))
}

/// Client for stateless daemon RPCs and persistent Fleet subscription.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    socket: PathBuf,
    token: String,
}

/// One pushed update from a persistent Fleet subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetStreamEvent {
    /// One durable revision committed after the subscription snapshot.
    Revision(ainb_hangar_proto::fleet::FleetEvent),
    /// Server detected subscriber lag and requires snapshot reconciliation.
    ResyncRequired,
}

/// Live Fleet connection retained after the subscribe acknowledgement.
pub struct FleetSubscription {
    reader: BufReader<OwnedReadHalf>,
    // Retain the write half so the server does not observe EOF and tear down
    // the per-connection Fleet forwarder after the acknowledgement.
    _writer: OwnedWriteHalf,
}

impl FleetSubscription {
    /// Wait for the next Fleet revision or explicit resync request.
    pub async fn next_event(&mut self) -> Result<FleetStreamEvent, DaemonError> {
        loop {
            let frame = read_frame(&mut self.reader).await?;
            let Some(method) = frame.get("method").and_then(Value::as_str) else {
                continue;
            };
            match method {
                "fleet/event" => {
                    let params = frame.get("params").cloned().unwrap_or(Value::Null);
                    let event = serde_json::from_value(params)
                        .map_err(|error| DaemonError::Decode(error.to_string()))?;
                    return Ok(FleetStreamEvent::Revision(event));
                }
                "fleet/resync_required" => return Ok(FleetStreamEvent::ResyncRequired),
                _ => {}
            }
        }
    }
}

impl DaemonClient {
    /// Resolve the socket + token from the environment. Errors when the home is
    /// unresolvable or the token file is absent (daemon not up) — the caller
    /// degrades on either.
    pub fn from_env() -> Result<Self, DaemonError> {
        let socket = socket_path().ok_or(DaemonError::NoHome)?;
        let token_path = auth::default_token_file().ok_or(DaemonError::NoHome)?;
        let token = std::fs::read_to_string(&token_path)
            .map_err(|e| DaemonError::Token(e.to_string()))?
            .trim()
            .to_string();
        Ok(Self { socket, token })
    }

    /// Construct from explicit parts (the test seam).
    #[must_use]
    pub fn with_parts(socket: PathBuf, token: String) -> Self {
        Self { socket, token }
    }

    /// Snapshot the OPEN fleet-wide attention inbox (`attention/list`,
    /// `fleet = true`), oldest-first.
    pub async fn attention_list_fleet(&self) -> Result<Vec<AttentionRow>, DaemonError> {
        let params = serde_json::to_value(AttentionListParams {
            workspace_id: None,
            fleet: true,
        })
        .expect("AttentionListParams serializes");
        let result = self.call(methods::ATTENTION_LIST, params).await?;
        let parsed: AttentionListResult =
            serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))?;
        Ok(parsed.attention)
    }

    /// Answer one open attention row (`attention/answer`). The daemon runs the
    /// first-answer-wins + C1 guards and performs the verified last-mile send.
    pub async fn answer(&self, params: AnswerParams) -> Result<AnswerResult, DaemonError> {
        let value = serde_json::to_value(params).expect("AnswerParams serializes");
        let result = self.call(methods::ATTENTION_ANSWER, value).await?;
        serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))
    }

    /// Read the authoritative host Fleet snapshot.
    pub async fn fleet_snapshot(
        &self,
    ) -> Result<ainb_hangar_proto::fleet::FleetSnapshot, DaemonError> {
        let result = self.call(methods::FLEET_SNAPSHOT, json!({})).await?;
        serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))
    }

    /// Subscribe from one revision and receive race-free snapshot plus replay.
    pub async fn fleet_subscribe(
        &self,
        after_revision: i64,
    ) -> Result<ainb_hangar_proto::fleet::FleetSubscribeResult, DaemonError> {
        let result = self
            .call(
                methods::FLEET_SUBSCRIBE,
                json!({ "after_revision": after_revision }),
            )
            .await?;
        serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))
    }

    /// Open a persistent Fleet subscription and retain its live event stream.
    pub async fn open_fleet_subscription(
        &self,
        after_revision: i64,
    ) -> Result<
        (
            ainb_hangar_proto::fleet::FleetSubscribeResult,
            FleetSubscription,
        ),
        DaemonError,
    > {
        tokio::time::timeout(
            RPC_TIMEOUT,
            self.open_fleet_subscription_inner(after_revision),
        )
        .await
        .map_err(|_| DaemonError::Timeout(RPC_TIMEOUT))?
    }

    /// Execute one exact, versioned Fleet action.
    pub async fn fleet_action(
        &self,
        params: ainb_hangar_proto::fleet::FleetActionParams,
    ) -> Result<ainb_hangar_proto::fleet::FleetActionReceipt, DaemonError> {
        let value = serde_json::to_value(params).expect("FleetActionParams serializes");
        let result = self.call(methods::FLEET_ACTION, value).await?;
        let result: ainb_hangar_proto::fleet::FleetActionResult =
            serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))?;
        Ok(result.receipt)
    }

    /// Broadcast text to explicit stable Fleet targets.
    pub async fn fleet_broadcast(
        &self,
        params: ainb_hangar_proto::fleet::FleetBroadcastParams,
    ) -> Result<ainb_hangar_proto::fleet::FleetBroadcastResult, DaemonError> {
        let value = serde_json::to_value(params).expect("FleetBroadcastParams serializes");
        let result = self.call(methods::FLEET_BROADCAST, value).await?;
        serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))
    }

    /// Register (or re-register) an ATC instance on the daemon (`atc/register`,
    /// spec P9 D12) — the daemon-native provisioning `ainb fleet atc setup`
    /// prefers over the legacy launchd/systemd timer. Returns the persisted name
    /// + the computed next heartbeat tick.
    pub async fn atc_register(
        &self,
        params: AtcRegisterParams,
    ) -> Result<AtcRegisterResult, DaemonError> {
        let value = serde_json::to_value(params).expect("AtcRegisterParams serializes");
        let result = self.call(methods::ATC_REGISTER, value).await?;
        serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))
    }

    /// Disable a registered ATC instance's heartbeat cron (`atc/unregister`, spec
    /// P9 D12) — the daemon-native counterpart to `ainb fleet atc teardown`'s
    /// timer removal. Clears `enabled` + `next_tick_at` so the daemon-owned
    /// heartbeat cron stops scheduling the instance. Idempotent (an unknown name
    /// answers `disabled = false`).
    pub async fn atc_unregister(
        &self,
        params: AtcUnregisterParams,
    ) -> Result<AtcUnregisterResult, DaemonError> {
        let value = serde_json::to_value(params).expect("AtcUnregisterParams serializes");
        let result = self.call(methods::ATC_UNREGISTER, value).await?;
        serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, DaemonError> {
        tokio::time::timeout(RPC_TIMEOUT, self.call_inner(method, params))
            .await
            .map_err(|_| DaemonError::Timeout(RPC_TIMEOUT))?
    }

    async fn call_inner(&self, method: &str, params: Value) -> Result<Value, DaemonError> {
        let stream =
            UnixStream::connect(&self.socket).await.map_err(|source| DaemonError::Connect {
                path: self.socket.display().to_string(),
                source,
            })?;
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        write_frame(
            &mut writer,
            methods::AUTH_HELLO,
            json!({ "token": self.token }),
            1,
        )
        .await?;
        let hello = read_response(&mut reader).await?;
        if let Some(err) = hello.error {
            return Err(DaemonError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        write_frame(&mut writer, method, params, 2).await?;
        let resp = read_response(&mut reader).await?;
        if let Some(err) = resp.error {
            return Err(DaemonError::Rpc {
                code: err.code,
                message: err.message,
            });
        }
        Ok(resp.result.unwrap_or(Value::Null))
    }

    async fn open_fleet_subscription_inner(
        &self,
        after_revision: i64,
    ) -> Result<
        (
            ainb_hangar_proto::fleet::FleetSubscribeResult,
            FleetSubscription,
        ),
        DaemonError,
    > {
        let stream =
            UnixStream::connect(&self.socket).await.map_err(|source| DaemonError::Connect {
                path: self.socket.display().to_string(),
                source,
            })?;
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        write_frame(
            &mut writer,
            methods::AUTH_HELLO,
            json!({ "token": self.token }),
            1,
        )
        .await?;
        let hello = read_response(&mut reader).await?;
        if let Some(error) = hello.error {
            return Err(DaemonError::Rpc {
                code: error.code,
                message: error.message,
            });
        }

        write_frame(
            &mut writer,
            methods::FLEET_SUBSCRIBE,
            json!({ "after_revision": after_revision }),
            2,
        )
        .await?;
        let response = read_response(&mut reader).await?;
        if let Some(error) = response.error {
            return Err(DaemonError::Rpc {
                code: error.code,
                message: error.message,
            });
        }
        let subscription = serde_json::from_value(response.result.unwrap_or(Value::Null))
            .map_err(|error| DaemonError::Decode(error.to_string()))?;
        Ok((
            subscription,
            FleetSubscription {
                reader,
                _writer: writer,
            },
        ))
    }
}

async fn write_frame(
    writer: &mut (impl AsyncWriteExt + Unpin),
    method: &str,
    params: Value,
    id: i64,
) -> Result<(), DaemonError> {
    let req = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(id),
        method: method.to_string(),
        params,
    };
    let body = serde_json::to_vec(&req).map_err(|e| DaemonError::Io(e.to_string()))?;
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    writer.write_all(&out).await.map_err(|e| DaemonError::Io(e.to_string()))?;
    writer.flush().await.map_err(|e| DaemonError::Io(e.to_string()))?;
    Ok(())
}

async fn read_response(reader: &mut BufReader<OwnedReadHalf>) -> Result<RpcResponse, DaemonError> {
    loop {
        let frame = read_frame(reader).await?;
        if frame.get("id").is_some() {
            return serde_json::from_value(frame).map_err(|e| DaemonError::Decode(e.to_string()));
        }
    }
}

async fn read_frame(reader: &mut BufReader<OwnedReadHalf>) -> Result<Value, DaemonError> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.map_err(|e| DaemonError::Io(e.to_string()))?;
        if n == 0 {
            return Err(DaemonError::Io(
                "connection closed while awaiting a frame".to_string(),
            ));
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let content_len = len.ok_or_else(|| {
                DaemonError::Decode("frame missing Content-Length header".to_string())
            })?;
            let mut body = vec![0u8; content_len];
            reader.read_exact(&mut body).await.map_err(|e| DaemonError::Io(e.to_string()))?;
            return serde_json::from_slice(&body).map_err(|e| DaemonError::Decode(e.to_string()));
        }
        if let Some((name, v)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                len = v.trim().parse().ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::fleet::{FleetEvent, FleetProvenance};
    use tokio::net::UnixListener;

    async fn write_test_frame(writer: &mut OwnedWriteHalf, value: &Value) {
        let body = serde_json::to_vec(value).expect("test frame serializes");
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        writer.write_all(&frame).await.expect("write test frame");
        writer.flush().await.expect("flush test frame");
    }

    #[tokio::test]
    async fn fleet_subscription_keeps_socket_open_for_live_revisions() {
        let temp = tempfile::tempdir().expect("temporary socket directory");
        let socket = temp.path().join("hangar.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake hangar socket");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept client");
            let (read_half, mut writer) = stream.into_split();
            let mut reader = BufReader::new(read_half);

            let hello = read_frame(&mut reader).await.expect("read auth request");
            assert_eq!(hello["method"], methods::AUTH_HELLO);
            assert_eq!(hello["params"]["token"], "test-token");
            write_test_frame(
                &mut writer,
                &json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
            )
            .await;

            let subscribe = read_frame(&mut reader).await.expect("read subscribe request");
            assert_eq!(subscribe["method"], methods::FLEET_SUBSCRIBE);
            assert_eq!(subscribe["params"]["after_revision"], 40);
            write_test_frame(
                &mut writer,
                &json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "snapshot": {"head_revision": 41, "sessions": []},
                        "replay": [],
                        "replay_state": {"state": "complete"}
                    }
                }),
            )
            .await;

            let event = FleetEvent {
                revision: 42,
                event_id: "event-42".into(),
                session_key: "codex:thread-1".into(),
                observed_at: 42,
                provenance: FleetProvenance::Authoritative,
                event_type: "turn/started".into(),
                payload: json!({}),
                session_version: 2,
                applied: true,
            };
            write_test_frame(
                &mut writer,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "fleet/event",
                    "params": event
                }),
            )
            .await;
        });

        let client = DaemonClient::with_parts(socket, "test-token".into());
        let (initial, mut subscription) =
            client.open_fleet_subscription(40).await.expect("open persistent subscription");
        assert_eq!(initial.snapshot.head_revision, 41);
        assert!(initial.replay.is_empty());

        let event = subscription.next_event().await.expect("receive live event");
        assert!(matches!(
            event,
            FleetStreamEvent::Revision(FleetEvent { revision: 42, .. })
        ));
        server.await.expect("fake server completes");
    }
}
