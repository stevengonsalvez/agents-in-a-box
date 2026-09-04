//! Crisp B3 §2.4 — Inbox render snapshots: the ONE attention surface.
//!
//! Pins the pane an operator actually reads, with `insta::assert_snapshot!`
//! (trailing newline trimmed per `reference_insta_trailing_newline_trap`):
//!
//!   * `the_full_attention_surface` — an ASK answerable inline above a `recent`
//!     list whose failed run has floated first;
//!   * `narrow_80col` — the same load at the 80-column floor, proving the two
//!     framed blocks and the header chips clip without bleeding;
//!   * `filtered_to_asks` — `f` pressed once: `recent` is gone and the
//!     attention block owns the pane;
//!   * `empty_inbox_and_empty_attention_board` — the cold-start pane, both
//!     blocks framed and saying what is not there.
//!
//! Non-vacuous colour assertions guard the unread amber, the `ERR` code's red
//! and a failed run's red by EQUALITY, independently of the golden text.

use ainb_hangar_proto::events::{AttentionRow, InboxEntryRow};
use ainb_plugin_hangar::screen::control_center::ControlCenterState;
use ainb_plugin_hangar::screen::inbox::{
    InboxIssueRef, InboxLookup, InboxState, InboxTaskRef, colors, render_inbox,
};
use ainb_plugin_sdk::WireBuffer;

/// A fixed render clock so every age in the snapshots is deterministic.
const NOW_MS: i64 = 1_700_000_600_000;

/// The task the finished + live runs in `recent` belong to.
const TASK: &str = "01M1GVN6MAF3121GEDM1E66KW5";
/// Its parent issue.
const ISSUE: &str = "01M1FH6AG5YJF1S";
/// The QA task whose run failed — the row the list floats first.
const QA_TASK: &str = "01M1QAQAQAQAQAQAQAQAQAQAQA";
/// The issue that QA task was working.
const QA_ISSUE: &str = "01M1FH5TICKETSTATS";

fn attention_row(id: &str, cwd: &str, kind: &str, created_at: i64, payload: &str) -> AttentionRow {
    AttentionRow {
        id: id.to_string(),
        session_id: format!("sess-{id}"),
        cwd: cwd.to_string(),
        workspace_id: None,
        kind: kind.to_string(),
        payload: payload.to_string(),
        degraded: false,
        created_at,
        channels: ainb_hangar_proto::ChannelSet::NONE,
    }
}

fn entry(id: &str, event: &str, subject: &str, summary: &str, age_ms: i64) -> InboxEntryRow {
    InboxEntryRow {
        id: id.to_string(),
        kind: "task".into(),
        event: event.to_string(),
        subject_id: subject.to_string(),
        summary: summary.to_string(),
        recipient: "member:me".into(),
        created_at: NOW_MS - age_ms,
        read_at: None,
    }
}

/// The open attention rows: one answerable ASK and one error.
fn attention() -> ControlCenterState {
    let ask = serde_json::json!({
        "kind": "ASK",
        "context": {
            "question": "Decide the Boxtrack sqlite file location",
            "options": [
                { "label": "data/boxtrack.db", "description": "Repo-root data/ dir, outside api/src" },
                { "label": "api/app.db", "description": "Sits next to the api package" },
            ]
        }
    })
    .to_string();
    let mut state = ControlCenterState::default();
    state.set_attention(&[
        attention_row(
            "ask-1",
            "/work/boxtrack",
            "ask_user_question",
            NOW_MS - 40_000,
            &ask,
        ),
        attention_row(
            "err-1",
            "/work/api",
            "error",
            NOW_MS - 180_000,
            r#"{"kind":"ERR","context":{"pattern":"agent_error","snippet":"agent_error exit 65"}}"#,
        ),
    ]);
    state
}

/// The aggregate rows: a failed run, a finished run, a live run, a new issue.
fn inbox() -> InboxState {
    let mut state = InboxState::default();
    state.replace_rows(
        vec![
            entry(
                "ie-1",
                "task_finished",
                TASK,
                &format!("Task finished (Success): {TASK}"),
                2 * 60_000,
            ),
            entry(
                "ie-2",
                "task_started",
                TASK,
                &format!("Task started: {TASK}"),
                4 * 60_000,
            ),
            entry(
                "ie-3",
                "task_finished",
                QA_TASK,
                &format!("Task finished (Failure): {QA_TASK}"),
                9 * 60_000,
            ),
            entry(
                "ie-4",
                "issue_created",
                QA_ISSUE,
                "New issue: Ticket stats: GET /api/tickets/stats",
                12 * 60_000,
            ),
        ],
        4,
        "member:me".into(),
    );
    state
}

