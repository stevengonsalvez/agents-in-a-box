//! What the window will carry between a pane and the platform clipboard.
//!
//! The rule is the pane's own input limit: what the operator pastes is typed
//! into a PTY, so a clipboard holding more than a pane accepts is refused
//! rather than truncated, in both directions and before the platform is asked
//! for anything.

/// The most bytes a paste or a copy carries, the same bound the window puts on
/// one call of typed input.
pub const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// `text` when it is within the limit, `None` when it is over it.
#[must_use]
pub fn within_limit(text: String) -> Option<String> {
    (text.len() <= MAX_CLIPBOARD_BYTES).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::{MAX_CLIPBOARD_BYTES, within_limit};

    #[test]
    fn a_clipboard_over_the_limit_is_refused_and_one_under_it_is_carried() {
        // No display and no clipboard: the rule is the shell's own, checked
        // before the platform is asked.
        let over = "x".repeat(2 * 1024 * 1024);
        assert!(
            within_limit(over).is_none(),
            "a 2 MiB clipboard was carried"
        );

        let at_limit = "y".repeat(MAX_CLIPBOARD_BYTES);
        assert_eq!(within_limit(at_limit.clone()), Some(at_limit));
        assert_eq!(within_limit("ls\r".to_string()), Some("ls\r".to_string()));
    }
}
