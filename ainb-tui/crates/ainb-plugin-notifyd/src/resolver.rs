//! OS-channel resolution for notifyd (tcp T5, agents-in-a-box-fyq).
//!
//! notifyd fires OS notifications on the raw hook-envelope SOCKET path, but the
//! authoritative per-attention [`ChannelSet`] is resolved by the SEPARATE hangar
//! daemon at attention-raise time (the `notify_rule` rules). This module lets
//! notifyd HONOUR that routing without ever touching the hangar STORE: it dials
//! the daemon's PUBLIC `hangar/notify_rules_list` RPC for the GLOBAL rules (hook
//! sessions are host-wide → `workspace_id = None`), maps the envelope's raw event
//! to an attention kind, and returns that kind's channel set.
//!
//! # Fail-open by construction
//!
//! No hangar home, no daemon token, a connect / auth / decode failure, a timeout,
//! or an unmapped event all yield [`ChannelResolution::Unknown`], and the caller
//! ([`crate::osnotify::notify`]) then notifies exactly as a plain notifyd-only
//! install always has. The daemon is CONSULTED, never DEPENDED on — notifyd stays
//! independent and daemon-down-safe. An OS notification is a best-effort side
//! effect (the board always shows the row), so a rule edit racing the resolve is
//! tolerable: worst case is one over- or under-fired local heads-up.
//!
//! # Kind fidelity
//!
//! notifyd sees only the raw hook event, not the transcript the hangar daemon
//! classifies against, so the ask-vs-idle split it cannot see falls to `waiting`
//! (the finished / idle case, whose board-only default is exactly what the Os gate
//! must honour). A bare `Notification` — Claude asking for input — maps to
//! `ask_user_question` instead, whose Os-included default keeps a real ask from
//! being false-suppressed.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use ainb_hangar_core::channel::ChannelSet;
use ainb_hangar_core::paths::hangar_home;
use ainb_hangar_proto::snapshots::{NotifyRulesListParams, NotifyRulesListResult};
use ainb_hangar_proto::{RpcId, RpcRequest, RpcResponse, auth, jsonrpc_version, methods};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::envelope::Envelope;
use crate::osnotify::{ChannelResolution, ChannelResolver};

/// Upper bound on the whole dial + auth + one round-trip. A local unix socket, so
/// generous; a slow / wedged daemon degrades to a fail-open notify rather than
/// stalling the connection handler.
const RPC_TIMEOUT: Duration = Duration::from_secs(2);

/// Resolves an envelope's OS-channel decision by dialling the hangar daemon's
/// public `notify_rules_list` RPC. Stateless: it re-resolves the socket + token
/// each call so a daemon that comes up (or restarts, rotating its token) is picked
/// up on the next notification with no cached staleness.
#[derive(Debug, Default, Clone, Copy)]
pub struct HangarRuleResolver;

impl HangarRuleResolver {
    /// Construct the production resolver.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ChannelResolver for HangarRuleResolver {
    fn resolve<'a>(
        &'a self,
        env: &'a Envelope,
    ) -> Pin<Box<dyn Future<Output = ChannelResolution> + Send + 'a>> {
        Box::pin(async move {
            let Some(kind) = attention_kind_token(&env.raw_event) else {
                return ChannelResolution::Unknown;
            };
            match resolve_kind(kind).await {
                Some(set) => ChannelResolution::Known(set),
                None => ChannelResolution::Unknown,
            }
        })
    }
}