/// The names the rows resolve through: impl-1 on HGR-3, qa-1 on HGR-5.
fn lookup() -> InboxLookup {
    InboxLookup {
        tasks: [
            (
                TASK.to_string(),
                InboxTaskRef {
                    agent: "impl-1".into(),
                    issue_id: Some(ISSUE.to_string()),
                },
            ),
            (
                QA_TASK.to_string(),
                InboxTaskRef {
                    agent: "qa-1".into(),
                    issue_id: Some(QA_ISSUE.to_string()),
                },
            ),
        ]
        .into_iter()
        .collect(),
        issues: [
            (
                ISSUE.to_string(),
                InboxIssueRef {
                    display_id: "HGR-3".into(),
                    title: "Add GET /api/version endpoint".into(),
                },
            ),
            (
                QA_ISSUE.to_string(),
                InboxIssueRef {
                    display_id: "HGR-5".into(),
                    title: "Ticket stats: GET /api/tickets/stats".into(),
                },
            ),
        ]
        .into_iter()
        .collect(),
    }
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

fn render(state: &InboxState, cols: u16, rows: u16) -> (WireBuffer, String) {
    let mut buf = WireBuffer::new(cols, rows);
    render_inbox(
        &mut buf,
        cols,
        0,
        rows - 1,
        state,
        &lookup(),
        &attention(),
        NOW_MS,
    );
    let map = glyph_map(&buf, cols);
    (buf, map)
}

#[test]
fn the_full_attention_surface() {
    let (_, full) = render(&inbox(), 100, 24);

    // The header carries both badges and the filter chips.
    assert!(full.contains("[1 need you]"), "needs-you badge:\n{full}");
    assert!(full.contains("[4 unread]"), "unread badge:\n{full}");
    assert!(full.contains("(all) asks"), "the active chip:\n{full}");

    // `needs you`: the ASK is answerable inline, right there.
    assert!(full.contains("▸● ASK  40s"), "the focused ASK row:\n{full}");
    assert!(
        full.contains("Decide the Boxtrack sqlite file location"),
        "the question:\n{full}"
    );
    assert!(
        full.contains("① data/boxtrack.db") && full.contains("② api/app.db"),
        "inline options:\n{full}"
    );
    assert!(full.contains("● ERR  3m"), "the error row:\n{full}");

    // `recent`: the failed run floated above the newer successes (Q10), and the
    // ULIDs are gone.
    let recent: Vec<&str> = full.lines().skip_while(|l| !l.contains("recent")).skip(1).collect();
    assert!(
        recent[0].contains("✗ 9m")
            && recent[0].contains("qa-1 failed")
            && recent[0].contains("HGR-5"),
        "the failed run floats first, named:\n{full}"
    );
    assert!(
        recent[1].contains("● 2m") && recent[1].contains("impl-1 done"),
        "\n{full}"
    );
    assert!(
        recent[2].contains("◔ 4m") && recent[2].contains("impl-1 running"),
        "\n{full}"
    );
    assert!(
        recent[3].contains("+ 12m") && recent[3].contains("new issue"),
        "\n{full}"
    );
    assert!(!full.contains(TASK), "no ULID survives:\n{full}");
    assert!(!full.contains(QA_TASK), "no ULID survives:\n{full}");

    insta::assert_snapshot!(full);
}

#[test]
fn narrow_80col() {
    let (_, full) = render(&inbox(), 80, 24);
    for line in full.lines() {
        assert!(line.chars().count() <= 80, "line over 80 cols: {line:?}");
    }
    assert!(
        full.contains("needs you") && full.contains("recent"),
        "\n{full}"
    );
    insta::assert_snapshot!(full);
}

#[test]
fn filtered_to_asks() {
    let mut state = inbox();
    state.cycle_filter();
    let (_, full) = render(&state, 100, 24);
    assert!(full.contains("(asks)"), "the active chip moved:\n{full}");
    assert!(
        full.contains("needs you"),
        "the attention block stays:\n{full}"
    );
    assert!(
        !full.contains("recent"),
        "`asks` drops the recent block:\n{full}"
    );
    assert!(!full.contains("impl-1 done"), "and its rows:\n{full}");
    insta::assert_snapshot!(full);
}

#[test]
fn unread_rows_are_amber_and_the_error_code_is_red() {
    let (buf, _) = render(&inbox(), 100, 24);

    // An unread `recent` subject is painted unread-amber (non-vacuous).
    let amber = buf.cells.iter().any(|(_, c)| c.fg == Some(colors::UNREAD));
    assert!(amber, "an unread row must be painted unread-amber");

    // The `ERR` code is alert-red, by equality — the one thing on the pane that
    // must not read as ordinary text. A "not the body colours" assertion would
    // pass on muted gray, which is exactly how an error stops looking like one.
    let err_red = buf
        .cells
        .iter()
        .filter(|(_, c)| c.symbol == "E")
        .any(|(_, c)| c.fg == Some(colors::ALERT));
    assert!(err_red, "the ERR code must be painted alert-red");

    // ...and so is the failed run's glyph in `recent`.
    let failed_glyph =
        buf.cells.iter().any(|(_, c)| c.symbol == "✗" && c.fg == Some(colors::ALERT));
    assert!(failed_glyph, "a failed run's ✗ must be painted alert-red");
}

/// The pane with nothing in it at all: both blocks still frame themselves and
/// say so, rather than leaving the operator on a blank screen wondering whether
/// the daemon is down.
#[test]
fn empty_inbox_and_empty_attention_board() {
    let mut buf = WireBuffer::new(100, 24);
    render_inbox(
        &mut buf,
        100,
        0,
        23,
        &InboxState::default(),
        &InboxLookup::default(),
        &ControlCenterState::default(),
        NOW_MS,
    );
    let full = glyph_map(&buf, 100);
    assert!(full.contains("nothing needs you"), "\n{full}");
    assert!(full.contains("no notifications"), "\n{full}");
    assert!(
        !full.contains("need you]"),
        "no badge when nothing is open:\n{full}"
    );
    insta::assert_snapshot!(full);
}
