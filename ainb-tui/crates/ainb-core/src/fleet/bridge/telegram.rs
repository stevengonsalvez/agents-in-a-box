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

/// Run the Telegram channel forever (until the task is cancelled). Drives the
/// shared `transport` through the relay core.
pub async fn run<T: FleetTransport>(cfg: TelegramConfig, transport: &T) -> Result<()> {
    let client = reqwest::Client::builder()
        // Slightly longer than the long-poll so the socket isn't killed mid-wait.
        .timeout(Duration::from_secs(LONG_POLL_SECS + 15))
        .build()
        .context("building Telegram HTTP client")?;

    let me = get_me(&client, &cfg.token).await;
    let bot_username = me.as_ref().and_then(|m| m.username.clone());
    tracing::info!(
        bot = bot_username.as_deref().unwrap_or("?"),
        "ainb phone bridge: Telegram channel online (long-polling)"
    );

    let mut offset: i64 = 0;
    loop {
        let updates = match get_updates(&client, &cfg.token, offset).await {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "getUpdates failed; backing off");
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
    if let (Some(username), Some(text)) = (bot_username, &message.text) {
        return text.to_lowercase().contains(&format!("@{}", username.to_lowercase()));
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

async fn get_updates(client: &reqwest::Client, token: &str, offset: i64) -> Result<Vec<TgUpdate>> {
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
        .context("getUpdates request")?;
    let body: TgResponse<Vec<TgUpdate>> = resp.json().await.context("getUpdates decode")?;
    if !body.ok {
        anyhow::bail!("getUpdates returned ok=false: {:?}", body.description);
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
}
