//! Secret scrubbing: keeps credentials out of disk, logs, frames and
//! transcripts.
//!
//! Lives here, below every consumer, because the transcript classifier in
//! `ainb-hangar-proto` must scrub before it cuts (#1187) and cannot reach
//! `ainb-app`, which re-exports this module where its callers always found it.
//!
//! The phone bridge persists its most-recent error to `daemons/bridge.json`
//! (`last_error`), which the `ainb fleet daemons` CLI verb and the TUI Daemons
//! screen render, and every channel also logs error diagnostics. A
//! `reqwest::Error`'s `Display` includes the request URL, and the Telegram Bot
//! API embeds the bot token IN the URL path
//! (`https://api.telegram.org/bot<TOKEN>/getUpdates`). Slack and Discord tokens
//! can likewise reach a diagnostic string. Letting any of those reach `last_error`
//! or a log leaks the token to disk and to anyone watching the surface.
//!
//! [`scrub`] is the single defense-in-depth sink shared by all three channels
//! (Telegram, Slack, Discord) and the heartbeat error recorder: every diagnostic
//! string is run through it before it is recorded or logged, so even a future
//! code path that forgets to build a clean message can't leak a known token
//! shape. It is a pure string transform (no allocation when nothing matches) so
//! it is cheap and exhaustively testable.
//!
//! The mirror frame (issue #983) reuses the same sink for text it cannot drop:
//! captured tmux scrollback, diff hunks and agent prose. So it also knows the
//! credential shapes an operator's terminal or repo carries (Anthropic, OpenAI,
//! GitHub, GitLab, AWS, Google, PEM private keys, JWTs, and a password in a URL),
//! and [`find_secret`] exposes the same table as a tripwire for tests over a
//! serialised frame.

use std::sync::LazyLock;

use regex::Regex;

