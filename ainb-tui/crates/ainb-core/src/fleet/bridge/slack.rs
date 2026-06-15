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
use super::relay::{FleetTransport, RelayParams, relay};

const API_BASE: &str = "https://slack.com/api";
/// Slack message text limit (chat.postMessage). We split below this.
const SLACK_MAX_LENGTH: usize = 3900;

/// Run the Slack channel forever (reconnecting on disconnect).
pub async fn run<T: FleetTransport>(cfg: SlackConfig, transport: &T) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("building Slack HTTP client")?;

    // Resolve our own bot user id once so we never relay our own posts.
    let self_user_id = auth_test(&client, &cfg.bot_token).await;
    tracing::info!(
        bot_user = self_user_id.as_deref().unwrap_or("?"),
        mode = ?cfg.listen_mode,
        "ainb phone bridge: Slack channel online (socket-mode)"
    );

    loop {
        match connect_and_serve(&client, &cfg, self_user_id.as_deref(), transport).await {
            Ok(()) => {
                tracing::info!("Slack socket closed; reconnecting");
            }
            Err(e) => {
                tracing::warn!(error = %e, "Slack socket error; reconnecting in 5s");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn connect_and_serve<T: FleetTransport>(
    client: &reqwest::Client,
    cfg: &SlackConfig,
    self_user_id: Option<&str>,
    transport: &T,
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
                    handle_event(client, cfg, self_user_id, transport, event).await;
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
}
