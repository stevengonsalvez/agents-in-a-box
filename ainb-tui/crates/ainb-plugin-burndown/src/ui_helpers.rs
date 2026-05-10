//! Plugin-local copy of small UI helpers. ainb-core's
//! `crate::widgets::truncate_with_ellipsis` is duplicated here so the
//! plugin doesn't depend on the host crate.

use std::borrow::Cow;

/// Truncate `s` to fit within `max_len` columns, replacing the tail
/// with an ellipsis when truncation occurs.
pub fn truncate_with_ellipsis(s: &str, max_len: usize) -> Cow<'_, str> {
    if s.chars().count() <= max_len {
        Cow::Borrowed(s)
    } else if max_len == 0 {
        Cow::Borrowed("")
    } else {
        let take = max_len.saturating_sub(1);
        let mut out: String = s.chars().take(take).collect();
        out.push('…');
        Cow::Owned(out)
    }
}
