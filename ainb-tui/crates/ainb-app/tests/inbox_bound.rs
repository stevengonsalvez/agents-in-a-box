#![allow(missing_docs)]

// ABOUTME: The `inbox` section is bounded by bytes, not by a row count the
// daemon already enforces: the daemon caps `hangar/inbox_list` at 200 rows and
// caps nothing else, so `summary` is the unbounded field. A section past
// `MAX_FRAME_BYTES` is withheld WHOLE, and a withheld inbox is a blank inbox
// with no cut counter to explain it. So the fold keeps fewer rows than the
// daemon sends, cuts every summary with a marker, drops a row whose ids are
// not ids, counts each cut, and the frame proves its bytes.

use ainb_app::AppState;
use ainb_app::SectionId;
use ainb_app::app::sections::{
    INBOX_SUMMARY_CUT_MARKER, MAX_INBOX_BYTES, MAX_INBOX_ID_CHARS, MAX_INBOX_REASON_CHARS,
    MAX_INBOX_ROWS, MAX_INBOX_SUMMARY_CHARS,
};
use ainb_app::wire::frame::{HostId, MAX_FRAME_BYTES};
use ainb_app::wire::section_json;
use ainb_hangar_proto::events::InboxEntryRow;
use ainb_hangar_proto::snapshots::InboxListResult;

const NOW: i64 = 1_700_000_000_000;

fn row(n: usize, summary: &str) -> InboxEntryRow {
    InboxEntryRow {
        id: format!("01J0INBOX{n:017}"),
        kind: "issue".into(),
        event: "issue_created".into(),
        subject_id: format!("issue-{n}"),
        summary: summary.to_string(),
        recipient: "member:me".into(),
        created_at: NOW - n as i64,
        read_at: (n % 2 == 0).then_some(NOW),
    }
}

fn read(rows: Vec<InboxEntryRow>) -> InboxListResult {
    let unread = rows.iter().filter(|r| r.read_at.is_none()).count() as i64;
    InboxListResult {
        entries: rows,
        unread,
    }
}

fn state_with(rows: Vec<InboxEntryRow>) -> AppState {
    let mut state = AppState::new();
    assert!(
        state.apply_inbox_read(read(rows), NOW),
        "the first read changes the section"
    );
    state
}

fn view_of(state: &AppState) -> serde_json::Value {
    section_json(state, SectionId::Inbox, &HostId::local())
}

fn encoded_len(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).expect("frame encodes").len()
}

#[test]
fn the_bounds_sit_below_the_daemons_cap_and_the_frame_ceiling() {
    // The daemon returns at most 200 rows (`INBOX_LIST_LIMIT`); a row cap at
    // or above it would never fire, and the cut path would go untested on a
    // real daemon.
    assert!(
        MAX_INBOX_ROWS < 200,
        "the row cap must sit below the daemon's own"
    );
    assert!(
        MAX_INBOX_BYTES < MAX_FRAME_BYTES,
        "the budget must sit under the ceiling"
    );
}

#[test]
fn rows_past_the_cap_are_cut_and_counted() {
    let rows: Vec<_> = (0..MAX_INBOX_ROWS + 37).map(|n| row(n, "New issue: x")).collect();
    let state = state_with(rows);
    let frame = view_of(&state);
    assert_eq!(
        frame["entries"].as_array().map(Vec::len),
        Some(MAX_INBOX_ROWS)
    );
    assert_eq!(frame["rows_cut"], 37);
    assert_eq!(frame["summaries_cut"], 0);
    // Newest first is the daemon's order and the cut keeps the head, so the
    // rows that survive are the newest ones.
    assert_eq!(frame["entries"][0]["id"], "01J0INBOX00000000000000000");
}

#[test]
fn a_long_summary_is_cut_with_a_marker_and_counted() {
    let long = "x".repeat(MAX_INBOX_SUMMARY_CHARS + 500);
    let state = state_with(vec![row(0, &long), row(1, "short")]);
    let frame = view_of(&state);
    let cut = frame["entries"][0]["summary"].as_str().expect("summary");
    assert!(
        cut.ends_with(INBOX_SUMMARY_CUT_MARKER),
        "cut summary carries the marker: {cut}"
    );
    assert_eq!(
        cut.chars().count(),
        MAX_INBOX_SUMMARY_CHARS + INBOX_SUMMARY_CUT_MARKER.chars().count()
    );
    assert_eq!(frame["entries"][1]["summary"], "short");
    assert_eq!(frame["summaries_cut"], 1);
    assert_eq!(frame["rows_cut"], 0);
}

