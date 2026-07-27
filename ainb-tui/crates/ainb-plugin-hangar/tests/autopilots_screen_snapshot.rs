//! P7.5 — autopilot-manager screen render snapshots.
//!
//! Four render behaviours are pinned (insta glyph snapshots, trailing newline
//! trimmed per `reference_insta_trailing_newline_trap`):
//!
//!   * `render_empty_list_shows_help` — the empty list shows the
//!     `No autopilots. Press 'a' to add` help line + the `[a]dd` hint.
//!   * `render_three_autopilots_table` — three rows render aligned, the first
//!     carrying the `▶` selection marker (with a non-vacuous colour check).
//!   * `render_run_history_below_selection` — loading the selected autopilot's
//!     runs renders them in the lower history pane.
//!   * `render_skipped_run_marked` — a `skipped` run is visibly marked in the
//!     history pane.

use ainb_hangar_proto::events::{AutopilotRow, AutopilotRunRow};
use ainb_plugin_hangar::screen::autopilots::{
    AutopilotsEvent, AutopilotsState, reduce_autopilots, render_autopilots,
};
use ainb_plugin_sdk::{Color, WireBuffer};

fn autopilot(id: &str, name: &str, cron: &str, enabled: bool, last: Option<&str>) -> AutopilotRow {
    AutopilotRow {
        id: id.into(),
        workspace_id: "default".into(),
        agent_id: "agent-1".into(),
        name: name.into(),
        cron_expr: cron.into(),
        next_tick_at: Some(1_700_000_300_000),
        enabled,
        last_run_status: last.map(str::to_string),
        last_run_at: last.map(|_| 1_699_000_000_000),
        api_trigger_enabled: false,
    }
}

