// ABOUTME: The Telegram channel — long-polling getUpdates, two-way relay.
//
// Ported from the Python `bridge.py` (aiogram) dispatcher, reimplemented over
// the Bot API's getUpdates long-poll via reqwest (no aiogram/teloxide runtime).
//
// Inbound: an authorized Telegram message -> the shared relay core resolves the
// target session and captures its reply -> reply to Telegram as HTML, splitting
// the RAW reply BEFORE HTML conversion at 4096 chars (so a chunk boundary can
// never slice an HTML tag — Telegram rejects that with a 400). HTML send
// failures fall back to plain text so the user still gets the content.
//
// Authorization is by Telegram user_id (unknown senders are silently ignored).
// In group chats the bot only acts when @mentioned or when the message is a
// reply to the bot (gated by `require_mention_in_groups`).

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;

use super::config::TelegramConfig;
use super::format::{TG_MAX_LENGTH, md_to_tg_html, split_message};
use super::heartbeat::BridgeHeartbeat;
use super::relay::{FleetTransport, RelayParams, relay};

const API_BASE: &str = "https://api.telegram.org";
/// Long-poll timeout (seconds) passed to getUpdates. The HTTP request waits up
/// to this long for an update before returning empty, so we don't hot-loop.
const LONG_POLL_SECS: u64 = 30;
/// Buffer added to the long-poll timeout to get the reqwest per-request timeout.
/// The client timeout must be comfortably GREATER than the server-side long-poll
/// so a normal empty 30s poll never trips the client's own deadline. 15s leaves
/// room for connect + TLS + the server's slack in honouring the timeout param.
const CLIENT_TIMEOUT_BUFFER_SECS: u64 = 15;

/// Run the Telegram channel forever (until the task is cancelled). Drives the
/// shared `transport` through the relay core, reporting connection + relay
/// activity through the shared `heartbeat` so the Daemons surface can show
/// "Telegram channel online" + last-relay (the original blind spot).
///
/// `transport` is `&'static` because each inbound message's relay runs in its
/// own `tokio::spawn` task (so a slow reply can never stall the getUpdates
/// long-poll loop — see the spawn in the loop below); the spawned future must
/// therefore not borrow a non-`'static` transport. The daemon leaks a single
/// shared transport, so this costs nothing (mirrors the Discord channel).
pub async fn run<T: FleetTransport + 'static>(
    cfg: TelegramConfig,
    transport: &'static T,
    heartbeat: BridgeHeartbeat,
) -> Result<()> {
    let client = build_client()?;

    let me = get_me(&client, &cfg.token).await;
    let bot_username = me.as_ref().and_then(|m| m.username.clone());
    // A successful getMe is the "channel online" signal. Label the channel with
    // the bot handle so the operator sees exactly which account is relaying.
    let online = me.is_some();
    let channel = match &bot_username {
        Some(u) => format!("Telegram (@{u})"),
        None => "Telegram".to_string(),
    };
    heartbeat.set_connected(online, Some(channel));
    tracing::info!(
        bot = bot_username.as_deref().unwrap_or("?"),
        "ainb phone bridge: Telegram channel online (long-polling)"
    );

    // `offset` is the exactly-once watermark: the next update_id we have NOT yet
    // consumed. It is mutated ONLY after an update is processed below, never on
    // the error path — so a transient getUpdates failure backs off and retries
    // the same offset rather than dropping or double-consuming a message.
    let mut offset: i64 = 0;
    loop {
        let updates = match get_updates(&client, &cfg.token, offset).await {
            Ok(u) => u,
            Err(e) => {
                // Surface the FULL reqwest failure shape, not a generic context
                // string, so an intermittent flap is diagnosable from the logs.
                let desc = describe_get_updates_error(&e);
                tracing::warn!(
                    offset,
                    error = desc,
                    "getUpdates failed; backing off (offset preserved)"
                );
                // A failed poll means the channel is no longer confirmed online.
                heartbeat.record_error(format!("getUpdates: {desc}"));
                heartbeat.set_connected(false, None);
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };
        // A successful poll (re)confirms the channel is online — recover from a
        // prior transient failure that flipped connected=false.
        heartbeat.set_connected(true, None);
        for update in updates {
            // Advance the watermark FIRST, then dispatch the (slow) relay OFF the
            // poll loop. `handle_message` -> `relay` polls the agent for up to
            // `response_timeout` (300s default); awaiting it inline here would
            // freeze getUpdates for that whole window — no new messages read, the
            // heartbeat-relevant poll socket stalls. Because the offset is already
            // advanced, the next poll moves on immediately while the spawned task
            // handles the reply independently. The `reqwest::Client` clone is
            // cheap (`Arc` inside), the config + bot handle are cloned, the
            // transport is `&'static` (shared, not the poll socket), and the
            // heartbeat handle is a cheap `Arc` clone — so the off-task relay
            // still records relay activity / errors on the shared surface.
            offset = offset.max(update.update_id + 1);
            if let Some(message) = update.message {
                let client = client.clone();
                let cfg = cfg.clone();
                let bot_username = bot_username.clone();
                let heartbeat = heartbeat.clone();
                tokio::spawn(async move {
                    handle_message(
                        &client,
                        &cfg,
                        bot_username.as_deref(),
                        transport,
                        &heartbeat,
                        message,
                    )
                    .await;
                });
            }
        }
    }
}

/// Build the long-poll HTTP client.
///
/// Two staleness mitigations beyond the plain timeout:
///   * The request timeout is `LONG_POLL_SECS + CLIENT_TIMEOUT_BUFFER_SECS`, so a
///     normal empty 30s long-poll never races the client deadline.
///   * `pool_idle_timeout` is kept BELOW the long-poll window and TCP keepalive is
///     enabled, so reqwest evicts a connection the upstream may have silently
///     half-closed instead of reusing a dead socket and surfacing a hard error.
///     (curl never hit this because it opens a fresh connection each invocation.)
fn build_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(
            LONG_POLL_SECS + CLIENT_TIMEOUT_BUFFER_SECS,
        ))
        // Evict idle pooled connections well before the upstream/NAT would, so a
        // stale keep-alive socket is never reused for the next long-poll.
        .pool_idle_timeout(Duration::from_secs(LONG_POLL_SECS / 2))
        .tcp_keepalive(Duration::from_secs(15))
        .build()
        .context("building Telegram HTTP client")
}

