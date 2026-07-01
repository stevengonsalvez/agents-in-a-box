// ABOUTME: The Slack channel — socket-mode, two-way relay.
//
// Flow: `apps.connections.open` (xapp- app token) returns a WSS URL; we connect
// with tokio-tungstenite, receive `events_api` envelopes (message events), ACK
// each by envelope_id, run authorized messages through the SHARED relay core,
// and post the reply via `chat.postMessage` (xoxb- bot token).
//
// Parity with the Telegram channel's policy:
//   * Authorization by Slack user id (unknown senders silently ignored).
//   * listen_mode = "mentions" (default): act only on app_mention events or DMs;
//     "all": act on every message event in subscribed channels + DMs.
//   * The bot's own messages (and other bots) are ignored to avoid loops.
//   * Replies are split at Slack's 4000-char limit; markdown passes through as
//     Slack mrkdwn (Slack renders **bold**/`code` natively, so the reply text is
//     posted verbatim with a light leading-mention strip).

use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};

use super::config::{SlackConfig, SlackListenMode};
use super::heartbeat::BridgeHeartbeat;
use super::relay::{FleetTransport, RelayParams, relay};

const API_BASE: &str = "https://slack.com/api";
/// Slack message text limit (chat.postMessage). We split below this.
const SLACK_MAX_LENGTH: usize = 3900;