/// Three autopilots mirroring the P7.5 mockup.
fn three_autopilots() -> Vec<AutopilotRow> {
    vec![
        autopilot(
            "ap-1",
            "daily-triage",
            "0 9 * * 1-5",
            true,
            Some("completed"),
        ),
        autopilot(
            "ap-2",
            "nightly-clean",
            "0 2 * * *",
            true,
            Some("completed"),
        ),
        autopilot(
            "ap-3",
            "weekly-report",
            "0 9 * * MON",
            true,
            Some("skipped"),
        ),
    ]
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

/// The empty list shows the help line + the add hint.
#[test]
fn render_empty_list_shows_help() {
    let state = AutopilotsState::new(Vec::new());
    let mut buf = WireBuffer::new(120, 10);
    render_autopilots(&mut buf, 120, 0, 10, &state);
    let full = glyph_map(&buf, 120);

    assert!(
        full.contains("No autopilots. Press 'a' to add"),
        "empty-list help line missing:\n{full}"
    );
    // The add hint sits next to the control it affects (header row, right-aligned
    // when there is room for it alongside the column headers).
    assert!(full.contains("[a]dd"), "missing `[a]dd` hint:\n{full}");
}

/// Three autopilots render as card-board cards (63l.6), the first selected with
/// the heavy clay highlight border, with the action hints on the header row.
#[test]
fn render_three_autopilots_as_card_board() {
    /// The heavy clay highlight colour of the selected card-board card.
    const CLAY: Color = Color::rgb(210, 130, 90);
    let state = AutopilotsState::new(three_autopilots());
    // A tall board so all three 6-row cards fit above the run-history pane.
    let mut buf = WireBuffer::new(120, 28);
    render_autopilots(&mut buf, 120, 0, 28, &state);
    let full = glyph_map(&buf, 120);

    // POSITIVE: the single `Autopilots (N)` card-board column header + glyph.
    assert!(
        full.contains("Autopilots (3)"),
        "card-board column header missing:\n{full}"
    );
    // Each autopilot renders as a card: its name on the id line, cron + state in
    // the title.
    assert!(
        full.contains("daily-triage"),
        "card 1 name missing:\n{full}"
    );
    assert!(
        full.contains("nightly-clean"),
        "card 2 name missing:\n{full}"
    );
    assert!(
        full.contains("weekly-report"),
        "card 3 name missing:\n{full}"
    );
    assert!(
        full.contains("enabled"),
        "enabled state in card title missing:\n{full}"
    );
    // The card-board paints bordered, rounded cards (the rounded corner glyph is
    // the card-board signature).
    assert!(full.contains('╭'), "rounded card borders missing:\n{full}");
    // The key hints sit next to the controls (keybinding-near-control).
    assert!(full.contains("[a]dd"), "missing [a]dd hint:\n{full}");
    assert!(full.contains("[r]un"), "missing [r]un hint:\n{full}");
    assert!(
        full.contains("[d]isable"),
        "missing [d]isable hint:\n{full}"
    );
    assert!(full.contains("[e]dit"), "missing [e]dit hint:\n{full}");

    // NON-VACUOUS COLOUR CHECK: the selected card carries the heavy clay border.
    let heavy_in_clay =
        buf.cells.iter().any(|(_, cell)| cell.symbol == "┏" && cell.fg == Some(CLAY));
    assert!(
        heavy_in_clay,
        "the selected autopilot card must carry the heavy clay highlight border"
    );
}

/// 63l.7 — the Autopilots board render golden: the single `Autopilots (N)`
/// card-board column of bordered cards, pinned with an `insta` glyph snapshot so
/// a regression to a bare table — or a dropped card / name / state — fails here.
/// Paired with the explicit content + colour assertions in
/// [`render_three_autopilots_as_card_board`] (this golden is the structural pin;
/// that test is the cell-content proof).
#[test]
fn render_autopilots_board_snapshot() {
    let state = AutopilotsState::new(three_autopilots());
    // A tall board so all three 6-row cards fit above the run-history pane.
    let mut buf = WireBuffer::new(120, 28);
    render_autopilots(&mut buf, 120, 0, 28, &state);
    let full = glyph_map(&buf, 120);

    insta::assert_snapshot!(full);

    // Non-vacuous backstop: the golden carries the card-board column header, every
    // autopilot card name, and the rounded-card signature.
    assert!(
        full.contains("Autopilots (3)"),
        "card-board header:\n{full}"
    );
    for name in ["daily-triage", "nightly-clean", "weekly-report"] {
        assert!(full.contains(name), "card {name} missing:\n{full}");
    }
    assert!(full.contains('╭'), "rounded card borders:\n{full}");
}

/// Loading the selected autopilot's runs renders them in the lower history pane.
#[test]
fn render_run_history_below_selection() {
    let state = AutopilotsState::new(three_autopilots());
    // Daemon replies with the selected autopilot's (ap-1) run history.
    let loaded = reduce_autopilots(
        &state,
        AutopilotsEvent::RunsLoaded {
            autopilot_id: "ap-1".into(),
            runs: vec![
                AutopilotRunRow {
                    id: "run-1".into(),
                    autopilot_id: "ap-1".into(),
                    started_at: 1_700_000_120_000,
                    completed_at: Some(1_700_000_300_000),
                    status: "completed".into(),
                    source: "schedule".into(),
                    failure_reason: None,
                },
                AutopilotRunRow {
                    id: "run-2".into(),
                    autopilot_id: "ap-1".into(),
                    started_at: 1_699_999_000_000,
                    completed_at: Some(1_699_999_100_000),
                    status: "completed".into(),
                    source: "schedule".into(),
                    failure_reason: None,
                },
            ],
        },
    )
    .state;

    let mut buf = WireBuffer::new(90, 16);
    render_autopilots(&mut buf, 90, 0, 16, &loaded);
    let full = glyph_map(&buf, 90);

    // POSITIVE: the run-history divider names the selected autopilot, and the
    // run rows render below it.
    assert!(
        full.contains("Recent runs (daily-triage)"),
        "history divider missing:\n{full}"
    );
    assert!(full.contains("completed"), "run status missing:\n{full}");
    assert!(full.contains("1700000120000"), "run start missing:\n{full}");
    // NEGATIVE: the empty-history placeholder is gone now that runs loaded.
    assert!(
        !full.contains("(no runs yet)"),
        "placeholder must be replaced:\n{full}"
    );
    // State accessor agrees.
    assert_eq!(loaded.runs().len(), 2);
}

/// A `skipped` run is visibly marked in the history pane.
#[test]
fn render_skipped_run_marked() {
    /// The warn-red accent skipped/failed runs are painted in.
    const WARN_RED: Color = Color::rgb(220, 120, 100);
    let state = AutopilotsState::new(three_autopilots());
    let loaded = reduce_autopilots(
        &state,
        AutopilotsEvent::RunsLoaded {
            autopilot_id: "ap-1".into(),
            runs: vec![AutopilotRunRow {
                id: "run-x".into(),
                autopilot_id: "ap-1".into(),
                started_at: 1_700_000_000_000,
                completed_at: None,
                status: "skipped".into(),
                source: "schedule".into(),
                failure_reason: None,
            }],
        },
    )
    .state;

    let mut buf = WireBuffer::new(90, 14);
    render_autopilots(&mut buf, 90, 0, 14, &loaded);
    let full = glyph_map(&buf, 90);

    assert!(
        full.contains("skipped"),
        "skipped run must be visible in the history pane:\n{full}"
    );

    // NON-VACUOUS COLOUR CHECK: the skipped run line is painted in the warn-red
    // accent, not the default soft-white.
    let red_skipped = buf
        .cells
        .iter()
        .any(|(_, cell)| cell.symbol == "s" && cell.fg == Some(WARN_RED));
    assert!(
        red_skipped,
        "the skipped run must be marked with the warn-red accent"
    );
}

/// A declined dispatch is READABLE in the history pane: its trigger source and
/// the admission reason render alongside the `skipped` status (migration 0057 /
/// multica parity item 15). Without this the only record of a decline is a log
/// line the operator never sees.
#[test]
fn render_skipped_run_shows_source_and_reason() {
    let state = AutopilotsState::new(three_autopilots());
    let loaded = reduce_autopilots(
        &state,
        AutopilotsEvent::RunsLoaded {
            autopilot_id: "ap-1".into(),
            runs: vec![AutopilotRunRow {
                id: "run-x".into(),
                autopilot_id: "ap-1".into(),
                started_at: 1_700_000_000_000,
                completed_at: Some(1_700_000_000_000),
                status: "skipped".into(),
                source: "api".into(),
                failure_reason: Some("concurrency limit: 1/1 in flight".into()),
            }],
        },
    )
    .state;

    let mut buf = WireBuffer::new(120, 14);
    render_autopilots(&mut buf, 120, 0, 14, &loaded);
    let full = glyph_map(&buf, 120);

    assert!(full.contains("skipped"), "status missing:\n{full}");
    assert!(
        full.contains("api"),
        "the trigger that fired the run must be visible:\n{full}"
    );
    assert!(
        full.contains("concurrency limit"),
        "the admission reason must be visible:\n{full}"
    );
}

/// An armed `api` trigger is visible on the autopilot card, next to the
/// enabled/disabled state — it is a second way the autopilot can fire.
#[test]
fn board_card_shows_the_armed_api_trigger() {
    let mut armed = autopilot("ap-1", "daily-triage", "0 9 * * *", true, Some("completed"));
    armed.api_trigger_enabled = true;
    let unarmed = autopilot(
        "ap-2",
        "nightly-clean",
        "0 3 * * *",
        true,
        Some("completed"),
    );

    let state = AutopilotsState::new(vec![armed, unarmed]);
    let columns = state.board_columns();
    let cards = &columns[0].cards;

    assert!(
        cards[0].title.contains("api"),
        "an armed api trigger must show on the card: {}",
        cards[0].title
    );
    assert!(
        !cards[1].title.contains("api"),
        "an unarmed one must not: {}",
        cards[1].title
    );
}