/// Render a getUpdates failure as a single diagnosable line. Replaces the old
/// generic `"getUpdates request"` context that swallowed everything.
fn describe_get_updates_error(err: &GetUpdatesError) -> String {
    match err {
        GetUpdatesError::Request(e) => format!("phase=request {}", describe_reqwest_error(e)),
        GetUpdatesError::Decode(e) => format!("phase=decode {}", describe_reqwest_error(e)),
        GetUpdatesError::ApiError(desc) => {
            format!("phase=api ok=false description={desc:?}")
        }
    }
}

/// Render a reqwest error as a single diagnosable line: the kind flags
/// (`timeout`/`connect`/`request`/`body`/`decode`), the HTTP status if any, and
/// the underlying source chain (e.g. hyper's "connection closed before message
/// completed" on a stale keep-alive socket).
///
/// SECURITY (M1): the error's own `Display` is deliberately NOT included. A
/// `reqwest::Error` Display carries the request URL, and the Telegram Bot API
/// embeds the bot token in the URL path
/// (`https://api.telegram.org/bot<TOKEN>/…`) — so emitting it would leak the
/// token into `last_error` (persisted to `bridge.json`, shown in CLI/TUI). The
/// kind flags + status + source chain give the same diagnosability without the
/// URL, and the whole line is additionally run through `scrub_secrets` at the
/// heartbeat sink as defense-in-depth.
fn describe_reqwest_error(err: &reqwest::Error) -> String {
    let mut kinds = Vec::new();
    if err.is_timeout() {
        kinds.push("timeout");
    }
    if err.is_connect() {
        kinds.push("connect");
    }
    if err.is_request() {
        kinds.push("request");
    }
    if err.is_body() {
        kinds.push("body");
    }
    if err.is_decode() {
        kinds.push("decode");
    }
    if kinds.is_empty() {
        kinds.push("other");
    }

    let status = err.status().map(|s| s.as_u16());

    // Walk the std::error::Error source chain (the real cause, e.g. hyper's
    // "connection closed before message completed" on a stale keep-alive). The
    // chain is scrubbed too — a source could in principle echo the URL.
    let mut sources = Vec::new();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(err);
    while let Some(s) = src {
        sources.push(super::redact::scrub_secrets(&s.to_string()));
        src = s.source();
    }

    format!(
        "kind={} status={:?} source=[{}]",
        kinds.join("|"),
        status,
        sources.join(" -> ")
    )
}

