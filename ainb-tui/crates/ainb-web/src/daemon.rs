//! Daemon control-plane client — the web surface's read + answer path onto the
//! hangar daemon (spec P8 / D18).
//!
//! Before P8 the dashboard's `needs` came from shelling `ainb fleet needs`
//! (which cold-booted a plugin runtime and capture-paned every session). The
//! converged control plane makes the daemon the single source of truth: `needs`
//! now reads the `attention/list` RPC, and ASK cards are answered by routing an
//! `attention/answer` RPC back through the daemon's ONE verified send path
//! (INV-2) rather than the web process ever touching tmux.
//!
//! This module owns the socket transport: dial `{hangar_home}/hangar.sock`,
//! send the mandatory `auth/hello` first frame, then one request/response over
//! the Content-Length-framed JSON-RPC the daemon speaks. It is deliberately
//! stateless (a fresh connection per call): the daemon may restart, the token
//! may only appear once it boots, and the web surface's read/answer volume is
//! low, so a persistent multiplexed connection would be complexity with no
//! payoff. The frame shapes come from the pure `ainb-hangar-proto` crate so the
//! wire contract can never drift from the daemon.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use ainb_hangar_proto::auth;
use ainb_hangar_proto::events::AttentionRow;
use ainb_hangar_proto::snapshots::{
    AnswerParams, AnswerResult, AttentionListParams, AttentionListResult,
};
use ainb_hangar_proto::{RpcId, RpcRequest, RpcResponse, methods};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedReadHalf;

/// How long a single dial + round-trip may take before the web surface gives up
/// and degrades (needs → empty, answer → error). The daemon is a local unix
/// socket, so this is generous; it only guards against a wedged daemon.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// A failure talking to the hangar daemon. Every variant is non-fatal to the
/// web surface: `needs` degrades to an empty list and `/api/answer` returns a
/// JSON error envelope, so a down daemon never takes the dashboard down.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// The hangar home / socket path could not be resolved from the environment.
    #[error("hangar home not resolvable (is $AINB_HANGAR_HOME / $HOME set?)")]
    NoHome,
    /// The daemon token file is missing or unreadable (daemon not running yet).
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

/// The daemon unix socket path (`{hangar_home}/hangar.sock`) — the same target
/// the TUI plugin dials. `None` when the home cannot be resolved.
#[must_use]
pub fn socket_path() -> Option<PathBuf> {
    Some(ainb_hangar_core::hangar_home()?.join("hangar.sock"))
}

/// A stateless client for the daemon control plane. Cheap to clone (just a path
/// + the plaintext token); every call opens a fresh connection.
#[derive(Debug, Clone)]
pub struct DaemonClient {
    socket: PathBuf,
    token: String,
}

impl DaemonClient {
    /// Resolve the socket + token from the environment (the daemon writes the
    /// plaintext token to `{hangar_home}/hangar/daemon.token` at boot). Returns
    /// an error when the home is unresolvable or the token file is absent — both
    /// of which the caller treats as "daemon not available" and degrades.
    pub fn from_env() -> Result<Self, DaemonError> {
        let socket = socket_path().ok_or(DaemonError::NoHome)?;
        let token_path = auth::default_token_file().ok_or(DaemonError::NoHome)?;
        let token = std::fs::read_to_string(&token_path)
            .map_err(|e| DaemonError::Token(e.to_string()))?
            .trim()
            .to_string();
        Ok(Self { socket, token })
    }

    /// Construct from explicit parts (the test seam — point it at a fake socket
    /// server with a known token).
    #[must_use]
    pub fn with_parts(socket: PathBuf, token: String) -> Self {
        Self { socket, token }
    }

    /// Snapshot the OPEN fleet-wide attention inbox (`attention/list`,
    /// `fleet = true`) — every session's open input request across every
    /// workspace plus the no-workspace host sessions, oldest-first.
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
    /// first-answer-wins + C1 ambiguity guards and performs the verified
    /// last-mile send; the tagged [`AnswerResult`] says what happened.
    pub async fn answer(&self, params: AnswerParams) -> Result<AnswerResult, DaemonError> {
        let value = serde_json::to_value(params).expect("AnswerParams serializes");
        let result = self.call(methods::ATTENTION_ANSWER, value).await?;
        serde_json::from_value(result).map_err(|e| DaemonError::Decode(e.to_string()))
    }

    /// Dial, authenticate, issue one RPC, and return its `result` value. Wraps
    /// the whole exchange in [`RPC_TIMEOUT`].
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

