// ABOUTME: Secret scrubbing for bridge diagnostics — keeps tokens out of disk.
//
// The phone bridge persists its most-recent error to `daemons/bridge.json`
// (`last_error`), which the `ainb fleet daemons` CLI verb and the TUI Daemons
// screen render. A `reqwest::Error`'s `Display` includes the request URL, and
// the Telegram Bot API embeds the bot token IN the URL path
// (`https://api.telegram.org/bot<TOKEN>/getUpdates`). Letting that reach
// `last_error` leaks the token to disk and to anyone watching the surface.
//
// [`scrub_secrets`] is the defense-in-depth sink: every diagnostic string is run
// through it before it is recorded, so even a future code path that forgets to
// build a clean message can't leak a known token shape. It is deliberately a
// pure string transform (no allocation when nothing matches) so it is cheap and
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
}

/// Replacement marker substituted for any matched secret. Stable so callers and
/// tests can assert on it.
pub const REDACTED: &str = "<redacted>";

/// Scrub any known token shape out of a diagnostic string before it is persisted
/// or displayed. Order matters only in that the more specific `bot…:…` form is
/// replaced before the bare `digits:base64` form, so the `bot` prefix is also
/// removed. Returns a new string; secret-free inputs round-trip unchanged.
#[must_use]
pub fn scrub_secrets(input: &str) -> String {
    let s = TELEGRAM_TOKEN.replace_all(input, REDACTED);
    let s = TELEGRAM_BARE_TOKEN.replace_all(&s, REDACTED);
    let s = SLACK_TOKEN.replace_all(&s, REDACTED);
    s.into_owned()
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
}
