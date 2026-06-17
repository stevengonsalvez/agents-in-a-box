// ABOUTME: Token redaction for bridge diagnostics — keep secrets out of logs.
//
// Channel error paths build their diagnostics from HTTP status / flags rather
// than echoing raw transport errors, but as defence-in-depth we also scrub any
// token-shaped substring before logging. A Discord *bot* token has the shape
//   <base64 user-id>.<base64 timestamp>.<base64 hmac>
// — three dot-separated base64url segments of roughly ~24+/~6/~27+ chars. We
// replace any such run with `[redacted]` so a token can never leak through an
// error string. (The tests below use obviously-synthetic placeholders rather
// than a realistic token so secret scanners don't flag this source file.)

use std::sync::LazyLock;

use regex::Regex;

/// Matches a Discord-bot-token-shaped substring: three base64url segments of
/// roughly 24+ / 6 / 27+ chars. Deliberately conservative on the boundaries so
/// it does not eat ordinary dotted identifiers.
static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\w-]{24,}\.[\w-]{6}\.[\w-]{27,}").expect("valid token regex"));

/// Replace any token-shaped substring in `text` with `[redacted]`.
///
/// Pure; safe to call on every diagnostic string before logging.
#[must_use]
pub fn scrub_token(text: &str) -> String {
    TOKEN_RE.replace_all(text, "[redacted]").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic placeholders: each matches the redaction regex (three
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