/// Telegram bot tokens: `bot<digits>:<base64ish>` (as they appear in the API
/// URL path) and the bare `<digits>:<base64ish>` token form.
static TELEGRAM_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"bot\d+:[A-Za-z0-9_-]{20,}").expect("valid telegram token regex"));
static TELEGRAM_BARE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b\d{6,}:[A-Za-z0-9_-]{20,}").expect("valid bare telegram token regex")
});
/// Slack tokens: bot (`xoxb-…`), the `xox*` families (user `xoxp-`, config
/// `xoxe-`, refresh `xoxr-`, …) AND app-level tokens (`xapp-…`), which use a
/// distinct `xapp` prefix rather than `xox`.
static SLACK_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:xox[bapcdrse]|xapp)-[A-Za-z0-9-]+").expect("valid slack token regex")
});
/// Discord bot tokens: three base64url segments of roughly 24+ / 6–12 / 27+
/// chars (`<user-id>.<timestamp>.<hmac>`). The middle (timestamp) segment is a
/// BOUNDED RANGE, not a fixed 6, because newer Discord tokens widen it (a
/// 7-char middle is already in the wild) and a fixed `{6}` silently failed to
/// redact those, leaking the token into `last_error`/logs. The `6,12` ceiling
/// keeps it conservative so it still won't eat a short dotted version string.
static DISCORD_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\w-]{24,}\.[\w-]{6,12}\.[\w-]{27,}").expect("valid discord token regex")
});
/// A PEM private key: the whole armoured block when it is closed, the
/// header and everything after it when the capture cut it off.
static PEM_PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z ]*PRIVATE KEY-----|\z)")
        .expect("valid pem regex")
});
/// Anthropic API and admin keys.
static ANTHROPIC_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"sk-ant-[A-Za-z0-9_-]{20,}").expect("valid anthropic key regex"));
/// `OpenAI` keys, legacy `sk-…` and project, service-account and admin
/// forms. Word-anchored: without `\b` a session id like `ainb-task-<uuid>`
/// or a branch like `fix-risk-assessment-…` matched from the `sk-` inside it.
static OPENAI_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bsk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_-]{32,}")
        .expect("valid openai key regex")
});
/// An AWS secret access key: 40 base64 characters carry no prefix of their
/// own, so the shape is the assignment that names one. Group 1 keeps the
/// name so a scrubbed `.env` line still says what was there.
static AWS_SECRET_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
    r#"((?i:aws_secret_access_key|aws_secret_key|secret_access_key|secretaccesskey)["']?\s*[:=]\s*["']?)[A-Za-z0-9/+=]{40}"#
    )
    .expect("valid aws secret key regex")
});
/// Stripe secret and restricted keys, live and test.
static STRIPE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk|rk)_(?:live|test)_[A-Za-z0-9]{16,}").expect("valid stripe key regex")
});
/// npm access tokens.
static NPM_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bnpm_[A-Za-z0-9]{36}\b").expect("valid npm token regex"));
/// `PyPI` upload tokens (a macaroon, always `pypi-AgEIcHlwaS5vcmc…`).
static PYPI_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bpypi-AgEIcHlwaS5vcmc[A-Za-z0-9_-]{50,}").expect("valid pypi token regex")
});
/// Hugging Face user access tokens.
static HUGGING_FACE_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bhf_[A-Za-z0-9]{34,}\b").expect("valid hugging face token regex")
});
/// `DigitalOcean` personal access tokens.
static DIGITALOCEAN_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bdo[por]_v1_[a-f0-9]{64}\b").expect("valid digitalocean token regex")
});
/// `SendGrid` API keys: `SG.<22>.<43>`.
static SENDGRID_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bSG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}\b").expect("valid sendgrid key regex")
});
/// PEM armour lines, for input that arrives one line at a time.
static PEM_BEGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("valid pem begin regex")
});
static PEM_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----END [A-Z ]*PRIVATE KEY-----").expect("valid pem end regex"));
/// ANSI CSI and OSC escape sequences, as `tmux capture-pane -e` keeps them.
static ANSI_ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b(?:\[[0-9;?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))")
        .expect("valid ansi escape regex")
});
/// GitHub classic (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`) and fine-grained tokens.
static GITHUB_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{80,}")
        .expect("valid github token regex")
});
/// GitLab personal access tokens.
static GITLAB_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"glpat-[A-Za-z0-9_-]{20,}").expect("valid gitlab token regex"));
/// AWS access key ids (long-term `AKIA`, temporary `ASIA`).
static AWS_ACCESS_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b").expect("valid aws key regex"));
/// Google API keys.
static GOOGLE_API_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"AIza[0-9A-Za-z_-]{35}").expect("valid google key regex"));
/// JSON Web Tokens: base64url header and payload, both starting `ey`.
static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"ey[A-Za-z0-9_-]{10,}\.ey[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]*")
        .expect("valid jwt regex")
});
/// A password (or token) in a URL's userinfo: `scheme://user:secret@host`.
/// Group 1 keeps the scheme so a scrubbed clone URL still reads as one.
static URL_USERINFO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z][A-Za-z0-9+.-]*://)[^/\s:@]+:[^/\s@]+@")
        .expect("valid url userinfo regex")
});

