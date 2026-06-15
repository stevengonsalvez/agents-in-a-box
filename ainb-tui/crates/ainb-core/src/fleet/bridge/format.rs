// ABOUTME: Markdown -> Telegram-HTML conversion and 4096-char message splitting.
//
// Ported from the Python `ainb_phone_bridge.format` (the verified spec). Both
// functions are pure. Telegram's HTML parse mode supports a small tag subset;
// we map the common Markdown constructs an agent emits and escape everything
// else so a stray `<` never breaks the message.
//
// The bridge ALWAYS splits the RAW reply (`split_message`) BEFORE converting
// each chunk to HTML (`md_to_tg_html`). That ordering — verified in the Python
// bridge — guarantees a chunk boundary can never slice through an HTML tag or
// entity, which Telegram rejects with a 400.

use std::sync::LazyLock;

use regex::Regex;

/// Telegram hard limit on a single text message.
pub const TG_MAX_LENGTH: usize = 4096;

// Order matters: fenced code blocks first (greedy, multiline), then inline spans.
static FENCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)```[a-zA-Z0-9_+-]*\n?(.*?)```").unwrap());
static INLINE_CODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\n]+?)`").unwrap());
static BOLD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\*\*([^*]+?)\*\*").unwrap());
// Italic: a single `*…*` not adjacent to another `*` or a word char (mirrors the
// Python lookarounds via explicit boundary captures, since `regex` has no
// lookaround). We capture the boundary chars and re-emit them around the tag.
static ITALIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(^|[^*\w])\*([^*\n]+?)\*([^*\w]|$)").unwrap());
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]+)\]\((https?://[^)\s]+)\)").unwrap());

/// HTML-escape the five characters Telegram's HTML parse-mode cares about,
/// matching Python's `html.escape` (which also escapes single quotes).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Convert a Markdown-ish string to the Telegram HTML subset.
///
/// Strategy: pull code spans out behind placeholders so their contents are
/// never re-interpreted as markup, HTML-escape the remainder, apply the inline
/// conversions, then restore the (escaped) code spans wrapped in `<pre>` /
/// `<code>`.
#[must_use]
pub fn md_to_tg_html(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    let mut placeholders: Vec<String> = Vec::new();

    // Placeholder token uses NUL sentinels so it can never appear in real prose.
    let stash = |placeholders: &mut Vec<String>, rendered: String| -> String {
        let token = format!("\u{0}PH{}\u{0}", placeholders.len());
        placeholders.push(rendered);
        token
    };

    // Fenced blocks -> <pre><code>…</code></pre>
    let mut body = String::new();
    let mut last = 0;
    for caps in FENCE_RE.captures_iter(text) {
        let whole = caps.get(0).unwrap();
        body.push_str(&text[last..whole.start()]);
        let inner = html_escape(caps.get(1).map_or("", |m| m.as_str()).trim_end_matches('\n'));
        let token = stash(
            &mut placeholders,
            format!("<pre><code>{inner}</code></pre>"),
        );
        body.push_str(&token);
        last = whole.end();
    }
    body.push_str(&text[last..]);

    // Inline `code` -> <code>…</code>
    let mut body2 = String::new();
    let mut last = 0;
    for caps in INLINE_CODE_RE.captures_iter(&body) {
        let whole = caps.get(0).unwrap();
        body2.push_str(&body[last..whole.start()]);
        let inner = html_escape(caps.get(1).map_or("", |m| m.as_str()));
        let token = stash(&mut placeholders, format!("<code>{inner}</code>"));
        body2.push_str(&token);
        last = whole.end();
    }
    body2.push_str(&body[last..]);

    // Escape the prose body (placeholders survive — they contain no '<').
    let mut text = html_escape(&body2);

    // Links: [label](url) -> <a href="url">label</a>
    text = LINK_RE
        .replace_all(&text, |caps: &regex::Captures| {
            let label = caps.get(1).map_or("", |m| m.as_str());
            let url = html_escape(caps.get(2).map_or("", |m| m.as_str()));
            format!("<a href=\"{url}\">{label}</a>")
        })
        .into_owned();

    // Bold / italic.
    text = BOLD_RE.replace_all(&text, "<b>$1</b>").into_owned();
    text = ITALIC_RE.replace_all(&text, "$1<i>$2</i>$3").into_owned();

    // Restore code placeholders.
    for (i, rendered) in placeholders.iter().enumerate() {
        text = text.replace(&format!("\u{0}PH{i}\u{0}"), rendered);
    }

    text
}

/// Split `text` into chunks no longer than `limit` characters.
///
/// Prefers newline boundaries; falls back to a hard character cut for any single
/// line that is itself longer than the limit. Never returns an empty list — an
/// empty input yields `[""]` so callers can always `send` once.
///
/// Length is measured in `char`s (Unicode scalar values), matching Python's
/// `len(str)` so the 4096 cap is the same code-point budget Telegram enforces.
#[must_use]
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for line in text.split('\n') {
        let line_len = line.chars().count();
        // A single oversized line: flush, then hard-cut it on char boundaries.
        if line_len > limit {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
            let chars: Vec<char> = line.chars().collect();
            for piece in chars.chunks(limit) {
                chunks.push(piece.iter().collect());
            }
            continue;
        }

        // `candidate_len` = current + '\n' + line (the '\n' only when joining).
        let joined_len = if current.is_empty() {
            line_len
        } else {
            current_len + 1 + line_len
        };
        if joined_len > limit {
            chunks.push(std::mem::take(&mut current));
            current.push_str(line);
            current_len = line_len;
        } else {
            if !current.is_empty() {
                current.push('\n');
                current_len += 1;
            }
            current.push_str(line);
            current_len += line_len;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_bare_angle_brackets() {
        assert_eq!(md_to_tg_html("a < b > c"), "a &lt; b &gt; c");
    }

    #[test]
    fn bold_and_italic() {
        assert_eq!(
            md_to_tg_html("**bold** and *italic*"),
            "<b>bold</b> and <i>italic</i>"
        );
    }

    #[test]
    fn inline_code_is_not_reinterpreted() {
        // `<b>` inside code must be escaped, not rendered as a tag.
        assert_eq!(
            md_to_tg_html("use `<b>` here"),
            "use <code>&lt;b&gt;</code> here"
        );
    }

    #[test]
    fn fenced_block_becomes_pre_code() {
        let out = md_to_tg_html("```rust\nlet x = 1 < 2;\n```");
        assert_eq!(out, "<pre><code>let x = 1 &lt; 2;</code></pre>");
    }

    #[test]
    fn link_is_anchored() {
        assert_eq!(
            md_to_tg_html("see [docs](https://example.com/a)"),
            "see <a href=\"https://example.com/a\">docs</a>"
        );
    }

    #[test]
    fn empty_input_is_empty() {
        assert_eq!(md_to_tg_html(""), "");
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(
            split_message("hello", TG_MAX_LENGTH),
            vec!["hello".to_string()]
        );
    }

    #[test]
    fn empty_text_yields_single_empty_chunk() {
        assert_eq!(split_message("", TG_MAX_LENGTH), vec![String::new()]);
    }

    #[test]
    fn splits_on_newline_boundaries() {
        // limit 5: "aaa" + "bbb" can't both fit in one 5-char chunk.
        let chunks = split_message("aaa\nbbb", 5);
        assert_eq!(chunks, vec!["aaa".to_string(), "bbb".to_string()]);
    }

    #[test]
    fn hard_cuts_an_oversized_line() {
        let chunks = split_message("aaaaaaaaaa", 4);
        assert_eq!(
            chunks,
            vec!["aaaa".to_string(), "aaaa".to_string(), "aa".to_string()]
        );
    }

    #[test]
    fn every_chunk_within_limit() {
        let big = "word ".repeat(2000); // ~10k chars
        for chunk in split_message(&big, TG_MAX_LENGTH) {
            assert!(chunk.chars().count() <= TG_MAX_LENGTH);
        }
    }

    #[test]
    fn split_before_convert_never_breaks_a_tag() {
        // The bridge contract: split raw, then convert each chunk. A chunk
        // boundary landing mid-markdown must still produce valid HTML per chunk.
        let raw = format!("{}\n```\ncode < here\n```", "x".repeat(TG_MAX_LENGTH - 2));
        let chunks = split_message(&raw, TG_MAX_LENGTH);
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            let html = md_to_tg_html(chunk);
            // No raw unescaped '<' that isn't part of a tag we emitted.
            assert!(!html.contains("< here") || html.contains("&lt; here"));
        }
    }
}