/// Map a hook envelope's raw event to the attention-kind wire token the notify
/// rules are keyed on — notifyd's best-effort echo of the hangar daemon's
/// transcript classifier. `None` for a telemetry event (which never notifies).
///
/// A matcher suffix (`Notification:idle_prompt`) is honoured: an explicit idle
/// prompt is `waiting`, but a bare `Notification` (an ask) is `ask_user_question`
/// so it is never false-suppressed by a board-only `waiting` rule.
fn attention_kind_token(raw_event: &str) -> Option<&'static str> {
    let head = raw_event.split(':').next().unwrap_or(raw_event);
    // Approval / permission — always an Os-included default; check first.
    match head {
        "PermissionRequest"
        | "permission_request"
        | "exec_approval_request"
        | "apply_patch_approval_request" => return Some("approval"),
        "request_user_input" => return Some("codex_request_user"),
        _ => {}
    }
    // Idle / finished → waiting (the board-only default the Os gate honours): an
    // explicit idle prompt, an explicit wait, or a finished turn.
    if raw_event == "Notification:idle_prompt"
        || matches!(
            head,
            "wait_for_user" | "Stop" | "agentStop" | "agent-turn-complete" | "task_complete"
        )
    {
        return Some("waiting");
    }
    // A bare Notification is Claude asking for input — lean to notify.
    if matches!(head, "Notification" | "notification") {
        return Some("ask_user_question");
    }
    None
}

/// Dial the daemon and return the resolved global channel set for `kind`, or
/// `None` on ANY fault (fail-open). Bounded by [`RPC_TIMEOUT`].
async fn resolve_kind(kind: &str) -> Option<ChannelSet> {
    let socket = hangar_home()?.join("hangar.sock");
    let token = std::fs::read_to_string(auth::default_token_file()?).ok()?;
    let token = token.trim().to_string();
    let result = tokio::time::timeout(RPC_TIMEOUT, list_global_rules(&socket, &token))
        .await
        .ok()??;
    result.rules.into_iter().find(|r| r.kind == kind).map(|r| r.channels)
}