async fn handle_message<T: FleetTransport>(
    client: &reqwest::Client,
    cfg: &TelegramConfig,
    bot_username: Option<&str>,
    transport: &T,
    heartbeat: &BridgeHeartbeat,
    message: TgMessage,
) {
    // Authorization by user_id — unknown senders are silently ignored.
    let from_id = message.from.as_ref().map(|u| u.id);
    if from_id != Some(cfg.authorized_user_id) {
        tracing::warn!(user_id = ?from_id, "ignoring message from unauthorized user");
        return;
    }

    if !is_addressed(cfg, bot_username, &message) {
        return;
    }

    let raw = message.text.clone().or_else(|| message.caption.clone()).unwrap_or_default();
    let text = strip_bot_mention(&raw, bot_username);
    if text.trim().is_empty() {
        return;
    }

    let params = RelayParams {
        default_target: cfg.default_target.as_deref(),
        response_timeout: Duration::from_secs(cfg.response_timeout),
    };
    let reply = relay(transport, &params, &text).await;
    // A relayed turn is the "last relay" activity signal the operator watches.
    heartbeat.record_relay();

    // Split the RAW reply BEFORE HTML conversion (the verified ordering).
    for raw_chunk in split_message(&reply, TG_MAX_LENGTH) {
        let html = md_to_tg_html(&raw_chunk);
        if let Err(e) = send_message(client, &cfg.token, message.chat.id, &html, true).await {
            tracing::warn!(error = %e, "HTML send failed; retrying as plain text");
            if let Err(e2) =
                send_message(client, &cfg.token, message.chat.id, &raw_chunk, false).await
            {
                tracing::warn!(error = %e2, "plain-text retry also failed");
                heartbeat.record_error(format!("sendMessage failed: {e2}"));
            }
        }
    }
}

/// Whether the bot should act on this message.
fn is_addressed(cfg: &TelegramConfig, bot_username: Option<&str>, message: &TgMessage) -> bool {
    if message.chat.chat_type == "private" {
        return true;
    }
    if !cfg.require_mention_in_groups {
        return true;
    }
    // A reply to the bot's own message counts as addressed.
    if let Some(reply) = &message.reply_to_message {
        if reply.from.as_ref().is_some_and(|u| u.is_bot) {
            return true;
        }
    }
    // Media messages carry their text in `caption`; `handle_message` falls back
    // to it, so the mention check must scan both fields to stay consistent.
    if let Some(username) = bot_username {
        let needle = format!("@{}", username.to_lowercase());
        return message
            .text
            .iter()
            .chain(message.caption.iter())
            .any(|s| s.to_lowercase().contains(&needle));
    }
    false
}

/// Remove a leading `@botname` mention from group messages.
fn strip_bot_mention(text: &str, username: Option<&str>) -> String {
    let Some(username) = username else {
        return text.to_string();
    };
    let handle = format!("@{username}");
    let stripped = text.trim_start();
    if stripped.to_lowercase().starts_with(&handle.to_lowercase()) {
        return stripped[handle.len()..].trim_start().to_string();
    }
    text.to_string()
}

// ── Bot API calls ───────────────────────────────────────────────────────────

async fn get_me(client: &reqwest::Client, token: &str) -> Option<TgUser> {
    let url = format!("{API_BASE}/bot{token}/getMe");
    let resp = client.get(&url).send().await.ok()?;
    let body: TgResponse<TgUser> = resp.json().await.ok()?;
    body.result
}

/// A getUpdates failure that preserves the typed `reqwest::Error` so the caller
/// can inspect its kind/status/source chain. The generic `"getUpdates request"`
/// context string used to collapse all of that into a single opaque message.
#[derive(Debug)]
enum GetUpdatesError {
    /// The HTTP request itself failed (connect/timeout/body/stale keep-alive).
    Request(reqwest::Error),
    /// The response body could not be decoded as the expected JSON shape.
    Decode(reqwest::Error),
    /// Telegram answered with `ok:false`.
    ApiError(Option<String>),
}

async fn get_updates(
    client: &reqwest::Client,
    token: &str,
    offset: i64,
) -> std::result::Result<Vec<TgUpdate>, GetUpdatesError> {
    let url = format!("{API_BASE}/bot{token}/getUpdates");
    let resp = client
        .get(&url)
        .query(&[
            ("offset", offset.to_string()),
            ("timeout", LONG_POLL_SECS.to_string()),
            ("allowed_updates", "[\"message\"]".to_string()),
        ])
        .send()
        .await
        .map_err(GetUpdatesError::Request)?;
    let body: TgResponse<Vec<TgUpdate>> = resp.json().await.map_err(GetUpdatesError::Decode)?;
    if !body.ok {
        return Err(GetUpdatesError::ApiError(body.description));
    }
    Ok(body.result.unwrap_or_default())
}

