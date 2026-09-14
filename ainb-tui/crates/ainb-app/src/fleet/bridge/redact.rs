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
// [`scrub`] is the single defense-in-depth sink shared by all three channels
// (Telegram, Slack, Discord) and the heartbeat error recorder: every diagnostic
// string is run through it before it is recorded or logged, so even a future
// code path that forgets to build a clean message can't leak a known token
// shape. It is a pure string transform (no allocation when nothing matches) so
// it is cheap and exhaustively testable.
//
// The mirror frame (issue #983) reuses the same sink for text it cannot drop:
// captured tmux scrollback, diff hunks and agent prose. So it also knows the
// credential shapes an operator's terminal or repo carries (Anthropic, OpenAI,
// GitHub, GitLab, AWS, Google, PEM private keys, JWTs, and a password in a URL),
// and [`find_secret`] exposes the same table as a tripwire for tests over a
// serialised frame.

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
    /// Discord bot tokens: three base64url segments of roughly 24+ / 6–12 / 27+
    /// chars (`<user-id>.<timestamp>.<hmac>`). The middle (timestamp) segment is a
    /// BOUNDED RANGE, not a fixed 6, because newer Discord tokens widen it (a
    /// 7-char middle is already in the wild) and a fixed `{6}` silently failed to
    /// redact those — leaking the token into `last_error`/logs. The `6,12` ceiling
    /// keeps it conservative so it still won't eat a short dotted version string.
    static ref DISCORD_TOKEN: Regex =
        Regex::new(r"[\w-]{24,}\.[\w-]{6,12}\.[\w-]{27,}").expect("valid discord token regex");
    /// A PEM private key: the whole armoured block when it is closed, the
    /// header and everything after it when the capture cut it off.
    static ref PEM_PRIVATE_KEY: Regex = Regex::new(
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z ]*PRIVATE KEY-----|\z)"
    )
    .expect("valid pem regex");
    /// Anthropic API and admin keys.
    static ref ANTHROPIC_KEY: Regex =
        Regex::new(r"sk-ant-[A-Za-z0-9_-]{20,}").expect("valid anthropic key regex");
    /// OpenAI keys, legacy `sk-…` and project `sk-proj-…`.
    static ref OPENAI_KEY: Regex =
        Regex::new(r"sk-(?:proj-)?[A-Za-z0-9_-]{32,}").expect("valid openai key regex");
    /// GitHub classic (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`) and fine-grained tokens.
    static ref GITHUB_TOKEN: Regex =
        Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{80,}")
            .expect("valid github token regex");
    /// GitLab personal access tokens.
    static ref GITLAB_TOKEN: Regex =
        Regex::new(r"glpat-[A-Za-z0-9_-]{20,}").expect("valid gitlab token regex");
    /// AWS access key ids (long-term `AKIA`, temporary `ASIA`).
    static ref AWS_ACCESS_KEY: Regex =
        Regex::new(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b").expect("valid aws key regex");
    /// Google API keys.
    static ref GOOGLE_API_KEY: Regex =
        Regex::new(r"AIza[0-9A-Za-z_-]{35}").expect("valid google key regex");
    /// JSON Web Tokens: base64url header and payload, both starting `ey`.
    static ref JWT: Regex =
        Regex::new(r"ey[A-Za-z0-9_-]{10,}\.ey[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]*")
            .expect("valid jwt regex");
    /// A password (or token) in a URL's userinfo: `scheme://user:secret@host`.
    /// Group 1 keeps the scheme so a scrubbed clone URL still reads as one.
    static ref URL_USERINFO: Regex =
        Regex::new(r"([A-Za-z][A-Za-z0-9+.-]*://)[^/\s:@]+:[^/\s@]+@")
            .expect("valid url userinfo regex");
}

