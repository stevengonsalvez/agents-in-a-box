//! e38.11 — settings Members pane render snapshot.
//!
//! The Members pane is render-only (the mutation surface is CLI-first via
//! `ainb hangar member set-role|remove`). It lists each member as `email · role`;
//! the `owner` role paints in `SELECTION_GREEN` (`rgb(100, 200, 100)`) so the
//! administrator stands out. These tests pin both the visible glyph layout (insta
//! inline) AND the owner-row colour (a direct cell scan), so a regression that
//! drops a member row OR the owner colour fails here.
//!
//! Glyph maps are `trim_end`-ed per line (`reference_insta_trailing_newline_trap`).

use ainb_hangar_proto::settings::HealthSnapshot;
use ainb_hangar_proto::snapshots::MemberWireRow;
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

fn members() -> Vec<MemberWireRow> {
    vec![
        MemberWireRow {
            user_id: "u-amy".into(),
            email: "amy@x.io".into(),
            role: "owner".into(),
        },
        MemberWireRow {
            user_id: "u-bob".into(),
            email: "bob@x.io".into(),
            role: "admin".into(),
        },
    ]
}

/// A state with members loaded, navigated to the Members section.
fn on_members() -> SettingsState {
    let mut s = SettingsState::new(health(), Vec::new(), Vec::new(), Vec::new());
    s.set_members(members());
    while s.section() != SettingsSection::Members {
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

/// The Members pane lists each member as `email · role`; the owner row paints in
/// `SELECTION_GREEN`, the admin row does not.
#[test]
fn tui_renders_members_with_roles() {
    let s = on_members();
    let mut buf = WireBuffer::new(60, 14);
    render_settings(&mut buf, 60, 14, 0, 14, &s);

    let map = glyph_map(&buf, 60);
    // POSITIVE: both members render under the Members section as `email · role`.
    insta::assert_snapshot!(map, @r###"
      Daemon
        /tmp/hangar.sock · ● connected
          Auto-standup: ○ off  ·  [a] toggle
      Providers
      LLM Keys
      Workspaces
    ▶ Members
        amy@x.io · owner
        bob@x.io · admin
      Notifications
        scope: global · [g] toggle
                      phone   web     os      atc
        J/K kind · h/l channel · space toggle
    "###);

    // NON-VACUOUS COLOUR CHECK: the owner row's glyphs paint in `SELECTION_GREEN`.
    let owner_row = row_with(&buf, 60, "amy@x.io").expect("owner row must render");
    let green_owner_glyphs: usize = buf
        .cells
        .iter()
        .filter(|(c, cell)| c.y == owner_row && cell.fg == Some(SELECTION_GREEN))
        .count();
    assert!(
        green_owner_glyphs > 1,
        "the owner member row must paint in `SELECTION_GREEN`"
    );

    // NEGATIVE: the admin row is NOT green (only the owner stands out).
    let admin_row = row_with(&buf, 60, "bob@x.io").expect("admin row must render");
    let green_admin_glyphs: usize = buf
        .cells
        .iter()
        .filter(|(c, cell)| c.y == admin_row && cell.fg == Some(SELECTION_GREEN))
        .count();
    assert_eq!(
        green_admin_glyphs, 0,
        "a non-owner member must not paint in `SELECTION_GREEN`"
    );
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