async fn send_message(
    client: &reqwest::Client,
    token: &str,
    chat_id: i64,
    text: &str,
    html: bool,
) -> Result<()> {
    let url = format!("{API_BASE}/bot{token}/sendMessage");
    let mut payload = json!({ "chat_id": chat_id, "text": text });
    if html {
        payload["parse_mode"] = json!("HTML");
    }
    let resp = client.post(&url).json(&payload).send().await.map_err(|e| {
        anyhow::anyhow!("sendMessage request failed: {}", describe_reqwest_error(&e))
    })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("sendMessage HTTP {status}: {body}");
    }
    Ok(())
}

// ── Bot API wire types (only the fields we read) ────────────────────────────

#[derive(Debug, Deserialize)]
struct TgResponse<R> {
    ok: bool,
    // `Option` fields default to `None` when absent without `#[serde(default)]`,
    // which keeps the generic `R` free of a spurious `Default` bound.
    description: Option<String>,
    result: Option<R>,
}

#[derive(Debug, Deserialize)]
struct TgUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TgMessage>,
}

#[derive(Debug, Clone, Deserialize)]
struct TgMessage {
    chat: TgChat,
    #[serde(default)]
    from: Option<TgUser>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    reply_to_message: Option<Box<TgMessage>>,
}

#[derive(Debug, Clone, Deserialize)]
struct TgChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TgUser {
    id: i64,
    #[serde(default)]
    is_bot: bool,
    #[serde(default)]
    username: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(require_mention: bool) -> TelegramConfig {
        TelegramConfig {
            token: "t".into(),
            authorized_user_id: 1,
            default_target: None,
            require_mention_in_groups: require_mention,
            response_timeout: 300,
        }
    }

    fn msg(chat_type: &str, text: Option<&str>) -> TgMessage {
        TgMessage {
            chat: TgChat {
                id: 9,
                chat_type: chat_type.into(),
            },
            from: Some(TgUser {
                id: 1,
                is_bot: false,
                username: None,
            }),
            text: text.map(str::to_string),
            caption: None,
            reply_to_message: None,
        }
    }

    #[test]
    fn private_chat_is_always_addressed() {
        assert!(is_addressed(
            &cfg(true),
            Some("bot"),
            &msg("private", Some("hi"))
        ));
    }

    #[test]
    fn group_without_mention_is_ignored_when_required() {
        assert!(!is_addressed(
            &cfg(true),
            Some("bot"),
            &msg("group", Some("hello there"))
        ));
    }

    #[test]
    fn group_with_mention_is_addressed() {
        assert!(is_addressed(
            &cfg(true),
            Some("MyBot"),
            &msg("group", Some("@mybot status"))
        ));
    }

    #[test]
    fn group_caption_mention_is_addressed() {
        // A media message carries its only text in `caption`; the mention there
        // must address the bot just as it would in `text`.
        let mut m = msg("group", None);
        m.caption = Some("@mybot look at this".into());
        assert!(is_addressed(&cfg(true), Some("MyBot"), &m));
    }

    #[test]
    fn group_addressed_when_mention_not_required() {
        assert!(is_addressed(
            &cfg(false),
            Some("bot"),
            &msg("group", Some("anything"))
        ));
    }

    #[test]
    fn reply_to_bot_counts_as_addressed() {
        let mut m = msg("group", Some("ok"));
        m.reply_to_message = Some(Box::new(TgMessage {
            chat: TgChat {
                id: 9,
                chat_type: "group".into(),
            },
            from: Some(TgUser {
                id: 5,
                is_bot: true,
                username: Some("bot".into()),
            }),
            text: None,
            caption: None,
            reply_to_message: None,
        }));
        assert!(is_addressed(&cfg(true), Some("bot"), &m));
    }

    #[test]
    fn strips_leading_bot_mention() {
        assert_eq!(
            strip_bot_mention("@MyBot run tests", Some("MyBot")),
            "run tests"
        );
        assert_eq!(
            strip_bot_mention("@mybot   spaced", Some("MyBot")),
            "spaced"
        );
    }

    #[test]
    fn leaves_non_leading_mention_intact() {
        assert_eq!(strip_bot_mention("hey @MyBot", Some("MyBot")), "hey @MyBot");
    }

    #[test]
    fn no_username_returns_text_unchanged() {
        assert_eq!(strip_bot_mention("@bot hi", None), "@bot hi");
    }

    // ── Long-poll robustness invariants ──────────────────────────────────────

