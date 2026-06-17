// ABOUTME: Secret scrubbing for bridge diagnostics — keeps tokens out of disk/logs.
//
// The phone bridge persists its most-recent error to `daemons/bridge.json`
// (`last_error`), which the `ainb fleet daemons` CLI verb and the TUI Daemons
// screen render, and every channel also logs error diagnostics. A
// `reqwest::Error`'s `Display` includes the request URL, and the Telegram Bot
// API embeds the bot token IN the URL path
// (`https://api.telegram.org/bot<TOKEN>/getUpdates`). Slack and Discord tokens
// can likewise reach a diagnostic string. Letting any of those reach `last_error`
// or a log leaks the token to disk and to anyone watching the surface.
//
// These scrubbers are the defense-in-depth sink: every diagnostic string is run
// through one before it is recorded or logged, so even a future code path that
// forgets to build a clean message can't leak a known token shape. They are pure
// string transforms (no allocation when nothing matches) so they are cheap and
// exhaustively testable.

use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    /// Telegram bot tokens: `bot<digits>:<base64ish>` (as they appear in the API
    /// URL path) and the bare `<digits>:<base64ish>` token form.
    static ref TELEGRAM_TOKEN: Regex =
        Regex::new(r"bot\d+:[A-Za-z0-9_-]{20,}").expect("valid telegram token regex");
    static ref TELEGRAM_BARE_TOKEN: Regex =
        Regex::new(r"\b\d{6,}:[A-Za-z0-9_-]{20,}").expect("valid bare telegram token regex");
    /// Slack tokens: bot (`xoxb-…`), the `xox*` families (user `xoxp-`, config
    /// `xoxe-`, refresh `xoxr-`, …) AND app-level tokens (`xapp-…`), which use a
    /// distinct `xapp` prefix rather than `xox`.
    static ref SLACK_TOKEN: Regex =
        Regex::new(r"(?:xox[baprse]|xapp)-[A-Za-z0-9-]+").expect("valid slack token regex");
    /// Discord bot tokens: three base64url segments of roughly 24+ / 6 / 27+
    /// chars (`<user-id>.<timestamp>.<hmac>`). Deliberately conservative on the
    /// boundaries so it does not eat ordinary dotted identifiers (e.g. versions).
    static ref DISCORD_TOKEN: Regex =
        Regex::new(r"[\w-]{24,}\.[\w-]{6}\.[\w-]{27,}").expect("valid discord token regex");
}

/// Replacement marker substituted for any matched secret. Stable so callers and
/// tests can assert on it.
pub const REDACTED: &str = "<redacted>";

/// Scrub Telegram/Slack token shapes out of a diagnostic string before it is
/// persisted or displayed. Order matters only in that the more specific `bot…:…`
/// form is replaced before the bare `digits:base64` form, so the `bot` prefix is
/// also removed. Returns a new string; secret-free inputs round-trip unchanged.
#[must_use]
pub fn scrub_secrets(input: &str) -> String {
    let s = TELEGRAM_TOKEN.replace_all(input, REDACTED);
    let s = TELEGRAM_BARE_TOKEN.replace_all(&s, REDACTED);
    let s = SLACK_TOKEN.replace_all(&s, REDACTED);
    s.into_owned()
}

/// Replace any Discord-bot-token-shaped substring in `text` with `[redacted]`.
///
/// Pure; safe to call on every diagnostic string before logging.
#[must_use]
pub fn scrub_token(text: &str) -> String {
    DISCORD_TOKEN.replace_all(text, "[redacted]").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_telegram_token_in_api_url() {
        // The exact leak shape: a reqwest error Display carrying the bot token in
        // the getUpdates URL path.
        let leak = "kind=connect status=None display=\"error sending request for url \
                    (https://api.telegram.org/bot123456789:ABC-DEF_ghiJKLmnopqrstuvwxyz012345/getUpdates)\" source=[]";
        let scrubbed = scrub_secrets(leak);
        assert!(
            scrubbed.contains(REDACTED),
            "expected redaction: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("ABC-DEF_ghiJKLmnopqrstuvwxyz012345"),
            "token body leaked: {scrubbed}"
        );
        assert!(
            !scrubbed.contains("bot123456789:"),
            "token prefix leaked: {scrubbed}"
        );
    }

    #[test]
    fn scrubs_bare_telegram_token() {
        let leak = "getUpdates: 123456789:ABCdefGHIjklMNOpqrstuvwx failed";
        let scrubbed = scrub_secrets(leak);
        assert!(scrubbed.contains(REDACTED));
        assert!(!scrubbed.contains("ABCdefGHIjklMNOpqrstuvwx"));
        assert!(!scrubbed.contains("123456789:"));
    }

    #[test]
    fn scrubs_slack_bot_and_app_tokens() {
        let leak =
            "socket error: auth failed for xoxb-1111-2222-aaaaBBBBcccc and xapp-1-A0-99-deadbeef";
        let scrubbed = scrub_secrets(leak);
        assert!(
            !scrubbed.contains("xoxb-1111-2222-aaaaBBBBcccc"),
            "{scrubbed}"
        );
        assert!(!scrubbed.contains("xapp-1-A0-99-deadbeef"), "{scrubbed}");
        assert_eq!(scrubbed.matches(REDACTED).count(), 2);
    }

    #[test]
    fn leaves_secret_free_strings_untouched() {
        let clean = "kind=timeout status=Some(429) display=\"operation timed out\" source=[]";
        assert_eq!(scrub_secrets(clean), clean);
    }

    #[test]
    fn does_not_redact_innocuous_colon_numbers() {
        // A short `id:value` like an http status or a chat id must NOT be eaten —
        // the bare-token rule requires >=6 leading digits AND a >=20-char tail.
        let clean = "sendMessage HTTP 400: chat_id 42 not found";
        assert_eq!(scrub_secrets(clean), clean);
    }

    // Synthetic placeholders: each matches the Discord redaction regex (three
    // `[\w-]{24+}.{6}.{27+}` segments) but is obviously not a real token, so
    // secret scanners don't flag this file.
    #[test]
    fn redacts_a_discord_bot_token() {
        let token = "fake-user-id-segment-xxxx.tttttt.fake-hmac-segment-yyyyyyyyyy";
        let msg = format!("auth failed with Bot {token} (401)");
        let scrubbed = scrub_token(&msg);
        assert!(
            !scrubbed.contains(token),
            "token must not survive scrubbing"
        );
        assert!(scrubbed.contains("[redacted]"));
        assert!(scrubbed.contains("(401)"), "non-token text is preserved");
    }

    #[test]
    fn redacts_token_anywhere_in_the_string() {
        let token = "placeholder-first-segment-zz.midseg.placeholder-third-segment-w";
        let scrubbed = scrub_token(&format!("prefix {token} suffix"));
        assert_eq!(scrubbed, "prefix [redacted] suffix");
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let msg = "HTTP 500 (code 50001: Missing Access)";
        assert_eq!(scrub_token(msg), msg);
        // A short dotted identifier (e.g. a version) is not token-shaped.
        assert_eq!(scrub_token("v10.0.1"), "v10.0.1");
    }

    #[test]
    fn redacts_multiple_tokens() {
        let t1 = "AAAAAAAAAAAAAAAAAAAAAAAAA.BBBBBB.CCCCCCCCCCCCCCCCCCCCCCCCCCC";
        let t2 = "DDDDDDDDDDDDDDDDDDDDDDDDD.EEEEEE.FFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let scrubbed = scrub_token(&format!("{t1} and {t2}"));
        assert_eq!(scrubbed, "[redacted] and [redacted]");
    }
}
