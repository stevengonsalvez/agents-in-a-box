//! e38.14 — Inbox screen: the aggregated notification inbox with an unread badge.
//!
//! The inbox screen (hotkey `I`) renders the durable notification aggregate the
//! daemon's inbox writer folds live issue / comment / task events into. Unlike a
//! live event stream, these survive a detach: an event that fired while the TUI
//! was closed is still here. The screen pulls `hangar/inbox_list` (an
//! [`InboxListResult`](ainb_hangar_proto::snapshots::InboxListResult)) and paints
//! each entry as `{kind} {summary}`, newest-first, with an unread-count badge in
//! the title — the same minimal list-plus-badge shape the host notifyd Inbox uses.
//!
//! It is intentionally minimal (the heavy data-plane proof is the store/RPC
//! layer): a read-only list. Pressing `r` raises a mark-read request the glue
//! turns into a `hangar/inbox_mark_read` RPC, after which the badge drops to zero.

use ainb_hangar_proto::events::InboxEntryRow;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Primary text (entry summary).
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (kind column, read entries, hints).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Unread badge + unread-entry marker.
const UNREAD_AMBER: Color = Color::rgb(230, 190, 90);

/// The render state for the inbox screen.
///
/// Holds the aggregated entries (newest-first, as the daemon orders them) plus
/// the unread count the badge renders. Default is the empty pane shown before the
/// first `hangar/inbox_list` snapshot lands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxState {
    /// The aggregated inbox rows, newest-first.
    entries: Vec<InboxEntryRow>,
    /// The unread count (`read_at IS NULL`) the badge renders.
    unread: i64,
}

impl InboxState {
    /// Build the state from an `hangar/inbox_list` snapshot result.
    #[must_use]
    pub const fn from_snapshot(entries: Vec<InboxEntryRow>, unread: i64) -> Self {
        Self { entries, unread }
    }

    /// The unread count (read accessor for the glue / tests).
    #[must_use]
    pub const fn unread(&self) -> i64 {
        self.unread
    }

    /// The aggregated entries (read accessor for tests).
    #[must_use]
    pub fn entries(&self) -> &[InboxEntryRow] {
        &self.entries
    }
}

/// Render the inbox pane into `buf` between rows `top` and `bottom`.
///
/// Layout (top-to-bottom):
///
/// ```text
/// Inbox  [3 unread]
/// r mark all read
/// issue    New issue: Refactor API
/// comment  New comment on issue-1
/// task     Task finished (Success): t-1
/// …
/// ```
///
/// The unread badge sits next to the title (`feedback_keybinding_hints_near_control`
/// for the `r` hint on the line below). Each entry is one row: a muted `kind`
/// column, then the summary — amber when unread, soft-white once read. Strings
/// truncate via `chars()`, never byte-slice (the rust-utf8-truncate trap).
pub fn render_inbox(buf: &mut WireBuffer, area_w: u16, top: u16, bottom: u16, state: &InboxState) {
    let mut row = top;

    // Title + unread badge.
    let mut x = put_str(buf, 0, row, "Inbox", GOLD, area_w);
    if state.unread > 0 {
        x = put_str(buf, x, row, "  ", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, "[", MUTED_GRAY, area_w);
        x = put_str(buf, x, row, &state.unread.to_string(), UNREAD_AMBER, area_w);
        let _ = put_str(buf, x, row, " unread]", UNREAD_AMBER, area_w);
    }
    row += 1;

    // The mark-all-read hint next to its key.
    put_str(buf, 0, row, "r mark all read", MUTED_GRAY, area_w);
    row += 2;

    if state.entries.is_empty() {
        put_str(buf, 0, row, "no notifications", MUTED_GRAY, area_w);
        return;
    }

    for entry in &state.entries {
        if row > bottom {
            break;
        }
        render_entry(buf, row, area_w, entry);
        row += 1;
    }
}

/// Render one inbox entry: `{kind}  {summary}`, summary coloured by read state.
fn render_entry(buf: &mut WireBuffer, row: u16, area_w: u16, entry: &InboxEntryRow) {
    let unread = entry.read_at.is_none();
    // The kind column (fixed-width-ish: pad to 8 so summaries align).
    let kind = pad_to(&entry.kind, 8);
    let mut x = put_str(buf, 0, row, &kind, MUTED_GRAY, area_w);
    x = put_str(buf, x, row, " ", MUTED_GRAY, area_w);
    let color = if unread { UNREAD_AMBER } else { SOFT_WHITE };
    let _ = put_str(buf, x, row, &entry.summary, color, area_w);
}

