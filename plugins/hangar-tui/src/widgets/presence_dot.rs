//! Presence-dot widget — the standalone 3-state `● / ◐ / ○` indicator (P4.8).
//!
//! A tiny reusable cell painter for an agent's presence: green `●` online, amber
//! `◐` unstable (degraded runtime, UX §12.2), gray `○` offline. The same mapping
//! backs the inline dot on the agent-picker rows (P4.5) and the providers section
//! of settings (P4.7); this widget exposes it as a standalone one-cell paint so
//! the chrome and the frosted banner can drop a dot anywhere.
//!
//! Pure render — no state, no IO.

use ainb_plugin_sdk::{Cell, Coord, WireBuffer};
use ainb_hangar_proto::events::PresenceState;

pub use crate::widgets::actor_row::presence_dot;

/// Paint the presence dot for `presence` at `(x, row)`.
pub fn render_presence_dot(buf: &mut WireBuffer, x: u16, row: u16, presence: PresenceState) {
    let (glyph, color) = presence_dot(presence);
    let mut cell = Cell::new(glyph.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each state paints a distinct glyph at the requested cell.
    #[test]
    fn paints_distinct_glyph_per_state() {
        for (presence, expect) in [
            (PresenceState::Online, '●'),
            (PresenceState::Unstable, '◐'),
            (PresenceState::Offline, '○'),
        ] {
            let mut buf = WireBuffer::new(4, 1);
            render_presence_dot(&mut buf, 0, 0, presence);
            let painted = buf.cells.iter().find(|(c, _)| c.x == 0 && c.y == 0);
            assert_eq!(
                painted.and_then(|(_, cell)| cell.symbol.chars().next()),
                Some(expect)
            );
        }
    }
}
