//! P5.5 RED — settings Workspace pane render snapshot.
//!
//! The Workspace pane renders a `slug · name [default]` table; the active
//! workspace carries a `▶` indicator painted in `SELECTION_GREEN`
//! (`rgb(100, 200, 100)`). These tests pin both the visible glyph layout (insta
//! inline) AND the active-row colour (a direct cell scan), so the snapshot is
//! not vacuous — a regression that drops the `▶` glyph OR the `SELECTION_GREEN`
//! colour fails here.
//!
//! Glyph maps are `trim_end`-ed per line (`reference_insta_trailing_newline_trap`).

use ainb_hangar_proto::settings::{HealthSnapshot, WorkspaceRow};
use ainb_plugin_hangar::screen::settings::{
    SettingsEvent, SettingsSection, SettingsState, reduce_settings, render_settings,
};
use ainb_plugin_sdk::{Color, WireBuffer};

/// `SELECTION_GREEN` from the TUI palette.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

fn health() -> HealthSnapshot {
    HealthSnapshot {
        socket_path: "/tmp/hangar.sock".into(),
        pid: 1,
        uptime_secs: 1,
        version: "0.1.0".into(),
        connected: true,
    }
}

fn workspaces() -> Vec<WorkspaceRow> {
    vec![
        WorkspaceRow {
            id: "01ID_DEFAULT".into(),
            slug: "default".into(),
            name: "Default".into(),
            current: true,
            default: true,
        },
        WorkspaceRow {
            id: "01ID_ACME".into(),
            slug: "acme".into(),
            name: "Acme".into(),
            current: false,
            default: false,
        },
    ]
}

/// Navigate to the Workspaces section.
fn on_workspaces() -> SettingsState {
    let mut s = SettingsState::new(health(), Vec::new(), Vec::new(), workspaces());
    while s.section() != SettingsSection::Workspaces {
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

/// The active workspace renders `▶ default` in `SELECTION_GREEN`; the inactive one
/// has no `▶` and is not green.
#[test]
fn tui_renders_active_indicator() {
    let s = on_workspaces();
    let mut buf = WireBuffer::new(60, 12);
    render_settings(&mut buf, 60, 12, 0, 12, &s);

    let map = glyph_map(&buf, 60);
    // POSITIVE: the active row carries `▶ default` and the [default] tag;
    // the inactive acme row renders without a `▶`.
    insta::assert_snapshot!(map, @"
      Daemon
        /tmp/hangar.sock · ● connected
      Providers
      LLM Keys
    ▶ Workspaces
        ›▶ default · Default [default]
           acme · Acme
      Members
      Notifications
        scope: global · [g] toggle
                      phone   web     os      atc
        J/K kind · h/l channel · space toggle
    ");

    // NON-VACUOUS COLOUR CHECK: the active workspace row's `▶` indicator is
    // painted in `SELECTION_GREEN`. The Workspaces *section header* also uses a
    // `▶` (in gold TITLE), so we specifically assert there exists a `▶` cell
    // whose fg is `SELECTION_GREEN` — that can only be the active workspace row.
    let green_active_indicator = buf
        .cells
        .iter()
        .any(|(_, cell)| cell.symbol == "▶" && cell.fg == Some(SELECTION_GREEN));
    assert!(
        green_active_indicator,
        "the active workspace `▶` indicator must be painted in `SELECTION_GREEN`"
    );
    // And the rest of the active row (slug glyphs) is green too.
    let active_row = green_indicator_row(&buf).expect("a green `▶` row must render");
    let green_glyphs: usize = buf
        .cells
        .iter()
        .filter(|(c, cell)| c.y == active_row && cell.fg == Some(SELECTION_GREEN))
        .count();
    assert!(
        green_glyphs > 1,
        "the active workspace row must paint its label in `SELECTION_GREEN`, not just the marker"
    );

    // NEGATIVE: the inactive acme row has no `▶` glyph.
    let acme_row = row_with(&buf, 60, "acme").expect("acme row must render");
    let acme_glyphs: String = buf
        .cells
        .iter()
        .filter(|(c, _)| c.y == acme_row)
        .map(|(_, cell)| cell.symbol.as_str())
        .collect();
    assert!(
        !acme_glyphs.contains('▶'),
        "inactive workspace must not carry the active indicator: {acme_glyphs}"
    );
}

/// Find the buffer row carrying the `SELECTION_GREEN` `▶` (the active workspace
/// row, distinct from the gold section-header `▶`).
fn green_indicator_row(buf: &WireBuffer) -> Option<u16> {
    buf.cells
        .iter()
        .find(|(_, cell)| cell.symbol == "▶" && cell.fg == Some(SELECTION_GREEN))
        .map(|(c, _)| c.y)
}

/// Find the row index whose glyph line contains `needle`.
fn row_with(buf: &WireBuffer, cols: u16, needle: &str) -> Option<u16> {
    for y in 0..buf.height {
        let line: String = {
            let mut row = vec![' '; cols as usize];
            for (c, cell) in &buf.cells {
                if c.y == y && c.x < cols {
                    if let Some(ch) = cell.symbol.chars().next() {
                        row[c.x as usize] = ch;
                    }
                }
            }
            row.into_iter().collect()
        };
        if line.contains(needle) {
            return Some(y);
        }
    }
    None
}
