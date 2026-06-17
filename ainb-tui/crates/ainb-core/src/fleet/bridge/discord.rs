// ABOUTME: The Discord channel — raw Gateway WebSocket, two-way relay.
//
// Flow: connect to the Discord Gateway (`wss://gateway.discord.gg/?v=10&
// encoding=json`) with tokio-tungstenite, complete the documented handshake
// (receive HELLO → start a heartbeat loop → IDENTIFY), receive `MESSAGE_CREATE`
// dispatches, run authorized messages through the SHARED relay core, and post
// the reply via the REST `POST /channels/{id}/messages` (Bot token).
//
// Parity with the Telegram + Slack channels' policy:
//   * Authorization by Discord user id (unknown senders silently ignored).
//   * The bot's own messages (and other bots) are ignored to avoid reply loops.
//   * Routing honours a leading `name:` prefix (shared `relay` core), else the
//     configured `default_target`.
//   * Replies are split at Discord's 2000-char limit; markdown passes through
//     verbatim (Discord renders **bold**/`code`/```fences``` natively).
//
// We use the RAW gateway rather than a heavyweight SDK (serenity/twilight) to
// match slack.rs's lightweight `tokio_tungstenite` + `reqwest` style and to
// avoid pulling a large dependency tree into the single `ainb` binary. The
// gateway surface the bridge needs is small: HELLO/heartbeat/IDENTIFY plus the
// MESSAGE_CREATE dispatch, all hand-rolled here.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use super::config::DiscordConfig;
use super::redact::scrub_token;
use super::relay::{FleetTransport, RelayParams, relay};

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
const API_BASE: &str = "https://discord.com/api/v10";

/// Discord message-content limit (`POST /channels/{id}/messages`). We split below it.
const DISCORD_MAX_LENGTH: usize = 2000;

/// Gateway opcodes we handle (subset of the documented set).
mod op {
    pub const DISPATCH: u64 = 0;
    pub const HEARTBEAT: u64 = 1;
    pub const IDENTIFY: u64 = 2;
    pub const RECONNECT: u64 = 7;
    pub const INVALID_SESSION: u64 = 9;
    pub const HELLO: u64 = 10;
    pub const HEARTBEAT_ACK: u64 = 11;
}

/// Gateway intents: `GUILD_MESSAGES` | `MESSAGE_CONTENT` | `DIRECT_MESSAGES`.
/// `(1 << 9) | (1 << 15) | (1 << 12)` = 512 | 32768 | 4096.
const INTENTS: u64 = (1 << 9) | (1 << 15) | (1 << 12);