/// One dial → `auth/hello` → `notify_rules_list` (global scope) exchange. Returns
/// `None` on any framing / auth / decode fault. Split out (socket + token as
/// args) so a fake-socket integration test can drive the real wire client.
async fn list_global_rules(socket: &Path, token: &str) -> Option<NotifyRulesListResult> {
    let stream = UnixStream::connect(socket).await.ok()?;
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Mandatory auth handshake first (mirrors the daemon's server contract).
    write_frame(
        &mut writer,
        methods::AUTH_HELLO,
        serde_json::json!({ "token": token }),
        1,
    )
    .await?;
    if read_response(&mut reader).await?.error.is_some() {
        return None;
    }

    let params = serde_json::to_value(NotifyRulesListParams { workspace_id: None }).ok()?;
    write_frame(&mut writer, methods::HANGAR_NOTIFY_RULES_LIST, params, 2).await?;
    let resp = read_response(&mut reader).await?;
    if resp.error.is_some() {
        return None;
    }
    serde_json::from_value(resp.result?).ok()
}

/// Write one `Content-Length`-framed JSON-RPC request. `None` on any I/O fault.
async fn write_frame(
    writer: &mut (impl AsyncWriteExt + Unpin),
    method: &str,
    params: serde_json::Value,
    id: i64,
) -> Option<()> {
    let req = RpcRequest {
        jsonrpc: jsonrpc_version(),
        id: RpcId::Number(id),
        method: method.to_string(),
        params,
    };
    let body = serde_json::to_vec(&req).ok()?;
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    writer.write_all(&out).await.ok()?;
    writer.flush().await.ok()?;
    Some(())
}

/// Read frames until one carries an `id` (a response, not a broadcast), and decode
/// it. `None` on EOF / I/O / decode fault.
async fn read_response(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Option<RpcResponse> {
    loop {
        let frame = read_frame(reader).await?;
        if frame.get("id").is_some() {
            return serde_json::from_value(frame).ok();
        }
    }
}

/// Read one `Content-Length`-framed JSON value. `None` on EOF / I/O / a missing
/// header / a decode fault.
async fn read_frame(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Option<serde_json::Value> {
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None; // connection closed
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let content_len = len?;
            let mut body = vec![0u8; content_len];
            reader.read_exact(&mut body).await.ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            len = rest.trim().parse().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_core::channel::Channel;
    use ainb_hangar_proto::snapshots::NotifyRuleWireRow;

    #[test]
    fn maps_raw_events_to_the_daemon_kind_tokens() {
        // Approval / permission family.
        for e in [
            "PermissionRequest",
            "permission_request",
            "exec_approval_request",
            "apply_patch_approval_request",
        ] {
            assert_eq!(attention_kind_token(e), Some("approval"), "{e}");
        }
        assert_eq!(
            attention_kind_token("request_user_input"),
            Some("codex_request_user")
        );
        // Idle / finished → waiting (the board-only case the Os gate honours).
        for e in [
            "Notification:idle_prompt",
            "wait_for_user",
            "Stop",
            "agentStop",
            "agent-turn-complete",
            "task_complete",
        ] {
            assert_eq!(attention_kind_token(e), Some("waiting"), "{e}");
        }
        // A bare Notification (an ask) leans to notify — never false-suppressed.
        assert_eq!(
            attention_kind_token("Notification"),
            Some("ask_user_question")
        );
        assert_eq!(
            attention_kind_token("notification"),
            Some("ask_user_question")
        );
        // Telemetry → no kind (never notifies).
        for e in ["PostToolUse", "SessionStart", "UserPromptSubmit", ""] {
            assert_eq!(attention_kind_token(e), None, "{e}");
        }
    }

    /// A minimal fake daemon over a real unix socket: answers `auth/hello` OK,
    /// then `notify_rules_list` with `rules`. Proves the hand-rolled wire client
    /// speaks the daemon's framing end-to-end.
    async fn serve_once(socket: std::path::PathBuf, rules: Vec<NotifyRuleWireRow>) {
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut writer) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            // auth/hello → ack.
            let hello = read_frame(&mut reader).await.unwrap();
            let id = hello.get("id").cloned().unwrap();
            reply(&mut writer, &id, serde_json::json!({ "ok": true })).await;
            // notify_rules_list → the rules.
            let req = read_frame(&mut reader).await.unwrap();
            let id = req.get("id").cloned().unwrap();
            let result = serde_json::to_value(NotifyRulesListResult {
                rules,
                workspace_id: None,
            })
            .unwrap();
            reply(&mut writer, &id, result).await;
        });
    }

    async fn reply(
        writer: &mut (impl AsyncWriteExt + Unpin),
        id: &serde_json::Value,
        result: serde_json::Value,
    ) {
        let resp = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result });
        let body = serde_json::to_vec(&resp).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        writer.write_all(&out).await.unwrap();
        writer.flush().await.unwrap();
    }

    #[tokio::test]
    async fn wire_client_reads_the_kind_channel_set_from_a_fake_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("hangar.sock");
        serve_once(
            socket.clone(),
            vec![
                NotifyRuleWireRow {
                    kind: "waiting".to_string(),
                    channels: ChannelSet::NONE,
                    overridden: false,
                },
                NotifyRuleWireRow {
                    kind: "ask_user_question".to_string(),
                    channels: ChannelSet::from_channels([Channel::Web, Channel::Os]),
                    overridden: false,
                },
            ],
        )
        .await;

        let result = list_global_rules(&socket, "tok").await.expect("rules");
        let waiting = result.rules.iter().find(|r| r.kind == "waiting").unwrap();
        assert!(
            !waiting.channels.contains(Channel::Os),
            "board-only waiting excludes Os"
        );
        let ask = result.rules.iter().find(|r| r.kind == "ask_user_question").unwrap();
        assert!(ask.channels.contains(Channel::Os), "ask includes Os");
    }

    #[tokio::test]
    async fn dial_to_a_dead_socket_fails_open() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("nope.sock");
        assert!(
            list_global_rules(&socket, "tok").await.is_none(),
            "no daemon → None → caller fails open"
        );
    }
}
