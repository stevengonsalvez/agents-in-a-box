//! P8.6 — Logs pane render snapshot + per-level colour assertions.
//!
//! Seeds three P8.1-shaped log lines (one each at INFO / WARN / ERROR) parsed
//! through the shared [`ainb_hangar_core::logs`] reader, renders [`LogsState`]
//! to a backing buffer, and pins the layout with `insta::assert_snapshot!`
//! (trailing newline trimmed per `reference_insta_trailing_newline_trap`).
//!
//! Three independent, non-vacuous assertions:
//!   * every seeded message + its `k=v` field tail is visible (the read path);
//!   * each level token is painted its own colour (INFO blue / WARN amber /
//!     ERROR red) — the colour dimension is verifiable per level;
//!   * the active level-filter chip is highlighted and a `--level` floor drops
//!     the below-floor line (the filter dimension).

use ainb_hangar_core::logs::{LogLevel, LogLine};
use ainb_plugin_hangar::screen::logs::{colors, render_logs, LogsState};
use ainb_plugin_sdk::{Color, WireBuffer};

/// The three seeded P8.1-shaped JSON log lines (INFO / WARN / ERROR).
const SEED: [&str; 3] = [
    r#"{"timestamp":"2026-05-31T12:00:00.000001Z","level":"INFO","target":"ainb_hangar_daemon","fields":{"message":"daemon ready","task_id":"t-aaa"}}"#,
    r#"{"timestamp":"2026-05-31T12:00:01.000002Z","level":"WARN","target":"ainb_hangar_daemon::run_loop","fields":{"message":"claim slot retry","attempts":2}}"#,
    r#"{"timestamp":"2026-05-31T12:00:02.000003Z","level":"ERROR","target":"ainb_hangar_daemon::runner","fields":{"message":"provider exited nonzero","code":7}}"#,
];

/// Parse the seed lines through the shared reader.
fn seeded_lines() -> Vec<LogLine> {
    SEED.iter().filter_map(|l| LogLine::parse(l)).collect()
}

/// Flatten the buffer into a `\n`-joined glyph map, each line `trim_end`-ed and
/// the whole map trailing-newline-trimmed (insta trap).
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

/// Whether any cell holding glyph `ch` is painted `color`.
fn glyph_has_color(buf: &WireBuffer, ch: char, color: Color) -> bool {
    buf.cells
        .iter()
        .any(|(_, cell)| cell.symbol.starts_with(ch) && cell.fg == Some(color))
}

#[test]
fn render_logs_pane_snapshot() {
    let state = LogsState::from_lines(seeded_lines());
    let mut buf = WireBuffer::new(90, 12);
    render_logs(&mut buf, 90, 0, 12, &state);
    let full = glyph_map(&buf, 90);

    // Title + chips.
    assert!(full.contains("Logs"), "title:\n{full}");
    assert!(full.contains("all"), "chip 'all':\n{full}");
    assert!(full.contains("info"), "chip 'info':\n{full}");
    assert!(full.contains("warn"), "chip 'warn':\n{full}");
    assert!(full.contains("error"), "chip 'error':\n{full}");

    // Every seeded message is visible.
    assert!(full.contains("daemon ready"), "INFO message:\n{full}");
    assert!(full.contains("claim slot retry"), "WARN message:\n{full}");
    assert!(
        full.contains("provider exited nonzero"),
        "ERROR message:\n{full}"
    );

    // Level tokens + clock-trimmed timestamps + k=v tail.
    assert!(full.contains("INFO"), "INFO token:\n{full}");
    assert!(full.contains("12:00:00"), "trimmed timestamp:\n{full}");
    assert!(full.contains("task_id=t-aaa"), "string field tail:\n{full}");
    assert!(full.contains("attempts=2"), "numeric field tail:\n{full}");

    insta::assert_snapshot!(full);
}

#[test]
fn each_level_token_is_painted_its_own_colour() {
    let state = LogsState::from_lines(seeded_lines());
    let mut buf = WireBuffer::new(90, 12);
    render_logs(&mut buf, 90, 0, 12, &state);

    // The level tokens lead with distinct first chars: INFO→'I', WARN→'W',
    // ERROR→'E'. Each must carry its own colour somewhere in the buffer.
    assert!(
        glyph_has_color(&buf, 'I', colors::INFO),
        "the INFO token must be painted INFO blue"
    );
    assert!(
        glyph_has_color(&buf, 'W', colors::WARN),
        "the WARN token must be painted WARN amber"
    );
    assert!(
        glyph_has_color(&buf, 'E', colors::ERROR),
        "the ERROR token must be painted ERROR red"
    );
    // Non-vacuity: the three level colours are distinct.
    assert_ne!(colors::INFO, colors::WARN);
    assert_ne!(colors::WARN, colors::ERROR);
    assert_ne!(colors::INFO, colors::ERROR);
}

#[test]
fn active_chip_is_highlighted_and_filter_drops_below_floor() {
    let mut state = LogsState::from_lines(seeded_lines());
    // Press `w` → floor at WARN; the chip change is reported, INFO line drops.
    assert!(state.handle_key('w'), "'w' changes the filter");
    assert_eq!(state.filter(), Some(LogLevel::Warn));

    let mut buf = WireBuffer::new(90, 12);
    render_logs(&mut buf, 90, 0, 12, &state);
    let full = glyph_map(&buf, 90);

    // The INFO line is filtered out; WARN + ERROR remain.
    assert!(
        !full.contains("daemon ready"),
        "INFO line should be hidden at the WARN floor:\n{full}"
    );
    assert!(
        full.contains("claim slot retry"),
        "WARN line remains:\n{full}"
    );
    assert!(
        full.contains("provider exited nonzero"),
        "ERROR line remains:\n{full}"
    );

    // The active `warn` chip is highlighted; a chip's label glyph carries the
    // active-chip colour ('w' in "warn").
    assert!(
        glyph_has_color(&buf, 'w', colors::ACTIVE_CHIP),
        "the active 'warn' chip must be highlighted:\n{full}"
    );

    // Pressing the same key again is a no-op (no spurious refresh).
    assert!(
        !state.handle_key('w'),
        "re-pressing the active chip is a no-op"
    );
    // `a` returns to all.
    assert!(state.handle_key('a'), "'a' clears the filter");
    assert_eq!(state.filter(), None);
}
