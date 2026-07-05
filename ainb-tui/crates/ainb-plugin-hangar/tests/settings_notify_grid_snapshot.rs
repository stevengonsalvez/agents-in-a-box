//! tcp T5 — settings Notifications routing-grid render snapshot.
//!
//! The Notifications pane is a workspace-scoped grid of attention kind × push
//! channel toggles. Each row is a kind; each cell is ●/○ for whether the rule
//! routes that kind to that channel. The active cursor cell is bracketed so the
//! 2D position reads without colour; a hint line names the keys. These tests pin
//! the glyph layout (insta inline) AND the cursor-cell accent (a direct cell
//! scan), so a regression that drops a row, a cell, or the cursor fails here.
//!
//! Glyph maps are `trim_end`-ed per line (`reference_insta_trailing_newline_trap`).

use ainb_hangar_proto::settings::HealthSnapshot;
use ainb_hangar_proto::snapshots::NotifyRuleWireRow;
use ainb_hangar_proto::{Channel, ChannelSet};
use ainb_plugin_hangar::screen::settings::{
    reduce_settings, render_settings, SettingsEvent, SettingsSection, SettingsState,
};
use ainb_plugin_sdk::{Color, WireBuffer};

/// The cursor-cell accent colour used by `render_notify_grid`.
const CURSOR: Color = Color::rgb(255, 215, 0);

fn health() -> HealthSnapshot {
    HealthSnapshot {
        socket_path: "/tmp/hangar.sock".into(),
        pid: 1,
        uptime_secs: 1,
        version: "0.1.0".into(),
        connected: true,
    }
}

fn rules() -> Vec<NotifyRuleWireRow> {
    vec![
        NotifyRuleWireRow {
            kind: "ask_user_question".into(),
            channels: ChannelSet::from_channels([Channel::Web, Channel::Os]),
            overridden: false,
        },
        NotifyRuleWireRow {
            kind: "error".into(),
            channels: ChannelSet::from_channels([Channel::Os]),
            overridden: false,
        },
        NotifyRuleWireRow {
            kind: "waiting".into(),
            channels: ChannelSet::NONE,
            overridden: false,
        },
    ]
}

/// A state on the Notifications section with the grid loaded.
fn on_notifications() -> SettingsState {
    let mut s = SettingsState::new(health(), Vec::new(), Vec::new(), Vec::new());
    s.set_notify_rules(rules());
    while s.section() != SettingsSection::Notifications {
        s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    }
    s
}

fn glyph_map(buf: &WireBuffer, cols: u16) -> String {
    let mut grid = vec![vec![' '; cols as usize]; buf.height as usize];
    for (coord, cell) in &buf.cells {
        if coord.y < buf.height && coord.x < cols {
            if let Some(ch) = cell.symbol.chars().next() {
                grid[coord.y as usize][coord.x as usize] = ch;
            }
        }
    }
    grid.into_iter()
        .map(|r| r.into_iter().collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .to_string()
}

/// The grid renders one ●/○ row per kind, a channel header, and the key hints,
/// with the ASK row's web+os cells ● and its phone/atc cells ○.
#[test]
fn tui_renders_notify_routing_grid() {
    let s = on_notifications();
    let mut buf = WireBuffer::new(60, 20);
    render_settings(&mut buf, 60, 20, 0, 20, &s);

    let map = glyph_map(&buf, 60);
    insta::assert_snapshot!(map);

    // NON-VACUOUS: the cursor starts on (ask, phone) — that cell paints in the
    // CURSOR accent (bracketed ○, since ask is not routed to phone by default).
    let cursor_glyphs: usize = buf
        .cells
        .iter()
        .filter(|(_, cell)| cell.fg == Some(CURSOR))
        .count();
    assert!(cursor_glyphs > 0, "the cursor cell must paint in the accent colour");
}

/// Moving the cursor down a kind and right a channel moves the accent onto the
/// (error, web) cell — proving the 2D cursor drives the render.
#[test]
fn tui_cursor_accent_follows_the_2d_cursor() {
    let mut s = on_notifications();
    s = reduce_settings(&s, SettingsEvent::Key('J')).state; // -> error row
    s = reduce_settings(&s, SettingsEvent::Key('l')).state; // -> web column
    assert_eq!(s.notify_cursor(), (1, 1));

    let mut buf = WireBuffer::new(60, 20);
    render_settings(&mut buf, 60, 20, 0, 20, &s);

    // The CURSOR-accented bracket glyph `[` appears exactly once (the single active
    // cell). Filter by the accent colour so the scope line's muted `[g]` hint
    // (agents-in-a-box-cqh) is never miscounted as a second cursor.
    let brackets = buf
        .cells
        .iter()
        .filter(|(_, cell)| cell.symbol == "[" && cell.fg == Some(CURSOR))
        .count();
    assert_eq!(brackets, 1, "exactly one cell is the bracketed cursor");
}
