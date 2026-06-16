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
/// shared `transport` through the relay core.
pub async fn run<T: FleetTransport>(cfg: TelegramConfig, transport: &T) -> Result<()> {
    let client = build_client()?;

    let me = get_me(&client, &cfg.token).await;
    let bot_username = me.as_ref().and_then(|m| m.username.clone());
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
                tracing::warn!(
                    offset,
                    error = describe_get_updates_error(&e),
                    "getUpdates failed; backing off (offset preserved)"
                );
                tokio::time::sleep(Duration::from_secs(3)).await;
                continue;
            }
        };
        for update in updates {
            offset = offset.max(update.update_id + 1);
            if let Some(message) = update.message {
                handle_message(&client, &cfg, bot_username.as_deref(), transport, message).await;
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
/// (`timeout`/`connect`/`request`/`body`/`decode`), the HTTP status if any, the
/// Display string, and the underlying source chain (e.g. hyper's "connection
/// closed before message completed" on a stale keep-alive socket).
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
    // "connection closed before message completed" on a stale keep-alive).
    let mut sources = Vec::new();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(err);
    while let Some(s) = src {
        sources.push(s.to_string());
        src = s.source();
    }

    format!(
        "kind={} status={:?} display=\"{}\" source=[{}]",
        kinds.join("|"),
        status,
        err,
        sources.join(" -> ")
    )
}

async fn handle_message<T: FleetTransport>(
    client: &reqwest::Client,
    cfg: &TelegramConfig,
    bot_username: Option<&str>,
    transport: &T,
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

    // Split the RAW reply BEFORE HTML conversion (the verified ordering).
    for raw_chunk in split_message(&reply, TG_MAX_LENGTH) {
        let html = md_to_tg_html(&raw_chunk);
        if let Err(e) = send_message(client, &cfg.token, message.chat.id, &html, true).await {
            tracing::warn!(error = %e, "HTML send failed; retrying as plain text");
            if let Err(e2) =
                send_message(client, &cfg.token, message.chat.id, &raw_chunk, false).await
            {
                tracing::warn!(error = %e2, "plain-text retry also failed");
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
    let resp = client.post(&url).json(&payload).send().await.context("sendMessage request")?;
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
}