/// Every credential shape [`scrub`] removes, by name, in the order it runs.
///
/// PEM runs first so a key block is removed whole before a narrower pattern
/// eats a line of its body, and the Anthropic shape runs before the `sk-` one
/// so an `sk-ant-` key is named for what it is.
fn shapes() -> [(&'static str, &'static Regex); 13] {
    [
        ("pem private key", &PEM_PRIVATE_KEY),
        ("telegram bot token", &TELEGRAM_TOKEN),
        ("telegram token", &TELEGRAM_BARE_TOKEN),
        ("slack token", &SLACK_TOKEN),
        ("discord token", &DISCORD_TOKEN),
        ("anthropic key", &ANTHROPIC_KEY),
        ("openai key", &OPENAI_KEY),
        ("github token", &GITHUB_TOKEN),
        ("gitlab token", &GITLAB_TOKEN),
        ("aws access key", &AWS_ACCESS_KEY),
        ("google api key", &GOOGLE_API_KEY),
        ("jwt", &JWT),
        ("url userinfo", &URL_USERINFO),
    ]
}

/// The first credential shape found in `input`, as `(shape name, matched text)`.
///
/// The value-shaped tripwire over a serialised frame: independent of field
/// names, so it catches a secret that arrived through a path nobody listed.
#[must_use]
pub fn find_secret(input: &str) -> Option<(&'static str, String)> {
    shapes()
        .into_iter()
        .find_map(|(name, re)| re.find(input).map(|m| (name, m.as_str().to_string())))
}

/// Replacement marker substituted for any matched secret. Stable so callers and
/// tests can assert on it.
pub const REDACTED: &str = "<redacted>";

/// Scrub every known credential shape (see [`find_secret`]) out of a string.
///
/// Runs before a string is persisted, logged or put in a mirror frame. Order matters: the
/// more specific `bot…:…` Telegram form is replaced before the bare
/// `digits:base64` form, so the `bot` prefix is also removed, and a URL keeps
/// its scheme and host with only the userinfo replaced. Returns a new string;
/// secret-free inputs round-trip unchanged.
#[must_use]
pub fn scrub(input: &str) -> String {
    let mut out = std::borrow::Cow::Borrowed(input);
    for (name, re) in shapes() {
        if !re.is_match(&out) {
            continue;
        }
        let replaced = if name == "url userinfo" {
            re.replace_all(&out, format!("${{1}}{REDACTED}@").as_str()).into_owned()
        } else {
            re.replace_all(&out, REDACTED).into_owned()
        };
        out = std::borrow::Cow::Owned(replaced);
    }
    out.into_owned()
}

/// Telegram/Slack-oriented alias for [`scrub`]. Retained so the heartbeat error
/// sink and the Telegram/Slack channels read intent-fully; it scrubs every known
/// token shape, not just Telegram/Slack ones.
#[must_use]
pub fn scrub_secrets(input: &str) -> String {
    scrub(input)
}