/// Every credential shape [`scrub`] removes, by name, in the order it runs.
///
/// PEM runs first so a key block is removed whole before a narrower pattern
/// eats a line of its body, and the Anthropic shape runs before the `sk-` one
/// so an `sk-ant-` key is named for what it is.
fn shapes() -> [(&'static str, &'static Regex); 20] {
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
        ("aws secret key", &AWS_SECRET_KEY),
        ("google api key", &GOOGLE_API_KEY),
        ("stripe key", &STRIPE_KEY),
        ("npm token", &NPM_TOKEN),
        ("pypi token", &PYPI_TOKEN),
        ("hugging face token", &HUGGING_FACE_TOKEN),
        ("digitalocean token", &DIGITALOCEAN_TOKEN),
        ("sendgrid key", &SENDGRID_KEY),
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
///
/// Captured panes keep their colour codes, and an SGR sequence inside a token
/// (a highlighted `.env`, a coloured prompt) splits it past every shape. When
/// the text has escapes and a shape only appears once they are stripped, the
/// stripped text is scrubbed and returned: the colours are lost only on the
/// capture that actually held a secret.
#[must_use]
pub fn scrub(input: &str) -> String {
    let scrubbed = scrub_shapes(input);
    if !input.contains('\x1b') {
        return scrubbed;
    }
    let plain = ANSI_ESCAPE.replace_all(&scrubbed, "");
    if find_secret(&plain).is_some() {
        scrub_shapes(&plain)
    } else {
        scrubbed
    }
}

/// Scrub a sequence of lines that together form one text (a diff, an editor,
/// an argument list), keeping one output line per input line.
///
/// [`scrub`] on each line alone would redact a PEM header and let every base64
/// body line through, so a private-key block is tracked across lines: every
/// line from `BEGIN` to `END` (or to the last line) becomes [`REDACTED`].
#[must_use]
pub fn scrub_lines<S: AsRef<str>>(lines: &[S]) -> Vec<String> {
    let mut in_key = false;
    lines
        .iter()
        .map(|line| {
            let line = line.as_ref();
            if in_key {
                if let Some(end) = PEM_END.find(line) {
                    in_key = false;
                    return format!("{REDACTED}{}", scrub(&line[end.end()..]));
                }
                return REDACTED.to_string();
            }
            match PEM_BEGIN.find(line) {
                Some(begin) if !PEM_END.is_match(&line[begin.end()..]) => {
                    in_key = true;
                    format!("{}{REDACTED}", scrub(&line[..begin.start()]))
                }
                _ => scrub(line),
            }
        })
        .collect()
}

/// Scrub every string value in a JSON document, in place.
///
/// For a structured payload that leaves the process as JSON (a transcript
/// chunk on the wire, #1199). Scrubbing each string on its own, rather than
/// the serialised text, keeps the document valid: an unclosed PEM block in
/// one value ends at that value instead of eating every field after it, and
/// a value is scrubbed as the text it decodes to, not its escaped form.
///
/// Object keys are left as they are. They are the producer's structure
/// (`content`, `rawInput`), not operator text, and rewriting one would change
/// the shape every reader parses against.
pub fn scrub_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if find_secret(text).is_some() {
                *text = scrub(text);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(scrub_json),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(scrub_json),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn scrub_shapes(input: &str) -> String {
    let mut out = std::borrow::Cow::Borrowed(input);
    for (name, re) in shapes() {
        if !re.is_match(&out) {
            continue;
        }
        let replaced = if name == "url userinfo" {
            re.replace_all(&out, format!("${{1}}{REDACTED}@").as_str()).into_owned()
        } else if name == "aws secret key" {
            re.replace_all(&out, format!("${{1}}{REDACTED}").as_str()).into_owned()
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
        // A short `id:value` like an http status or a chat id must NOT be eaten:
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
    fn scrub_json_scrubs_every_string_value_and_keeps_the_shape() {
        let github = fake("ghp_", 'C', 36);
        let pem = format!("-----BEGIN RSA PRIVATE KEY-----\n{}", "M".repeat(64));
        let mut value = serde_json::json!({
            "content": { "text": format!("token {github} here") },
            "argv": ["curl", format!("x-api-key: {}", fake("sk-ant-api03-", 'A', 40))],
            "rawOutput": pem,
            "after": "kept",
            "exitCode": 0,
            "ok": true,
            "none": null,
        });
        scrub_json(&mut value);
        assert_eq!(
            value,
            serde_json::json!({
                "content": { "text": format!("token {REDACTED} here") },
                "argv": ["curl", format!("x-api-key: {REDACTED}")],
                "rawOutput": REDACTED,
                "after": "kept",
                "exitCode": 0,
                "ok": true,
                "none": null,
            }),
            "an unclosed key ends at its own value, and nothing else changes"
        );
        assert_eq!(find_secret(&value.to_string()), None);
    }

    #[test]
    fn scrub_json_leaves_object_keys_alone() {
        let github = fake("ghp_", 'C', 36);
        let mut value = serde_json::json!({ github.clone(): "v" });
        scrub_json(&mut value);
        assert!(
            value.get(&github).is_some(),
            "keys are structure and are not rewritten"
        );
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
            ("stripe key", fake("sk_live_", 'S', 24)),
            ("stripe key", fake("rk_live_", 'R', 24)),
            ("npm token", fake("npm_", 'N', 36)),
            ("pypi token", fake("pypi-AgEIcHlwaS5vcmc", 'P', 60)),
            ("hugging face token", fake("hf_", 'H', 34)),
            ("digitalocean token", fake("dop_v1_", 'a', 64)),
            (
                "sendgrid key",
                format!("{}.{}", fake("SG.", 'G', 22), fake("", 'g', 43)),
            ),
            ("slack token", fake("xoxc-", '1', 40)),
            ("slack token", fake("xoxd-", '2', 40)),
            ("aws secret key", fake("AWS_SECRET_ACCESS_KEY=", 'w', 40)),
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
    fn session_ids_and_branch_names_are_not_mistaken_for_openai_keys() {
        for clean in [
            "ainb-task-123e4567-e89b-12d3-a456-426614174000",
            "agents/fix-risk-assessment-for-the-new-billing-flow-v2",
            "tmux session ainb-disk-cleanup-0123456789abcdef0123456789abcdef",
            "npm_config_cache=/tmp/npm and hf_home=/tmp/hf",
            "risk_live_update and task_test_runner",
        ] {
            assert_eq!(scrub(clean), clean);
            assert!(find_secret(clean).is_none(), "{clean}");
        }
    }

    #[test]
    fn a_key_block_split_across_lines_is_redacted_line_by_line() {
        let body = fake("", 'p', 64);
        let lines = vec![
            "+API=1".to_string(),
            "+-----BEGIN RSA PRIVATE KEY-----".to_string(),
            format!("+{body}"),
            format!("+{body}"),
            "+-----END RSA PRIVATE KEY----- tail".to_string(),
            "+DONE=1".to_string(),
        ];
        let out = scrub_lines(&lines);
        assert_eq!(out.len(), lines.len());
        assert_eq!(out[0], "+API=1");
        assert!(out[1..5].iter().all(|l| l.contains(REDACTED)), "{out:?}");
        assert!(out.iter().all(|l| !l.contains(&body)), "{out:?}");
        assert_eq!(out[4], format!("{REDACTED} tail"));
        assert_eq!(out[5], "+DONE=1");
        // A block the capture cut off redacts to the last line.
        let cut = scrub_lines(&["-----BEGIN OPENSSH PRIVATE KEY-----", &body]);
        assert_eq!(cut, vec![REDACTED.to_string(), REDACTED.to_string()]);
    }

    #[test]
    fn a_token_split_by_colour_codes_is_still_scrubbed() {
        let token = fake("ghp_", 'C', 36);
        let coloured = format!(
            "\x1b[32mexport GH={}\x1b[1m{}\x1b[0m",
            &token[..10],
            &token[10..]
        );
        let scrubbed = scrub(&coloured);
        assert!(!scrubbed.contains(&token[10..]), "{scrubbed:?}");
        assert!(scrubbed.contains(REDACTED));
        // Colour stays when there is nothing to scrub.
        let clean = "\x1b[32mcargo test\x1b[0m";
        assert_eq!(scrub(clean), clean);
    }

    #[test]
    fn an_aws_secret_key_keeps_its_name_and_loses_its_value() {
        let secret = fake("", 'W', 40);
        for line in [
            format!("export AWS_SECRET_ACCESS_KEY={secret}"),
            format!("aws_secret_access_key = {secret}"),
            format!("\"SecretAccessKey\": \"{secret}\""),
        ] {
            let scrubbed = scrub(&line);
            assert!(!scrubbed.contains(&secret), "{scrubbed}");
            assert!(scrubbed.contains(REDACTED), "{scrubbed}");
            assert!(
                scrubbed.to_ascii_lowercase().contains("secret"),
                "the name stays: {scrubbed}"
            );
        }
        // Forty base64 characters with no name are not claimed.
        assert!(find_secret(&secret).is_none());
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