        // First frame MUST be auth/hello or the daemon closes the connection.
        write_frame(&mut writer, methods::AUTH_HELLO, json!({ "token": self.token }), 1).await?;
        let hello = read_response(&mut reader).await?;
        if let Some(err) = hello.error {
            return Err(DaemonError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        // The real call.
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
}

/// Write one Content-Length-framed JSON-RPC request.
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

/// Read the next id-bearing [`RpcResponse`], skipping any interleaved
/// notification frames (event pushes carry no `id`).
async fn read_response(reader: &mut BufReader<OwnedReadHalf>) -> Result<RpcResponse, DaemonError> {
    loop {
        let frame = read_frame(reader).await?;
        // A notification (no `id`) is an event push; skip it and keep reading
        // for the response to our request.
        if frame.get("id").is_some() {
            return serde_json::from_value(frame).map_err(|e| DaemonError::Decode(e.to_string()));
        }
    }
}

/// Read one Content-Length-framed JSON object off the socket.
async fn read_frame(reader: &mut BufReader<OwnedReadHalf>) -> Result<Value, DaemonError> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.map_err(|e| DaemonError::Io(e.to_string()))?;
        if n == 0 {
            return Err(DaemonError::Io("connection closed while awaiting a frame".to_string()));
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            // End of headers — read the body.
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

// ── needs mapping ────────────────────────────────────────────────────────────

/// Map an [`AttentionRow`] wire `kind` token to the short display kind the
/// dashboard's needs cards already colour on (ASK / ERR / WAIT). Answerable
/// request families (question / approval / codex-request / escalation) render as
/// `ASK` so they get the answer buttons; errors and waits render as their own
/// non-answerable badges.
#[must_use]
pub fn display_kind(wire_kind: &str) -> &'static str {
    match wire_kind {
        "error" => "ERR",
        "waiting" => "WAIT",
        // ask_user_question | approval | codex_request_user | escalation | anything else
        _ => "ASK",
    }
}

/// Convert the daemon's open attention rows into the JSON `needs` array the
/// dashboard renders. Each card carries `attentionId` (the answer RPC target),
/// its display `kind`, the raising session + cwd, and the normalised request
/// `payload` so the frontend can render the question/options and wire answer
/// buttons. Ordering is preserved (the daemon returns oldest-first, which is the
/// urgency order the card shuffle relies on).
///
/// The `payload` field is best-effort JSON: the daemon stores it as a serialised
/// string, so a parseable payload is normalised (see [`normalize_payload`]) into
/// the flat shape the dashboard renders and an unparseable one falls back to the
/// raw string — the card always has something to show.
#[must_use]
pub fn attention_to_needs(rows: &[AttentionRow]) -> Value {
    let cards: Vec<Value> = rows
        .iter()
        .map(|row| {
            let payload = normalize_payload(&row.payload);
            json!({
                "attentionId": row.id,
                "kind": display_kind(&row.kind),
                "wireKind": row.kind,
                "sessionId": row.session_id,
                "cwd": row.cwd,
                "workspaceId": row.workspace_id,
                "degraded": row.degraded,
                "createdAt": row.created_at,
                "payload": payload,
                // The push channels resolved at raise time (tcp T5). The web-push
                // delivery loop filters on this: it buzzes a device only when the
                // rules routed this attention to the `web` channel.
                "channels": row.channels,
            })
        })
        .collect();
    Value::Array(cards)
}

/// Normalise one stored attention payload into the flat card shape the dashboard
/// renders (`{ question | marker | snippet | …, options: [String] }`).
///
/// The daemon stores the request *context* verbatim, and a real ingest wraps the
/// request fields under a `context` object with `AskUserQuestion` options as
/// `{ "label": "…" }` objects (the canonical shape the TUI control centre and the
/// acceptance fixtures use). The dashboard's card renderer, by contrast, reads
/// the detail fields (`question`, `marker`, …) and the option *strings* at the
/// TOP level. Without this bridge a real ASK card renders its title but no
/// question and — fatally — no answer buttons, because `payload.options` is
/// absent. So the mapping boundary flattens a `context` wrapper up one level and
/// converts each option to its label string, while leaving an already-flat
/// payload (or a raw non-object string) untouched.
fn normalize_payload(raw: &str) -> Value {
    let parsed = match serde_json::from_str::<Value>(raw) {
        Ok(v) => v,
        // Not JSON — surface the raw string so the card still shows something.
        Err(_) => return json!(raw),
    };

    // Unwrap a `context` wrapper when present; otherwise normalise in place.
    let base = match parsed.get("context") {
        Some(Value::Object(_)) => parsed.get("context").cloned().unwrap_or(parsed),
        _ => parsed,
    };

    // Only object payloads carry render fields; anything else passes through.
    let Value::Object(mut map) = base else {
        return base;
    };

    // Convert `options` to an array of label strings: accept `{ "label": "x" }`
    // objects (canonical) OR bare strings (already-flat test/legacy payloads).
    if let Some(Value::Array(opts)) = map.get("options") {
        let labels: Vec<Value> = opts
            .iter()
            .filter_map(|opt| match opt {
                Value::Object(o) => o.get("label").and_then(Value::as_str).map(|s| json!(s)),
                Value::String(s) => Some(json!(s)),
                _ => None,
            })
            .collect();
        map.insert("options".to_string(), Value::Array(labels));
    }

    Value::Object(map)
}

// ── answer seam ──────────────────────────────────────────────────────────────

/// The `/api/answer` route's dependency: route an answer through the daemon.
/// Abstracted so route tests can inject a deterministic fake instead of dialling
/// a real socket, mirroring the [`crate::data::DataSource`] seam.
pub trait Answerer: Send + Sync + 'static {
    /// Deliver `params` to the daemon's `attention/answer` RPC.
    fn answer(
        &self,
        params: AnswerParams,
    ) -> Pin<Box<dyn Future<Output = Result<AnswerResult, DaemonError>> + Send + '_>>;
}

/// Production [`Answerer`]: resolves a fresh [`DaemonClient`] from the
/// environment per call (so it picks up a daemon that started after the web
/// server did) and forwards the answer.
#[derive(Debug, Clone, Default)]
pub struct DaemonAnswerer;

impl Answerer for DaemonAnswerer {
    fn answer(
        &self,
        params: AnswerParams,
    ) -> Pin<Box<dyn Future<Output = Result<AnswerResult, DaemonError>> + Send + '_>> {
        Box::pin(async move {
            let client = DaemonClient::from_env()?;
            client.answer(params).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, kind: &str, payload: &str) -> AttentionRow {
        AttentionRow {
            id: id.to_string(),
            session_id: format!("sess-{id}"),
            cwd: format!("/work/{id}"),
            workspace_id: None,
            kind: kind.to_string(),
            payload: payload.to_string(),
            degraded: false,
            created_at: 1000,
            channels: ainb_hangar_proto::ChannelSet::NONE,
        }
    }

    #[test]
    fn display_kind_maps_answerable_families_to_ask() {
        assert_eq!(display_kind("ask_user_question"), "ASK");
        assert_eq!(display_kind("approval"), "ASK");
        assert_eq!(display_kind("codex_request_user"), "ASK");
        assert_eq!(display_kind("escalation"), "ASK");
        assert_eq!(display_kind("error"), "ERR");
        assert_eq!(display_kind("waiting"), "WAIT");
    }

    #[test]
    fn attention_to_needs_carries_answer_id_and_display_kind() {
        let rows = vec![
            row("a1", "ask_user_question", r#"{"question":"pick one","options":["x","y"]}"#),
            row("e1", "error", "boom"),
        ];
        let needs = attention_to_needs(&rows);
        let arr = needs.as_array().expect("needs is an array");
        assert_eq!(arr.len(), 2);

        // Ordering preserved (oldest-first urgency order from the daemon).
        assert_eq!(arr[0]["attentionId"], "a1");
        assert_eq!(arr[0]["kind"], "ASK");
        assert_eq!(arr[0]["wireKind"], "ask_user_question");
        // A parseable payload is embedded as structured JSON the card can render.
        assert_eq!(arr[0]["payload"]["question"], "pick one");
        assert_eq!(arr[0]["payload"]["options"][1], "y");

        assert_eq!(arr[1]["attentionId"], "e1");
        assert_eq!(arr[1]["kind"], "ERR");
        // An unparseable payload falls back to the raw string, never dropped.
        assert_eq!(arr[1]["payload"], "boom");
    }

    #[test]
    fn attention_to_needs_flattens_canonical_context_ask_payload() {
        // The canonical stored shape (real ingest + acceptance fixtures): the
        // request fields nested under `context`, with options as `{label}`
        // objects. The card renderer reads the detail + option STRINGS at the top
        // level, so the mapping must flatten the wrapper and unwrap the labels —
        // otherwise the ASK renders no question and no answer buttons.
        let payload = r#"{"kind":"ASK","context":{"question":"Ship to which env?","options":[{"label":"staging"},{"label":"prod"},{"label":"canary"}]}}"#;
        let rows = vec![row("att-ask-1", "ask_user_question", payload)];
        let needs = attention_to_needs(&rows);
        let card = &needs.as_array().expect("needs is an array")[0];

        assert_eq!(card["kind"], "ASK");
        // Detail is lifted out of the `context` wrapper to the top level.
        assert_eq!(card["payload"]["question"], "Ship to which env?");
        // Options are flattened to their label strings, in order — this is what
        // the frontend turns into the "1. staging" / "2. prod" answer buttons.
        assert_eq!(card["payload"]["options"][0], "staging");
        assert_eq!(card["payload"]["options"][1], "prod");
        assert_eq!(card["payload"]["options"][2], "canary");
    }

    #[test]
    fn attention_to_needs_flattens_canonical_wait_marker() {
        // A WAIT row nests its marker the same way; flattening keeps the
        // dashboard's `needDetail` (which reads `payload.marker`) working.
        let payload = r#"{"kind":"WAIT","context":{"marker":"WAITING: still idle"}}"#;
        let rows = vec![row("att-wait-1", "waiting", payload)];
        let needs = attention_to_needs(&rows);
        let card = &needs.as_array().expect("needs is an array")[0];
        assert_eq!(card["kind"], "WAIT");
        assert_eq!(card["payload"]["marker"], "WAITING: still idle");
    }

    #[test]
    fn attention_to_needs_empty_is_empty_array() {
        assert_eq!(attention_to_needs(&[]), json!([]));
    }
}