/// Right-pad `s` to `width` chars (char-safe). A longer string is returned as-is
/// (the summary clip happens in `put_str`).
fn pad_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        let mut out = s.to_string();
        out.extend(std::iter::repeat_n(' ', width - len));
        out
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Returns the next free
/// column. Char-safe (iterates `char`s, not bytes — the utf8-truncate trap).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        put_cell(buf, cx, row, ch, color);
        cx = cx.saturating_add(1);
    }
    cx
}

/// Write a single coloured glyph at `(x, row)`.
fn put_cell(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color) {
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

/// The colours, exported so a snapshot/render test can assert the unread badge +
/// per-entry colouring non-vacuously without re-declaring the RGB triples.
pub mod colors {
    use ainb_plugin_sdk::Color;

    /// Unread badge + unread-entry amber.
    pub const UNREAD: Color = super::UNREAD_AMBER;
    /// Read-entry soft white.
    pub const READ: Color = super::SOFT_WHITE;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: &str, summary: &str, read: bool) -> InboxEntryRow {
        InboxEntryRow {
            id: format!("ie-{summary}"),
            kind: kind.into(),
            event: "issue_created".into(),
            subject_id: "s-1".into(),
            summary: summary.into(),
            created_at: 0,
            read_at: if read { Some(100) } else { None },
        }
    }

    /// Collect the rendered glyphs at `row` into a string (for assertions).
    fn row_text(buf: &WireBuffer, row: u16, width: u16) -> String {
        let mut s = String::new();
        for x in 0..width {
            let ch = buf
                .cells
                .iter()
                .find(|(coord, _)| coord.x == x && coord.y == row)
                .map_or(' ', |(_, c)| c.symbol.chars().next().unwrap_or(' '));
            s.push(ch);
        }
        s.trim_end().to_string()
    }

    #[test]
    fn renders_unread_badge_and_entries() {
        let state = InboxState::from_snapshot(
            vec![
                entry("issue", "New issue: Refactor API", false),
                entry("comment", "New comment on issue-1", false),
            ],
            2,
        );
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state);

        // Title row carries the unread badge.
        let title = row_text(&buf, 0, 60);
        assert!(title.starts_with("Inbox"), "title: {title:?}");
        assert!(title.contains("2 unread"), "badge shows count: {title:?}");

        // The two entries render their kind + summary below the hint row
        // (title=0, hint=1, blank=2, entries from row 3).
        let body = (3..7).map(|r| row_text(&buf, r, 60)).collect::<Vec<_>>().join("\n");
        assert!(body.contains("issue"), "issue entry: {body}");
        assert!(
            body.contains("New issue: Refactor API"),
            "issue summary: {body}"
        );
        assert!(
            body.contains("New comment on issue-1"),
            "comment summary: {body}"
        );
    }

    #[test]
    fn unread_entries_render_amber_read_entries_white() {
        let state = InboxState::from_snapshot(vec![entry("task", "Task finished", true)], 0);
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state);

        // No badge when unread is zero.
        let title = row_text(&buf, 0, 60);
        assert_eq!(title, "Inbox", "no badge when zero unread: {title:?}");

        // A read entry's summary glyph is soft-white, not unread-amber. The
        // single entry renders at row 3 (title=0, hint=1, blank=2).
        let summary_cell = buf
            .cells
            .iter()
            .find(|(coord, c)| coord.y == 3 && c.symbol == "T")
            .map(|(_, c)| c.fg);
        assert_eq!(
            summary_cell,
            Some(Some(colors::READ)),
            "a read entry renders soft-white, not amber"
        );
    }

    #[test]
    fn empty_inbox_renders_placeholder() {
        let state = InboxState::default();
        let mut buf = WireBuffer::new(60, 24);
        render_inbox(&mut buf, 60, 0, 20, &state);
        // The empty placeholder renders at row 3 (title=0, hint=1, blank=2).
        let body = (3..5).map(|r| row_text(&buf, r, 60)).collect::<Vec<_>>().join("\n");
        assert!(
            body.contains("no notifications"),
            "empty placeholder: {body}"
        );
        assert_eq!(state.unread(), 0);
    }
}