#[test]
fn a_multibyte_summary_is_cut_on_a_char_boundary() {
    let long = "é".repeat(MAX_INBOX_SUMMARY_CHARS + 3);
    let state = state_with(vec![row(0, &long)]);
    let cut = view_of(&state)["entries"][0]["summary"].as_str().unwrap().to_string();
    assert!(cut.starts_with(&"é".repeat(MAX_INBOX_SUMMARY_CHARS)));
    assert!(cut.ends_with(INBOX_SUMMARY_CUT_MARKER));
}

#[test]
fn a_row_whose_id_is_not_an_id_is_dropped_and_counted() {
    let mut long = row(0, "ok");
    long.subject_id = "i".repeat(MAX_INBOX_ID_CHARS + 1);
    let mut text = row(2, "ok");
    text.recipient = "member:me <token sk-abcdefghijklmnopqrstuvwxyz0123456789>".into();
    let mut control = row(3, "ok");
    control.event = "issue\u{1b}[31mcreated".into();
    let state = state_with(vec![long, text, control, row(1, "kept")]);
    let frame = view_of(&state);
    assert_eq!(frame["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(frame["entries"][0]["summary"], "kept");
    assert_eq!(frame["rows_cut"], 3);
}

#[test]
fn a_long_reason_is_scrubbed_and_cut_and_the_frame_stays_inside_the_budget() {
    // The daemon's error text is as long as the client accepts (4 MiB);
    // a reason that blanked the section is the failure the budget stops.
    let reason = format!(
        "connect: sk-{} {}",
        "k".repeat(48),
        "e".repeat(4 * 1024 * 1024)
    );
    let mut state = AppState::new();
    assert!(state.inbox_absent(reason.clone()));
    let frame = view_of(&state);
    let absent = frame["absent"].as_str().unwrap();
    assert!(absent.ends_with(INBOX_SUMMARY_CUT_MARKER));
    assert!(absent.chars().count() <= MAX_INBOX_REASON_CHARS + INBOX_SUMMARY_CUT_MARKER.len());
    assert!(
        !absent.contains(&"k".repeat(48)),
        "the key survived: {absent}"
    );
    assert!(encoded_len(&frame) < MAX_INBOX_BYTES);
    // The same for a failure after rows landed.
    let mut state = state_with(vec![row(0, "a")]);
    assert!(state.inbox_read_failed(reason));
    let frame = view_of(&state);
    assert!(frame["unreachable"].as_str().unwrap().ends_with(INBOX_SUMMARY_CUT_MARKER));
    assert!(encoded_len(&frame) < MAX_INBOX_BYTES);
}

#[test]
fn the_budget_is_enforced_on_the_encoded_frame_not_only_by_the_fold() {
    // Rows placed on the section directly, past what the fold would keep, so
    // the frame's own check is what holds the line.
    let mut state = AppState::new();
    state.inbox.update(|section| {
        section.entries = (0..MAX_INBOX_ROWS).map(|n| row(n, &"s".repeat(32 * 1024))).collect();
        section.unread = 1;
        true
    });
    let frame = view_of(&state);
    let bytes = encoded_len(&frame);
    assert!(bytes < MAX_INBOX_BYTES, "framed {bytes} bytes");
    let kept = frame["entries"].as_array().unwrap().len();
    assert!(kept < MAX_INBOX_ROWS, "some rows were cut: {kept}");
    assert_eq!(frame["rows_cut"], MAX_INBOX_ROWS - kept);
}

#[test]
fn the_worst_case_read_frames_inside_the_budget_and_says_what_it_cut() {
    // 200 rows (the daemon's cap) of 64 KiB summaries full of control
    // characters, which encode at six bytes each, with every id at its cap.
    let noisy: String = "\u{1}".repeat(64 * 1024);
    let rows: Vec<_> = (0..200)
        .map(|n| {
            let mut r = row(n, &noisy);
            r.id = "a".repeat(MAX_INBOX_ID_CHARS);
            r.subject_id = "b".repeat(MAX_INBOX_ID_CHARS);
            r.kind = "c".repeat(MAX_INBOX_ID_CHARS);
            r.event = "d".repeat(MAX_INBOX_ID_CHARS);
            r.recipient = "e".repeat(MAX_INBOX_ID_CHARS);
            r
        })
        .collect();
    let state = state_with(rows);
    let frame = view_of(&state);
    let bytes = encoded_len(&frame);
    assert!(
        bytes < MAX_INBOX_BYTES,
        "the worst case framed {bytes} bytes, over the budget of {MAX_INBOX_BYTES}"
    );
    assert_eq!(frame["rows_cut"], 200 - MAX_INBOX_ROWS);
    assert_eq!(frame["summaries_cut"], MAX_INBOX_ROWS);
}

#[test]
fn a_credential_past_the_cut_does_not_survive_and_the_marker_draws() {
    // Scrub runs before the cut: a token whose head is inside the cut and whose
    // tail is past it would otherwise frame as a plausible-looking prefix.
    let token = format!("{}.{}.{}", "A".repeat(30), "B".repeat(8), "C".repeat(40));
    // The token starts inside the cut window and ends past it; the text
    // after it is what makes the cut happen once the token has shrunk to
    // the redaction marker.
    let summary = format!("{} {token} {}", "x".repeat(200), "y".repeat(400));
    let state = state_with(vec![row(0, &summary)]);
    let cut = view_of(&state)["entries"][0]["summary"].as_str().unwrap().to_string();
    assert!(
        !cut.contains(&"A".repeat(30)),
        "the token's head survived the frame: {cut}"
    );
    assert!(
        !cut.contains("AAAA"),
        "no fragment of the token survives the cut: {cut}"
    );
    assert!(
        cut.contains("<redacted>"),
        "the redaction marker is drawn: {cut}"
    );
    assert!(cut.ends_with(INBOX_SUMMARY_CUT_MARKER));
}

#[test]
fn a_credential_inside_the_cut_is_scrubbed_on_the_frame() {
    let summary = format!("Task started: sk-{}", "k".repeat(48));
    let state = state_with(vec![row(0, &summary)]);
    let drawn = view_of(&state)["entries"][0]["summary"].as_str().unwrap().to_string();
    assert!(
        !drawn.contains(&"k".repeat(48)),
        "the key survived: {drawn}"
    );
}

#[test]
fn the_read_folds_unread_and_a_repeat_read_changes_nothing() {
    let mut state = state_with(vec![row(0, "a"), row(1, "b"), row(3, "c")]);
    let before = view_of(&state);
    assert_eq!(before["unread"], 2);
    assert_eq!(before["absent"], serde_json::Value::Null);
    assert!(
        !state.apply_inbox_read(read(vec![row(0, "a"), row(1, "b"), row(3, "c")]), NOW + 1),
        "the same rows again is not a change"
    );
}

#[test]
fn a_failed_read_keeps_the_rows_and_says_the_host_is_unreachable() {
    let mut state = state_with(vec![row(0, "a")]);
    assert!(state.inbox_read_failed("daemon io: broken pipe"));
    let frame = view_of(&state);
    assert_eq!(frame["entries"].as_array().map(Vec::len), Some(1));
    assert_eq!(frame["unreachable"], "daemon io: broken pipe");
    assert_eq!(frame["absent"], serde_json::Value::Null);
    // A read landing again clears it.
    assert!(state.apply_inbox_read(read(vec![row(0, "a"), row(2, "b")]), NOW + 6));
    assert_eq!(view_of(&state)["unreachable"], serde_json::Value::Null);
}

#[test]
fn an_absent_daemon_frames_the_reason_and_no_rows() {
    let mut state = AppState::new();
    assert!(state.inbox_absent("daemon has no hangar/inbox_list"));
    let frame = view_of(&state);
    assert_eq!(frame["absent"], "daemon has no hangar/inbox_list");
    assert_eq!(frame["entries"].as_array().map(Vec::len), Some(0));
    assert!(
        !state.inbox_absent("daemon has no hangar/inbox_list"),
        "same reason, no change"
    );
}

#[test]
fn mark_all_read_folds_the_daemons_count_and_never_stamps_a_local_clock() {
    let mut state = state_with(vec![row(1, "a"), row(3, "b"), row(0, "c")]);
    assert_eq!(view_of(&state)["unread"], 2);
    assert!(state.apply_inbox_mark_all_read(0));
    let frame = view_of(&state);
    assert_eq!(frame["unread"], 0);
    // The stamps are the daemon's: they arrive with the read after the sweep.
    assert_eq!(
        frame["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["read_at"].is_i64())
            .count(),
        1,
        "no row was stamped locally: {frame}"
    );
    let mut after = vec![row(1, "a"), row(3, "b"), row(0, "c")];
    for r in &mut after {
        r.read_at = Some(NOW + 9);
    }
    assert!(state.apply_inbox_read(read(after), NOW + 10));
    let frame = view_of(&state);
    assert!(frame["entries"].as_array().unwrap().iter().all(|e| e["read_at"] == NOW + 9));
}

