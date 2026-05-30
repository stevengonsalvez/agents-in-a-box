//! Actor-row widget — polymorphic member/agent row with presence dot.
//!
//! One row in the agent picker (P4.5): a glyph (violet-ringed `⬡` for agents,
//! neutral `👤` for humans), the display name, a subtitle, and a 3-state inline
//! presence dot (`● online` / `◐ unstable` / `○ offline`, UX §12.2 — amber for
//! unstable). Pure render — it takes an [`ActorRow`] and paints it; the owning
//! screen owns selection + filtering.
//!
//! This widget is reused across the picker (P4.5), the issue-list assignee
//! column (P4.3), and the task-detail sidebar assignee field (P4.4), so the same
//! actor renders identically everywhere (Multica visual parity).

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};
use ainb_hangar_proto::events::{ActorRow, PresenceState};

/// Violet ring colour on an agent glyph (Multica UX §12.1 differentiation).
pub const AGENT_VIOLET: Color = Color::rgb(180, 120, 220);
/// Neutral gray for a human glyph.
pub const HUMAN_NEUTRAL: Color = Color::rgb(170, 170, 180);
/// Green presence dot for `Online`.
const ONLINE_GREEN: Color = Color::rgb(100, 200, 100);
/// Amber presence dot for `Unstable` (degraded runtime).
const UNSTABLE_AMBER: Color = Color::rgb(230, 180, 60);
/// Muted gray presence dot for `Offline`.
const OFFLINE_GRAY: Color = Color::rgb(120, 120, 140);
/// Soft-white name text.
const NAME: Color = Color::rgb(220, 220, 230);
/// Muted subtitle text.
const SUBTITLE: Color = Color::rgb(120, 120, 140);
/// Selection accent (the `▶` marker, ainb-tui `SELECTION_GREEN`).
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

/// The glyph rendered for an actor: `⬡` for agents, `@` for humans.
///
/// (`@` rather than the `👤` emoji keeps the glyph a single terminal cell so the
/// row layout stays width-deterministic across terminals.)
#[must_use]
pub const fn actor_glyph(actor: &ActorRow) -> char {
    if actor.is_agent { '⬡' } else { '@' }
}

/// The glyph colour for an actor: violet ring for agents, neutral for humans.
#[must_use]
pub const fn actor_glyph_color(actor: &ActorRow) -> Color {
    if actor.is_agent {
        AGENT_VIOLET
    } else {
        HUMAN_NEUTRAL
    }
}

/// The `(glyph, colour)` for a presence state: `● online` (green), `◐ unstable`
/// (amber), `○ offline` (gray).
#[must_use]
pub const fn presence_dot(presence: PresenceState) -> (char, Color) {
    match presence {
        PresenceState::Online => ('●', ONLINE_GREEN),
        PresenceState::Unstable => ('◐', UNSTABLE_AMBER),
        PresenceState::Offline => ('○', OFFLINE_GRAY),
    }
}

/// The textual presence label (`online` / `unstable` / `offline`).
#[must_use]
pub const fn presence_label(presence: PresenceState) -> &'static str {
    match presence {
        PresenceState::Online => "online",
        PresenceState::Unstable => "unstable",
        PresenceState::Offline => "offline",
    }
}

/// Render one actor row at `(x, row)` within `width` cells: an optional `▶`
/// selection marker, the typed glyph, the name, the subtitle, and a right-aligned
/// presence dot + label.
pub fn render_actor_row(
    buf: &mut WireBuffer,
    x: u16,
    row: u16,
    width: u16,
    actor: &ActorRow,
    selected: bool,
) {
    let right = x.saturating_add(width);
    let mut cx = x;
    // Selection marker.
    let marker = if selected { '▶' } else { ' ' };
    cx = put_char(buf, cx, row, marker, SELECTION_GREEN, right);
    cx = put_char(buf, cx, row, ' ', NAME, right);
    // Glyph in its actor-kind colour.
    cx = put_char(buf, cx, row, actor_glyph(actor), actor_glyph_color(actor), right);
    cx = put_char(buf, cx, row, ' ', NAME, right);
    // Name.
    cx = put_str(buf, cx, row, &actor.display_name, NAME, right);
    cx = put_char(buf, cx, row, ' ', SUBTITLE, right);
    // Subtitle.
    cx = put_str(buf, cx, row, &actor.subtitle, SUBTITLE, right);

    // Right-aligned presence dot + label.
    let (dot, dot_color) = presence_dot(actor.presence);
    let label = presence_label(actor.presence);
    let presence_w = u16::try_from(label.chars().count()).unwrap_or(0) + 2; // dot + space + label
    if let Some(pstart) = right.checked_sub(presence_w) {
        if pstart > cx {
            let mut px = put_char(buf, pstart, row, dot, dot_color, right);
            px = put_char(buf, px, row, ' ', dot_color, right);
            let _ = put_str(buf, px, row, label, dot_color, right);
        }
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at column `right` (exclusive).
/// Returns the next free column. Multi-byte safe.
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        cx = put_char(buf, cx, row, ch, color, right);
    }
    cx
}

/// Write one `ch` at `(x, row)` in `color`, clipping at `right`. Returns the next
/// free column.
fn put_char(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color, right: u16) -> u16 {
    if x >= right {
        return x;
    }
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
    x.saturating_add(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_proto::events::PresenceState;

    fn actor(is_agent: bool, presence: PresenceState) -> ActorRow {
        ActorRow {
            actor_ref: if is_agent { "agent:a".into() } else { "member:m".into() },
            display_name: "name".into(),
            subtitle: "sub".into(),
            presence,
            is_agent,
            recent_rank: None,
        }
    }

    /// Agent glyph is violet-ringed; human glyph is neutral.
    #[test]
    fn glyph_colour_differs_by_kind() {
        assert_eq!(actor_glyph_color(&actor(true, PresenceState::Online)), AGENT_VIOLET);
        assert_eq!(actor_glyph_color(&actor(false, PresenceState::Online)), HUMAN_NEUTRAL);
    }

    /// The three presence states map to three distinct dots + colours.
    #[test]
    fn presence_dots_are_three_distinct_states() {
        let online = presence_dot(PresenceState::Online);
        let unstable = presence_dot(PresenceState::Unstable);
        let offline = presence_dot(PresenceState::Offline);
        assert_ne!(online, unstable);
        assert_ne!(unstable, offline);
        assert_ne!(online, offline);
    }
}