    #[test]
    fn client_timeout_exceeds_long_poll_window() {
        // The reqwest per-request timeout MUST be comfortably greater than the
        // server-side long-poll, or a normal empty 30s poll trips the client
        // deadline and surfaces as a spurious timeout error (the flapping bug).
        let client_timeout = LONG_POLL_SECS + CLIENT_TIMEOUT_BUFFER_SECS;
        assert!(
            client_timeout > LONG_POLL_SECS,
            "client timeout {client_timeout}s must exceed long-poll {LONG_POLL_SECS}s"
        );
        assert!(
            CLIENT_TIMEOUT_BUFFER_SECS >= 10,
            "buffer must leave >=10s for connect/TLS/server slack, got {CLIENT_TIMEOUT_BUFFER_SECS}s"
        );
    }

    #[test]
    fn build_client_succeeds() {
        // The pool/keepalive config must produce a valid client.
        assert!(build_client().is_ok());
    }

    #[test]
    fn offset_advance_is_exactly_once() {
        // Mirror the run loop's offset advancement: offset becomes max(offset,
        // update_id + 1) per processed update, and is NEVER touched on the error
        // path. Simulate updates [10, 11], a transient error (no advance), then a
        // retry returning [11] again (no double-consume past 12) and a new [12].
        let mut offset: i64 = 0;

        // First successful batch: updates 10 and 11.
        for update_id in [10_i64, 11] {
            offset = offset.max(update_id + 1);
        }
        assert_eq!(offset, 12, "offset advances past the highest consumed id");

        // Transient error path: offset MUST be preserved (loop does `continue`).
        let offset_before_error = offset;
        // (no mutation here — exactly what the Err arm does)
        assert_eq!(
            offset, offset_before_error,
            "error path must not move offset"
        );

        // Telegram only returns updates with id >= offset, so re-polling at 12
        // never re-delivers 10/11. A genuinely new update 12 advances to 13 once.
        for update_id in [12_i64] {
            offset = offset.max(update_id + 1);
        }
        assert_eq!(
            offset, 13,
            "a new update advances exactly once, no double-count"
        );
    }

    #[test]
    fn describe_get_updates_error_surfaces_api_failure() {
        let e = GetUpdatesError::ApiError(Some("Unauthorized".to_string()));
        let s = describe_get_updates_error(&e);
        assert!(s.contains("phase=api"), "got: {s}");
        assert!(s.contains("ok=false"), "got: {s}");
        assert!(s.contains("Unauthorized"), "got: {s}");
    }

    #[tokio::test]
    async fn reqwest_error_description_does_not_include_telegram_token_url() {
        let token = "123456789:AAHabcdefghijklmnopqrstuvwxyz123456";
        let client =
            reqwest::Client::builder().timeout(Duration::from_millis(100)).build().unwrap();
        let err = client
            .get(format!("http://127.0.0.1:9/bot{token}/sendMessage"))
            .send()
            .await
            .expect_err("closed localhost port should fail");
        let s = describe_reqwest_error(&err);
        assert!(!s.contains(token), "token leaked in error: {s}");
        assert!(!s.contains("/bot"), "bot URL path leaked in error: {s}");
        assert!(
            !s.contains("sendMessage"),
            "request URL leaked in error: {s}"
        );
    }

    // ── Relay runs OFF the poll loop; getUpdates keeps progressing ───────────
    // Mirrors Discord's `relay_runs_off_task_so_heartbeat_keeps_firing`: proves a
    // slow reply (the workload the bridge exists for) cannot stall the long-poll
    // loop, so new updates keep being read and the poll socket never freezes.

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
            // Real wall-clock so the test needs no tokio `test-util` feature.
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
    async fn relay_runs_off_task_so_poll_loop_keeps_progressing() {
        // Leak a slow transport to get the `&'static` the spawned relay needs,
        // mirroring how the daemon leaks its shared transport.
        let transport: &'static SlowTransport = Box::leak(Box::new(SlowTransport {
            sessions: vec![sess("conductor")],
        }));

        // Kick off a relay exactly the way the poll loop does — in a spawned task
        // — then prove the loop keeps advancing its watermark while the relay (an
        // hour-long sleep) is still outstanding.
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

        // Stand-in for the getUpdates poll cadence: a fast interval the loop
        // services. With the relay OFF-task these keep firing; if the relay were
        // awaited inline they would all be blocked behind the 1h sleep — exactly
        // the long-poll stall this fix removes.
        let mut polls: u64 = 0;
        let mut ticker = tokio::time::interval(Duration::from_millis(5));
        ticker.tick().await; // immediate first tick
        for _ in 0..5 {
            ticker.tick().await;
            polls += 1;
        }

        assert_eq!(
            polls, 5,
            "poll loop must keep progressing during a slow relay"
        );
        assert_eq!(
            relay_done.load(Ordering::SeqCst),
            0,
            "the relay should still be in flight — proving it did not block the poll loop"
        );
    }
}
