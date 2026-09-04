//! What a `^P <word>` walk costs the socket suites, in one place.
//!
//! Crisp B5 §2.5 demoted nine screens off the tab strip, so five socket tests
//! that used to press one tab key now walk the command palette. The walk is not
//! free: it puts `word.len() + 2` key deliveries on the wire and arms one
//! `hangar/search` per query edit. Left in the pipe, that traffic comes out of
//! the caller's bounded relay budget, and the assertion under test can run out
//! of pumps before its own RPC is ever reached (the ~2/23 flake in
//! `screens_render_from_daemon`).
//!
//! Each caller drains it before asserting. The WALK itself is not shared: the
//! five suites send keys through four different signatures and pump through
//! four different relay helpers, so one common walk would be an abstraction
//! over five shapes. The COST is shared, because that is the part that drifts —
//! it was five copies in three forms, two of them bare literals whose
//! derivation lived only in a comment, so a palette that fired two searches per
//! keystroke would have silently left two of them short.
//!
//! Included with `#[path = "palette_nav_common.rs"] mod palette_nav;`, the same
//! way `tripwire_p4_issue_list_renders.rs` shares `tripwire_p4_common.rs`.

/// Relay rounds one `^P <word>` walk needs to clear: `word.len() + 2` key
/// deliveries (`^P`, the word, Enter), one `hangar/search` per query edit, and
/// two spare.
///
/// Callers loop this count unconditionally rather than stopping at the first
/// round that relays nothing: a round with nothing to relay is one render on a
/// duplex pipe, and an early exit returns before the plugin has queued the next
/// search.
// Compiled as its own (empty) test target as well as into each includer, where
// only some of them may end up using it.
#[allow(dead_code)]
pub fn nav_drain_rounds(word: &str) -> usize {
    2 * word.chars().count() + 4
}
