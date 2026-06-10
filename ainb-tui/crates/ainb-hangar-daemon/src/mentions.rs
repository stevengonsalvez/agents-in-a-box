//! `@handle` mention parsing over a comment body (e38.7).
//!
//! Multica's collaboration loop lets a user `@`-mention an agent in a comment to
//! trigger a task for that agent (`docs/hangar/research/01-codebase-archaeology.md`:
//! "`@agent_name` mention in a comment triggers a new task run"). This module is
//! the parser half: a pure scan over a comment body that extracts the distinct
//! `@handle` tokens, preserving first-seen order. The *resolution* half (matching
//! a handle to a real agent in the comment's workspace and enqueuing its task)
//! lives in [`crate::rpc::snapshots`], driven from the `comment_add` handler
//! after the comment commits.
//!
//! # Handle grammar
//!
//! A mention is an `@` immediately followed by one or more handle characters:
//! ASCII letters, digits, `-`, or `_`. Agent names like `claude-agent` therefore
//! parse whole. The `@` must start a token boundary (start of the body or after
//! whitespace / a non-handle char), so an email address (`a@b.com`) or a literal
//! `foo@bar` does NOT yield a `bar` mention — the `@` there is preceded by a
//! handle char, not a boundary. A bare `@` with no following handle char is
//! ignored.
//!
//! Parsing is deliberately permissive: it does not validate a handle against the
//! agent roster (the caller does, workspace-scoped), so an unknown `@nobody` is
//! returned here and silently dropped by the resolver — never an error.

/// The handle characters that may follow `@` in a mention: ASCII alphanumerics
/// plus `-` and `_` (agent names like `claude-agent` are valid handles).
const fn is_handle_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Scan `body` for `@handle` mentions, returning the distinct handles in
/// first-seen order (case preserved — the resolver decides case sensitivity).
///
/// A mention is an `@` at a token boundary (body start, or after a non-handle
/// char such as whitespace) followed by ≥1 [handle char](is_handle_char). The
/// leading `@` and the handle are case-preserved verbatim. Duplicates collapse
/// to their first occurrence so `"@bot @bot"` yields one handle, mirroring the
/// resolver's idempotent per-agent enqueue.
///
/// An `@` embedded mid-token (e.g. inside `user@host`) is not a boundary mention
/// and is skipped, so email-shaped text never spawns a phantom mention.
#[must_use]
pub fn parse_mentions(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // A mention's `@` must sit at a token boundary: either the body start or
        // immediately after a character that cannot itself be part of a handle.
        // This stops `user@host` (the `@` follows the handle char `r`) from being
        // read as an `@host` mention.
        let at_boundary = i == 0 || !is_handle_char(chars[i - 1]);
        if chars[i] == '@' && at_boundary {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && is_handle_char(chars[j]) {
                j += 1;
            }
            // Require at least one handle char after `@`; a bare `@` is ignored.
            if j > start {
                let handle: String = chars[start..j].iter().collect();
                if !out.contains(&handle) {
                    out.push(handle);
                }
            }
            i = j.max(start);
            continue;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse_mentions;

    #[test]
    fn extracts_a_single_hyphenated_handle() {
        assert_eq!(
            parse_mentions("@claude-agent please do X"),
            vec!["claude-agent".to_string()]
        );
    }

    #[test]
    fn extracts_multiple_handles_in_first_seen_order() {
        assert_eq!(
            parse_mentions("ping @alpha and @beta then @gamma"),
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
    }

    #[test]
    fn collapses_duplicate_handles_to_first_occurrence() {
        assert_eq!(
            parse_mentions("@bot @bot @bot"),
            vec!["bot".to_string()],
            "a repeated mention yields one handle (resolver is idempotent)"
        );
    }

    #[test]
    fn a_plain_comment_yields_no_handles() {
        assert!(parse_mentions("just a normal comment, no pings").is_empty());
    }

    #[test]
    fn an_email_address_is_not_a_mention() {
        assert!(
            parse_mentions("mail me at alice@example.com").is_empty(),
            "the @ in an email follows a handle char, not a boundary"
        );
    }

    #[test]
    fn a_bare_at_sign_is_ignored() {
        assert!(parse_mentions("just an @ here and @ there").is_empty());
    }

    #[test]
    fn punctuation_terminates_a_handle() {
        assert_eq!(
            parse_mentions("hey @claude-agent, ship it!"),
            vec!["claude-agent".to_string()],
            "the comma is not a handle char so it ends the handle"
        );
    }

    #[test]
    fn underscores_and_digits_are_handle_chars() {
        assert_eq!(parse_mentions("@agent_7 go"), vec!["agent_7".to_string()]);
    }

    #[test]
    fn mention_at_body_start_with_no_leading_space() {
        assert_eq!(parse_mentions("@solo"), vec!["solo".to_string()]);
    }
}