/// Run the Discord channel forever (reconnecting on disconnect).
pub async fn run<T: FleetTransport>(cfg: DiscordConfig, transport: &T) -> Result<()> {
    let client =
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| {
                anyhow::anyhow!(
                    "building Discord HTTP client: {}",
                    scrub_token(&e.to_string())
                )
            })?;

    tracing::info!(
        authorized_user = %cfg.authorized_user_id,
        "ainb phone bridge: Discord channel online (gateway)"
    );

    loop {
        match connect_and_serve(&client, &cfg, transport).await {
            Ok(()) => tracing::info!("Discord gateway closed; reconnecting"),
            Err(e) => {
                // Build the diagnostic from the error's text, then scrub any
                // token-shaped substring so a Bot token can never leak into logs.
                tracing::warn!(
                    error = %scrub_token(&e.to_string()),
                    "Discord gateway error; reconnecting in 5s"
                );
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// One gateway connection: handshake, then pump dispatches until the socket
/// closes or Discord asks us to reconnect.
async fn connect_and_serve<T: FleetTransport>(
    client: &reqwest::Client,
    cfg: &DiscordConfig,
    transport: &T,
) -> Result<()> {
    let (ws, _resp) = tokio_tungstenite::connect_async(GATEWAY_URL)
        .await
        .context("connecting to Discord gateway WSS")?;
    let (mut write, mut read) = ws.split();

    // ── HELLO (op 10) — carries the heartbeat interval ──────────────────────
    let hello = next_payload(&mut read).await.context("waiting for Discord HELLO")?;
    if hello.op != op::HELLO {
        anyhow::bail!("expected HELLO (op 10), got op {}", hello.op);
    }
    let heartbeat_ms = hello
        .d
        .as_ref()
        .and_then(|d| d.get("heartbeat_interval"))
        .and_then(Value::as_u64)
        .context("HELLO missing heartbeat_interval")?;

    // ── IDENTIFY (op 2) ─────────────────────────────────────────────────────
    let identify = json!({
        "op": op::IDENTIFY,
        "d": {
            "token": cfg.token,
            "intents": INTENTS,
            "properties": { "os": "linux", "browser": "ainb", "device": "ainb" }
        }
    });
    write
        .send(Message::Text(identify.to_string()))
        .await
        .context("sending Discord IDENTIFY")?;

    // Last received sequence number, shared with the heartbeat loop.
    let last_seq = Arc::new(AtomicU64::new(0));
    let mut heartbeat = tokio::time::interval(Duration::from_millis(heartbeat_ms));
    // The first tick fires immediately; skip it so we don't double-send before
    // the gateway is ready (Discord tolerates an early beat, but this is tidy).
    heartbeat.tick().await;

    loop {
        tokio::select! {
            // Periodic heartbeat (op 1 with the last seq, or null if none yet).
            _ = heartbeat.tick() => {
                let seq = last_seq.load(Ordering::Relaxed);
                let d = if seq == 0 { Value::Null } else { json!(seq) };
                let beat = json!({ "op": op::HEARTBEAT, "d": d }).to_string();
                if write.send(Message::Text(beat)).await.is_err() {
                    break; // socket gone — outer loop reconnects
                }
            }
            frame = read.next() => {
                let Some(frame) = frame else { break };
                let payload = match frame.context("reading Discord gateway frame")? {
                    Message::Text(t) => match serde_json::from_str::<GatewayPayload>(&t) {
                        Ok(p) => p,
                        Err(_) => continue,
                    },
                    Message::Ping(p) => { write.send(Message::Pong(p)).await.ok(); continue; }
                    Message::Close(_) => break,
                    _ => continue,
                };

                if let Some(s) = payload.s {
                    last_seq.store(s, Ordering::Relaxed);
                }

                match payload.op {
                    op::HEARTBEAT => {
                        // Server asked for an immediate beat.
                        let seq = last_seq.load(Ordering::Relaxed);
                        let d = if seq == 0 { Value::Null } else { json!(seq) };
                        let beat = json!({ "op": op::HEARTBEAT, "d": d }).to_string();
                        write.send(Message::Text(beat)).await.ok();
                    }
                    op::HEARTBEAT_ACK => tracing::trace!("Discord heartbeat ack"),
                    op::RECONNECT => {
                        tracing::info!("Discord requested reconnect");
                        break;
                    }
                    op::INVALID_SESSION => {
                        // We never persist a session for RESUME, so a fresh
                        // reconnect (new IDENTIFY) is the correct minimal handling.
                        tracing::info!("Discord invalidated session; reconnecting fresh");
                        break;
                    }
                    op::DISPATCH => {
                        if payload.t.as_deref() == Some("MESSAGE_CREATE") {
                            if let Some(d) = payload.d {
                                if let Ok(msg) = serde_json::from_value::<DiscordMessage>(d) {
                                    handle_message(client, cfg, transport, msg).await;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Read frames until the next JSON gateway payload (skipping ping/pong/binary).
async fn next_payload<S>(read: &mut S) -> Result<GatewayPayload>
where
    S: StreamExt<Item = tokio_tungstenite::tungstenite::Result<Message>> + Unpin,
{
    while let Some(frame) = read.next().await {
        match frame.context("reading Discord gateway frame")? {
            Message::Text(t) => {
                if let Ok(p) = serde_json::from_str::<GatewayPayload>(&t) {
                    return Ok(p);
                }
            }
            Message::Close(_) => anyhow::bail!("gateway closed before payload"),
            _ => {}
        }
    }
    anyhow::bail!("gateway stream ended before a payload")
}

/// Process one `MESSAGE_CREATE`: authorize, relay through the shared core, and
/// post the reply back to the originating (or configured) channel.
async fn handle_message<T: FleetTransport>(
    client: &reqwest::Client,
    cfg: &DiscordConfig,
    transport: &T,
    msg: DiscordMessage,
) {
    if !authorize(cfg, &msg) {
        return;
    }

    let text = msg.content.trim();
    if text.is_empty() {
        return;
    }

    let params = RelayParams {
        default_target: cfg.default_target.as_deref(),
        response_timeout: Duration::from_secs(cfg.response_timeout),
    };
    let reply = relay(transport, &params, text).await;

    // Reply to the message's channel; fall back to the configured channel_id.
    let channel = msg.channel_id.as_deref().or(cfg.channel_id.as_deref());
    let Some(channel) = channel else {
        tracing::warn!(
            "Discord reply has no channel (no originating channel_id and no configured channel_id)"
        );
        return;
    };

    for chunk in split_for_discord(&reply) {
        if let Err(e) = post_message(client, &cfg.token, channel, &chunk).await {
            // Scrub before logging: the error may echo the Authorization header.
            tracing::warn!(error = %scrub_token(&e.to_string()), "Discord post message failed");
        }
    }
}

/// Decide whether to act on a message. Returns `true` to act, `false` to skip.
/// Pure — exercised directly in tests.
///
/// Skips the bot's own posts and other bots (`author.bot`), and any author whose
/// id is not the single authorized user.
fn authorize(cfg: &DiscordConfig, msg: &DiscordMessage) -> bool {
    let Some(author) = msg.author.as_ref() else {
        return false;
    };
    // Ignore bot authors (including ourselves) to avoid reply loops.
    if author.bot.unwrap_or(false) {
        return false;
    }
    if author.id != cfg.authorized_user_id {
        tracing::warn!(user = %author.id, "ignoring Discord message from unauthorized user");
        return false;
    }
    true
}

/// Split a reply for Discord's message-length limit on newline boundaries (with
/// a hard char cut for oversized lines). Markdown passes through (Discord renders
/// it natively).
fn split_for_discord(text: &str) -> Vec<String> {
    super::format::split_message(text, DISCORD_MAX_LENGTH)
}

/// Post a message to a channel via the REST API (`Authorization: Bot <token>`).
async fn post_message(
    client: &reqwest::Client,
    token: &str,
    channel_id: &str,
    text: &str,
) -> Result<()> {
    let resp = client
        .post(format!("{API_BASE}/channels/{channel_id}/messages"))
        .header("Authorization", format!("Bot {token}"))
        .json(&json!({ "content": text }))
        .send()
        .await
        .context("POST /channels/{id}/messages request")?;
    let status = resp.status();
    if !status.is_success() {
        // Surface status + the API error code/message, never the token.
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let code = body.get("code").and_then(Value::as_i64);
        let message = body.get("message").and_then(Value::as_str).unwrap_or("");
        anyhow::bail!(
            "Discord message post failed: HTTP {} (code {:?}: {})",
            status.as_u16(),
            code,
            message
        );
    }
    Ok(())
}

// ── Gateway / REST wire types ───────────────────────────────────────────────

/// A gateway frame: `{ op, d, s, t }`. `d`/`s`/`t` are present only on some ops.
#[derive(Debug, Deserialize)]
struct GatewayPayload {
    op: u64,
    #[serde(default)]
    d: Option<Value>,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

/// A `MESSAGE_CREATE` dispatch payload (the fields the bridge reads).
#[derive(Debug, Default, Deserialize)]
struct DiscordMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    author: Option<DiscordAuthor>,
}

#[derive(Debug, Default, Deserialize)]
struct DiscordAuthor {
    #[serde(default)]
    id: String,
    #[serde(default)]
    bot: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DiscordConfig {
        DiscordConfig {
            token: "bot-token".into(),
            authorized_user_id: "123456789".into(),
            default_target: None,
            channel_id: None,
            response_timeout: 300,
        }
    }

    fn msg(user_id: &str) -> DiscordMessage {
        DiscordMessage {
            content: "hello".into(),
            channel_id: Some("C1".into()),
            author: Some(DiscordAuthor {
                id: user_id.into(),
                bot: None,
            }),
        }
    }

    #[test]
    fn authorizes_the_configured_user() {
        assert!(authorize(&cfg(), &msg("123456789")));
    }

    #[test]
    fn ignores_unauthorized_user() {
        assert!(!authorize(&cfg(), &msg("999")));
    }

    #[test]
    fn ignores_bot_authors() {
        // Even the authorized id is ignored if the message is flagged as a bot
        // post (this is how the bot's own messages are filtered — no reply loop).
        let mut m = msg("123456789");
        m.author.as_mut().unwrap().bot = Some(true);
        assert!(!authorize(&cfg(), &m));
    }

    #[test]
    fn ignores_message_with_no_author() {
        let mut m = msg("123456789");
        m.author = None;
        assert!(!authorize(&cfg(), &m));
    }

    #[test]
    fn intents_cover_guild_messages_message_content_and_dms() {
        // GUILD_MESSAGES (1<<9), DIRECT_MESSAGES (1<<12), MESSAGE_CONTENT (1<<15).
        assert_eq!(INTENTS & (1 << 9), 1 << 9);
        assert_eq!(INTENTS & (1 << 12), 1 << 12);
        assert_eq!(INTENTS & (1 << 15), 1 << 15);
    }

    #[test]
    fn split_respects_discord_limit() {
        let big = "x".repeat(5000);
        for chunk in split_for_discord(&big) {
            assert!(chunk.chars().count() <= DISCORD_MAX_LENGTH);
        }
    }

    #[test]
    fn parses_hello_payload() {
        let raw = r#"{"op":10,"d":{"heartbeat_interval":41250},"s":null,"t":null}"#;
        let p: GatewayPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(p.op, op::HELLO);
        assert_eq!(
            p.d.unwrap().get("heartbeat_interval").and_then(Value::as_u64),
            Some(41250)
        );
    }

    #[test]
    fn parses_message_create_dispatch() {
        let raw = r#"{
            "op":0,"s":42,"t":"MESSAGE_CREATE",
            "d":{"content":"backend: run tests","channel_id":"C9","author":{"id":"123456789","bot":false}}
        }"#;
        let p: GatewayPayload = serde_json::from_str(raw).unwrap();
        assert_eq!(p.op, op::DISPATCH);
        assert_eq!(p.s, Some(42));
        assert_eq!(p.t.as_deref(), Some("MESSAGE_CREATE"));
        let m: DiscordMessage = serde_json::from_value(p.d.unwrap()).unwrap();
        assert_eq!(m.content, "backend: run tests");
        assert_eq!(m.channel_id.as_deref(), Some("C9"));
        assert_eq!(m.author.as_ref().unwrap().id, "123456789");
        assert_eq!(m.author.as_ref().unwrap().bot, Some(false));
    }

    #[test]
    fn message_without_bot_flag_defaults_to_human() {
        // `author.bot` absent → treated as a human author (not filtered).
        let raw = r#"{"content":"hi","channel_id":"C1","author":{"id":"123456789"}}"#;
        let m: DiscordMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(m.author.unwrap().bot, None);
    }

    // ── Relay-path tests over an in-memory mock FleetTransport ───────────────
    // These exercise the same shared `relay` core the live channel calls, so the
    // routing/degrade behaviour is verified without a live Discord connection.

    use std::sync::Mutex;

    use super::super::relay::FleetTransport;
    use super::super::routing::TargetSession;

    struct FakeTransport {
        sessions: Vec<TargetSession>,
        last_send: Mutex<Option<(String, String)>>,
        reply: Option<String>,
    }

    impl FakeTransport {
        fn new(sessions: Vec<TargetSession>, reply: Option<String>) -> Self {
            Self {
                sessions,
                last_send: Mutex::new(None),
                reply,
            }
        }
    }

    impl FleetTransport for FakeTransport {
        async fn discover(&self) -> Vec<TargetSession> {
            self.sessions.clone()
        }
        async fn send_and_capture(
            &self,
            session: &TargetSession,
            text: &str,
            _timeout: Duration,
        ) -> Option<String> {
            *self.last_send.lock().unwrap() = Some((session.name.clone(), text.to_string()));
            self.reply.clone()
        }
    }

    fn sess(name: &str) -> TargetSession {
        TargetSession::new(
            name,
            format!("tmux-{name}"),
            format!("/cwd/{name}"),
            format!("id-{name}"),
        )
    }

    fn relay_params() -> RelayParams<'static> {
        RelayParams {
            default_target: None,
            response_timeout: Duration::from_secs(300),
        }
    }

    #[tokio::test]
    async fn relay_routes_name_prefix_to_session() {
        let t = FakeTransport::new(vec![sess("backend"), sess("frontend")], Some("ok".into()));
        let out = relay(&t, &relay_params(), "backend: run tests").await;
        assert_eq!(out, "ok");
        let (target, msg) = t.last_send.lock().unwrap().clone().unwrap();
        assert_eq!(target, "backend");
        assert_eq!(msg, "run tests");
    }

    #[tokio::test]
    async fn relay_uses_configured_default_target() {
        let t = FakeTransport::new(vec![sess("alpha"), sess("beta")], Some("hi".into()));
        let p = RelayParams {
            default_target: Some("beta"),
            response_timeout: Duration::from_secs(5),
        };
        let out = relay(&t, &p, "do it").await;
        assert_eq!(out, "hi");
        assert_eq!(t.last_send.lock().unwrap().clone().unwrap().0, "beta");
    }

    #[tokio::test]
    async fn relay_reports_empty_fleet() {
        let t = FakeTransport::new(vec![], Some("x".into()));
        let out = relay(&t, &relay_params(), "hello").await;
        assert_eq!(out, "No running ainb sessions to relay to.");
    }
}