/// Discord-oriented alias for [`scrub`]. Retained so the Discord channel reads
/// intent-fully; it scrubs every known token shape, not just Discord ones.
#[must_use]
pub fn scrub_token(text: &str) -> String {
    scrub(text)
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
        assert!(scrubbed.contains(REDACTED));
        assert!(scrubbed.contains("(401)"), "non-token text is preserved");
    }

    #[test]
    fn redacts_token_anywhere_in_the_string() {
        let token = "placeholder-first-segment-zz.midseg.placeholder-third-segment-w";
        let scrubbed = scrub_token(&format!("prefix {token} suffix"));
        assert_eq!(scrubbed, format!("prefix {REDACTED} suffix"));
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        let msg = "HTTP 500 (code 50001: Missing Access)";
        assert_eq!(scrub_token(msg), msg);
        // A short dotted identifier (e.g. a version) is not token-shaped.
        assert_eq!(scrub_token("v10.0.1"), "v10.0.1");
    }

    #[test]
    fn redacts_discord_token_with_seven_char_middle_segment() {
        // REGRESSION: newer Discord tokens carry a 7-char (not 6) middle segment.
        // The old `[\w-]{6}` middle pinned exactly 6 and silently let these
        // through, leaking the token into last_error/logs. The bounded `{6,12}`
        // middle must now catch it.
        let token = "AAAAAAAAAAAAAAAAAAAAAAAAA.BBBBBBB.CCCCCCCCCCCCCCCCCCCCCCCCCCC";
        let scrubbed = scrub_token(&format!("auth failed with Bot {token} (401)"));
        assert!(
            !scrubbed.contains(token),
            "7-char-middle token must be redacted: {scrubbed}"
        );
        assert!(scrubbed.contains(REDACTED));
        assert!(scrubbed.contains("(401)"), "non-token text is preserved");
    }

    // Synthetic credential shapes, assembled at runtime so no literal in this
    // file matches a secret scanner.
    fn fake(prefix: &str, body: char, len: usize) -> String {
        format!("{prefix}{}", body.to_string().repeat(len))
    }

    #[test]
    fn finds_and_scrubs_every_issue_983_shape() {
        let cases = [
            ("anthropic key", fake("sk-ant-api03-", 'A', 40)),
            ("openai key", fake("sk-", 'b', 48)),
            ("github token", fake("ghp_", 'C', 36)),
            ("github token", fake("github_pat_", 'd', 82)),
            ("gitlab token", fake("glpat-", 'e', 20)),
            ("aws access key", fake("AKIA", 'F', 16)),
            ("google api key", fake("AIza", 'g', 35)),
            (
                "pem private key",
                format!(
                    "-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----",
                    fake("", 'h', 64)
                ),
            ),
            (
                "jwt",
                format!(
                    "{}.{}.{}",
                    fake("eyJ", 'i', 20),
                    fake("eyJ", 'j', 20),
                    fake("", 'k', 20)
                ),
            ),
            (
                "url userinfo",
                format!(
                    "https://x-access-token:{}@github.com/o/r",
                    fake("", 'l', 12)
                ),
            ),
        ];
        for (shape, secret) in cases {
            let text = format!("before {secret} after");
            let found = find_secret(&text).unwrap_or_else(|| panic!("{shape} not found"));
            assert_eq!(found.0, shape, "{secret}");
            let scrubbed = scrub(&text);
            assert!(
                find_secret(&scrubbed).is_none(),
                "{shape} survived: {scrubbed}"
            );
            assert!(
                scrubbed.starts_with("before ") && scrubbed.ends_with(" after"),
                "{scrubbed}"
            );
        }
    }

    #[test]
    fn a_scrubbed_clone_url_keeps_its_scheme_and_host() {
        let url = format!("https://oauth2:{}@gitlab.com/o/r.git", fake("", 'z', 20));
        assert_eq!(
            scrub(&url),
            format!("https://{REDACTED}@gitlab.com/o/r.git")
        );
        assert_eq!(scrub("https://github.com/o/r"), "https://github.com/o/r");
        assert_eq!(
            scrub("ssh://git@github.com/o/r"),
            "ssh://git@github.com/o/r"
        );
    }

    #[test]
    fn a_truncated_pem_block_is_removed_to_the_end() {
        let text = format!(
            "key:\n-----BEGIN OPENSSH PRIVATE KEY-----\n{}",
            fake("", 'q', 70)
        );
        assert_eq!(scrub(&text), format!("key:\n{REDACTED}"));
    }

    #[test]
    fn redacts_multiple_discord_tokens() {
        let t1 = "AAAAAAAAAAAAAAAAAAAAAAAAA.BBBBBB.CCCCCCCCCCCCCCCCCCCCCCCCCCC";
        let t2 = "DDDDDDDDDDDDDDDDDDDDDDDDD.EEEEEE.FFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let scrubbed = scrub_token(&format!("{t1} and {t2}"));
        assert_eq!(scrubbed, format!("{REDACTED} and {REDACTED}"));
    }
}