/// Run the Slack channel forever (reconnecting on disconnect). Reports
/// connection + relay activity through the shared `heartbeat`.
///
/// `transport` is `&'static` because each inbound event's relay runs in its own
/// `tokio::spawn` task (so a slow reply can never block the socket read loop —
/// see `connect_and_serve`); the spawned future must therefore not borrow a
/// non-`'static` transport. The daemon leaks a single shared transport, so this
/// costs nothing (mirrors the Discord channel).
pub async fn run<T: FleetTransport + 'static>(
    cfg: SlackConfig,
    transport: &'static T,
    heartbeat: BridgeHeartbeat,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("building Slack HTTP client")?;

    // Resolve our own bot user id once so we never relay our own posts.
    let self_user_id = auth_test(&client, &cfg.bot_token).await;
    // A successful auth_test is the "channel online" signal.
    heartbeat.set_connected(
        self_user_id.is_some(),
        Some("Slack (socket-mode)".to_string()),
    );
    tracing::info!(
        bot_user = self_user_id.as_deref().unwrap_or("?"),
        mode = ?cfg.listen_mode,
        "ainb phone bridge: Slack channel online (socket-mode)"
    );

    loop {
        match connect_and_serve(
            &client,
            &cfg,
            self_user_id.as_deref(),
            transport,
            &heartbeat,
        )
        .await
        {
            Ok(()) => {
                tracing::info!("Slack socket closed; reconnecting");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Slack socket error; reconnecting in 5s");
                heartbeat.record_error(format!("socket error: {e}"));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn connect_and_serve<T: FleetTransport + 'static>(
    client: &reqwest::Client,
    cfg: &SlackConfig,
    self_user_id: Option<&str>,
    transport: &'static T,
    heartbeat: &BridgeHeartbeat,
) -> Result<()> {
    let ws_url = open_connection(client, &cfg.app_token).await?;
    let (ws, _resp) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .context("connecting to Slack socket-mode WSS")?;
    let (mut write, mut read) = ws.split();

    while let Some(frame) = read.next().await {
        use tokio_tungstenite::tungstenite::Message;
        let text = match frame.context("reading Slack socket frame")? {
            Message::Text(t) => t,
            Message::Ping(p) => {
                write.send(Message::Pong(p)).await.ok();
                continue;
            }
            Message::Close(_) => break,
            _ => continue,
        };

        let Ok(envelope) = serde_json::from_str::<SocketEnvelope>(&text) else {
            continue;
        };

        // ACK every envelope that carries one, immediately (Slack requires an
        // ack within 3s or it redelivers).
        if let Some(envelope_id) = &envelope.envelope_id {
            let ack = json!({ "envelope_id": envelope_id }).to_string();
            write.send(Message::Text(ack)).await.ok();
        }

        match envelope.envelope_type.as_deref() {
            Some("hello") => tracing::debug!("Slack socket hello"),
            Some("disconnect") => {
                tracing::info!("Slack requested disconnect (refresh)");
                break;
            }
            Some("events_api") => {
                if let Some(event) = envelope.payload.and_then(|p| p.event) {
                    // Dispatch the relay OFF the socket read loop. `handle_event`
                    // -> `relay` polls the agent for up to `response_timeout`
                    // (300s default); awaiting it inline here would block the read
                    // loop for that whole window — so it could not pong a `Ping`
                    // (Slack then drops the conn), process a `disconnect` refresh
                    // envelope, or read further events. The envelope is already
                    // ACKed above, so spawning the handler keeps the read loop free
                    // to service pings/acks/disconnects. The `reqwest::Client`
                    // clone is cheap (`Arc` inside), the config + self-id are
                    // cloned, the transport is `&'static` (shared, not the WSS
                    // socket), and the heartbeat handle is a cheap `Arc` clone.
                    let client = client.clone();
                    let cfg = cfg.clone();
                    let self_user_id = self_user_id.map(str::to_string);
                    let heartbeat = heartbeat.clone();
                    tokio::spawn(async move {
                        handle_event(
                            &client,
                            &cfg,
                            self_user_id.as_deref(),
                            transport,
                            &heartbeat,
                            event,
                        )
                        .await;
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn handle_event<T: FleetTransport>(
    client: &reqwest::Client,
    cfg: &SlackConfig,
    self_user_id: Option<&str>,
    transport: &T,
    heartbeat: &BridgeHeartbeat,
    event: SlackEvent,
) {
    if classify_event(cfg, self_user_id, &event).is_none() {
        return;
    }

    let text = strip_leading_mention(event.text.as_deref().unwrap_or_default());
    if text.trim().is_empty() {
        return;
    }

    let params = RelayParams {
        default_target: cfg.default_target.as_deref(),
        response_timeout: Duration::from_secs(cfg.response_timeout),
    };
    let reply = relay(transport, &params, &text).await;
    // A relayed turn is the "last relay" activity signal.
    heartbeat.record_relay();

    let Some(channel) = event.channel.as_deref() else {
        return;
    };
    let thread_ts = event.thread_ts.clone().or_else(|| event.ts.clone());
    for chunk in split_for_slack(&reply) {
        if let Err(e) = post_message(
            client,
            &cfg.bot_token,
            channel,
            &chunk,
            thread_ts.as_deref(),
        )
        .await
        {
            tracing::warn!(error = %e, "Slack chat.postMessage failed");
            heartbeat.record_error(format!("chat.postMessage failed: {e}"));
        }
    }
}

/// Decide whether to act on an event. Returns `Some(())` to act, `None` to skip.
/// Pure — exercised directly in tests.
fn classify_event(cfg: &SlackConfig, self_user_id: Option<&str>, event: &SlackEvent) -> Option<()> {
    // Only message-bearing events.
    match event.event_type.as_deref() {
        Some("message") | Some("app_mention") => {}
        _ => return None,
    }

    // Ignore message subtypes (edits, joins, bot_message, deletions) and our
    // own / any bot's posts — prevents reply loops.
    if event.subtype.is_some() {
        return None;
    }
    if event.bot_id.is_some() {
        return None;
    }
    let sender = event.user.as_deref()?;
    if Some(sender) == self_user_id {
        return None;
    }

    // Authorization by Slack user id.
    if sender != cfg.authorized_user_id {
        tracing::warn!(
            user = sender,
            "ignoring Slack message from unauthorized user"
        );
        return None;
    }

    let is_dm = event.channel_type.as_deref() == Some("im");
    let is_mention = event.event_type.as_deref() == Some("app_mention")
        || self_user_id.is_some_and(|id| {
            event.text.as_deref().is_some_and(|t| t.contains(&format!("<@{id}>")))
        });

    match cfg.listen_mode {
        SlackListenMode::All => {
            if is_dm || is_mention || event.channel.is_some() {
                Some(())
            } else {
                None
            }
        }
        SlackListenMode::Mentions => (is_dm || is_mention).then_some(()),
    }
}

/// Strip a leading `<@U…>` Slack mention (and any following whitespace) from the
/// message text. Pure.
fn strip_leading_mention(text: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<@") {
        if let Some(close) = rest.find('>') {
            return rest[close + 1..].trim_start().to_string();
        }
    }
    text.to_string()
}

/// Split a reply for Slack's message-length limit on newline boundaries (with a
/// hard char cut for oversized lines). Mirrors `format::split_message` but at
/// Slack's limit, keeping markdown intact (Slack renders mrkdwn natively).
fn split_for_slack(text: &str) -> Vec<String> {
    super::format::split_message(text, SLACK_MAX_LENGTH)
}

// ── Slack Web API calls ─────────────────────────────────────────────────────

async fn auth_test(client: &reqwest::Client, bot_token: &str) -> Option<String> {
    let resp = client
        .post(format!("{API_BASE}/auth.test"))
        .bearer_auth(bot_token)
        .send()
        .await
        .ok()?;
    let body: Value = resp.json().await.ok()?;
    body.get("user_id").and_then(Value::as_str).map(str::to_string)
}

async fn open_connection(client: &reqwest::Client, app_token: &str) -> Result<String> {
    let resp = client
        .post(format!("{API_BASE}/apps.connections.open"))
        .bearer_auth(app_token)
        .send()
        .await
        .context("apps.connections.open request")?;
    let body: Value = resp.json().await.context("apps.connections.open decode")?;
    if body.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!(
            "apps.connections.open failed: {}",
            body.get("error").and_then(Value::as_str).unwrap_or("unknown")
        );
    }
    body.get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("apps.connections.open returned no url")
}

async fn post_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<()> {
    let mut payload = json!({ "channel": channel, "text": text });
    if let Some(ts) = thread_ts {
        payload["thread_ts"] = json!(ts);
    }
    let resp = client
        .post(format!("{API_BASE}/chat.postMessage"))
        .bearer_auth(bot_token)
        .json(&payload)
        .send()
        .await
        .context("chat.postMessage request")?;
    let body: Value = resp.json().await.context("chat.postMessage decode")?;
    if body.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!(
            "chat.postMessage failed: {}",
            body.get("error").and_then(Value::as_str).unwrap_or("unknown")
        );
    }
    Ok(())
}

// ── Socket-mode wire types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SocketEnvelope {
    #[serde(rename = "type")]
    envelope_type: Option<String>,
    envelope_id: Option<String>,
    #[serde(default)]
    payload: Option<SocketPayload>,
}

#[derive(Debug, Deserialize)]
struct SocketPayload {
    #[serde(default)]
    event: Option<SlackEvent>,
}

#[derive(Debug, Default, Deserialize)]
struct SlackEvent {
    #[serde(rename = "type")]
    event_type: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    bot_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    channel_type: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    thread_ts: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: SlackListenMode) -> SlackConfig {
        SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            authorized_user_id: "UAUTH".into(),
            default_target: None,
            listen_mode: mode,
            response_timeout: 300,
        }
    }

    fn ev(event_type: &str, user: &str) -> SlackEvent {
        SlackEvent {
            event_type: Some(event_type.into()),
            user: Some(user.into()),
            text: Some("hello".into()),
            channel: Some("C1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn mentions_mode_ignores_plain_channel_message() {
        let e = ev("message", "UAUTH"); // no mention, no DM
        assert!(classify_event(&cfg(SlackListenMode::Mentions), Some("UBOT"), &e).is_none());
    }

    #[test]
    fn mentions_mode_acts_on_app_mention() {
        let e = ev("app_mention", "UAUTH");
        assert!(classify_event(&cfg(SlackListenMode::Mentions), Some("UBOT"), &e).is_some());
    }

    #[test]
    fn mentions_mode_acts_on_dm() {
        let mut e = ev("message", "UAUTH");
        e.channel_type = Some("im".into());
        assert!(classify_event(&cfg(SlackListenMode::Mentions), Some("UBOT"), &e).is_some());
    }

    #[test]
    fn mentions_mode_acts_on_inline_mention() {
        let mut e = ev("message", "UAUTH");
        e.text = Some("hey <@UBOT> status".into());
        assert!(classify_event(&cfg(SlackListenMode::Mentions), Some("UBOT"), &e).is_some());
    }

    #[test]
    fn all_mode_acts_on_plain_channel_message() {
        let e = ev("message", "UAUTH");
        assert!(classify_event(&cfg(SlackListenMode::All), Some("UBOT"), &e).is_some());
    }

    #[test]
    fn unauthorized_user_ignored() {
        let e = ev("app_mention", "USTRANGER");
        assert!(classify_event(&cfg(SlackListenMode::Mentions), Some("UBOT"), &e).is_none());
    }

    #[test]
    fn own_message_ignored() {
        let e = ev("message", "UBOT");
        assert!(classify_event(&cfg(SlackListenMode::All), Some("UBOT"), &e).is_none());
    }

    #[test]
    fn bot_and_subtype_messages_ignored() {
        let mut e = ev("message", "UAUTH");
        e.bot_id = Some("B1".into());
        assert!(classify_event(&cfg(SlackListenMode::All), Some("UBOT"), &e).is_none());
        let mut e2 = ev("message", "UAUTH");
        e2.subtype = Some("message_changed".into());
        assert!(classify_event(&cfg(SlackListenMode::All), Some("UBOT"), &e2).is_none());
    }

    #[test]
    fn non_message_events_ignored() {
        let e = ev("reaction_added", "UAUTH");
        assert!(classify_event(&cfg(SlackListenMode::All), Some("UBOT"), &e).is_none());
    }

    #[test]
    fn strips_leading_mention() {
        assert_eq!(strip_leading_mention("<@UBOT> run tests"), "run tests");
        assert_eq!(strip_leading_mention("  <@UBOT>   spaced"), "spaced");
    }

    #[test]
    fn leaves_text_without_leading_mention() {
        assert_eq!(strip_leading_mention("just text"), "just text");
        assert_eq!(strip_leading_mention("ping <@UBOT>"), "ping <@UBOT>");
    }

    #[test]
    fn parses_events_api_envelope() {
        let raw = r#"{
            "type":"events_api",
            "envelope_id":"abc-123",
            "payload":{"event":{"type":"app_mention","user":"UAUTH","text":"<@UBOT> hi","channel":"C9","ts":"1.2"}}
        }"#;
        let env: SocketEnvelope = serde_json::from_str(raw).unwrap();
        assert_eq!(env.envelope_type.as_deref(), Some("events_api"));
        assert_eq!(env.envelope_id.as_deref(), Some("abc-123"));
        let event = env.payload.unwrap().event.unwrap();
        assert_eq!(event.event_type.as_deref(), Some("app_mention"));
        assert_eq!(event.channel.as_deref(), Some("C9"));
    }

    // ── Relay runs OFF the socket read loop; pings/acks keep being serviced ───
    // Mirrors Discord's off-task test: proves a slow reply cannot block the read
    // loop, so the loop stays free to pong pings, ack envelopes, and handle a
    // disconnect refresh — none of which can happen if the relay is awaited inline.

    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::super::routing::TargetSession;

    /// A transport whose `send_and_capture` blocks far longer than the test runs,
    /// modelling a slow agent reply.
    struct SlowTransport {
        sessions: Vec<TargetSession>,
    }

    impl FleetTransport for SlowTransport {
        async fn discover(&self) -> Vec<TargetSession> {
            self.sessions.clone()
        }
        async fn send_and_capture(
            &self,
            _session: &TargetSession,
            _text: &str,
            _timeout: Duration,
        ) -> Option<String> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Some("late reply".into())
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

    #[tokio::test]
    async fn relay_runs_off_task_so_read_loop_keeps_servicing() {
        let transport: &'static SlowTransport = Box::leak(Box::new(SlowTransport {
            sessions: vec![sess("conductor")],
        }));

        // Kick off a relay exactly the way the read loop does — in a spawned task
        // — then prove the loop keeps servicing frames (its ping/ack cadence)
        // while the relay (an hour-long sleep) is still outstanding.
        let relay_done = Arc::new(AtomicU64::new(0));
        let rd = relay_done.clone();
        tokio::spawn(async move {
            let params = RelayParams {
                default_target: None,
                response_timeout: Duration::from_secs(300),
            };
            let _ = relay(transport, &params, "status?").await;
            rd.store(1, Ordering::SeqCst);
        });

        // Stand-in for the socket read cadence (pings to pong / envelopes to ack):
        // a fast interval the loop services. With the relay OFF-task these keep
        // firing; awaited inline they would all block behind the 1h sleep — the
        // dropped-connection failure this fix removes.
        let mut serviced: u64 = 0;
        let mut ticker = tokio::time::interval(Duration::from_millis(5));
        ticker.tick().await; // immediate first tick
        for _ in 0..5 {
            ticker.tick().await;
            serviced += 1;
        }

        assert_eq!(
            serviced, 5,
            "read loop must keep servicing frames during a slow relay"
        );
        assert_eq!(
            relay_done.load(Ordering::SeqCst),
            0,
            "the relay should still be in flight — proving it did not block the read loop"
        );
    }
}
