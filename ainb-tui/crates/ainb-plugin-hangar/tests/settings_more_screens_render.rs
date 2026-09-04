//! Crisp B5 §2.5 — the Settings `More screens` section renders all nine rows.
//!
//! This is the SECOND of the two routes to a screen the tab-strip shrink demoted,
//! and the discoverable one: the palette only helps an operator who already
//! suspects the screen exists. It had no render test — the only committed
//! evidence was a notify-grid snapshot that clips the list after four rows and
//! reads as if only four screens exist.
//!
//! Asserted at the 80×24 floor with the section selected, which is the state an
//! operator reaches by pressing `,` then `j` to the bottom.

use ainb_hangar_proto::settings::HealthSnapshot;
use ainb_plugin_hangar::screen::command_palette::GO_SCREENS;
use ainb_plugin_hangar::screen::settings::{
    SettingsEvent, SettingsSection, SettingsState, reduce_settings, render_settings,
};
use ainb_plugin_sdk::WireBuffer;

const FLOOR_W: u16 = 80;
const FLOOR_H: u16 = 24;

fn health() -> HealthSnapshot {
    HealthSnapshot {
        socket_path: "/tmp/hangar.sock".into(),
        pid: 1,
        uptime_secs: 1,
        version: "0.1.0".into(),
        connected: true,
    }
}

/// `j` to the last section, the way an operator gets there.
fn on_more_screens() -> SettingsState {
    let mut s = SettingsState::new(health(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..10 {
        s = reduce_settings(&s, SettingsEvent::Key('j')).state;
    }
    assert_eq!(
        s.section(),
        SettingsSection::MoreScreens,
        "`j` must clamp on the last section"
    );
    s
}

/// The painted text of `buf`, row-major.
fn painted(buf: &WireBuffer) -> String {
    let mut grid = vec![vec![' '; FLOOR_W as usize]; FLOOR_H as usize];
    for (coord, cell) in &buf.cells {
        if coord.x < FLOOR_W && coord.y < FLOOR_H {
            grid[coord.y as usize][coord.x as usize] = cell.symbol.chars().next().unwrap_or(' ');
        }
    }
    grid.into_iter()
        .map(|r| r.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every demoted screen is listed with the `^P` word that reaches it, at 80×24.
///
/// Derived from `GO_SCREENS`, so the section cannot advertise a word the palette
/// does not match, nor omit one the palette does.
#[test]
fn more_screens_lists_every_demoted_screen_at_the_floor() {
    let mut buf = WireBuffer::new(FLOOR_W, FLOOR_H);
    render_settings(
        &mut buf,
        FLOOR_W,
        FLOOR_H,
        1,
        FLOOR_H - 1,
        &on_more_screens(),
    );
    let text = painted(&buf);

    assert!(
        text.contains("More screens"),
        "the section header must be on screen:\n{text}"
    );
    for (word, screen) in GO_SCREENS {
        let row = format!("^P {word}");
        assert!(
            text.contains(&row),
            "`{row}` ({}) is missing from the section:\n{text}",
            screen.tab_label()
        );
        assert!(
            text.contains(screen.tab_label()),
            "`{row}` has no label beside it:\n{text}"
        );
    }
}

/// Nothing paints outside the 80×24 area, and nothing paints on the chrome rows
/// the caller reserved (row 0 is the tab strip, row 23 the footer).
#[test]
fn more_screens_stays_inside_the_body_band() {
    let mut buf = WireBuffer::new(FLOOR_W, FLOOR_H);
    render_settings(
        &mut buf,
        FLOOR_W,
        FLOOR_H,
        1,
        FLOOR_H - 1,
        &on_more_screens(),
    );
    for (coord, cell) in &buf.cells {
        assert!(
            coord.x < FLOOR_W && coord.y > 0 && coord.y < FLOOR_H - 1,
            "painted {:?} at ({}, {}), outside the body band",
            cell.symbol,
            coord.x,
            coord.y
        );
    }
}
