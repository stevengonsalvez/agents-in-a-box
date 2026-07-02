//! P2 — Control-center screen render snapshots.
//!
//! Pins the agentpeek control-center layout across three states with
//! `insta::assert_snapshot!` (trailing newline trimmed per
//! `reference_insta_trailing_newline_trap`):
//!
//!   * `empty` — the "no sessions need you" placeholder;
//!   * `loaded_20` — twenty mixed-kind cards, so the auto-shuffle (needs-input to
//!     the top) and the selected-card detail pane are both visible;
//!   * `narrow_80` — the same load at the 80-column floor, proving the two-column
//!     split + detail pane clip cleanly without bleeding.
//!
//! A pair of non-vacuous colour assertions guard the status-line palette (the ASK
//! "waiting for your input" amber) and the selection marker green independently of
//! the golden text.

use ainb_hangar_proto::events::AttentionRow;
use ainb_plugin_hangar::screen::control_center::{
    ControlCenterState, colors, render_control_center,
};
use ainb_plugin_sdk::WireBuffer;

/// A fixed render clock so the card ages in the snapshots are deterministic.
const NOW_MS: i64 = 1_000_000;

/// Build one attention row.
fn row(
    id: &str,
    session: &str,
    cwd: &str,
    kind: &str,
    created_at: i64,
    payload: &str,
) -> AttentionRow {
    AttentionRow {
        id: id.to_string(),
        session_id: session.to_string(),
        cwd: cwd.to_string(),
        workspace_id: None,
        kind: kind.to_string(),
        payload: payload.to_string(),
        degraded: false,
        created_at,
    }
}

/// An ASK payload with the given question + option labels.
fn ask(q: &str, opts: &[&str]) -> String {
    let options: Vec<serde_json::Value> =
        opts.iter().map(|l| serde_json::json!({ "label": l })).collect();
    serde_json::json!({ "kind": "ASK", "context": { "question": q, "options": options } })
        .to_string()
}

/// Twenty mixed-kind rows: a handful of ASKs (answerable), some errors, and idle
/// waiters, with staggered `created_at` so the recency tiebreak is exercised.
fn twenty_rows() -> Vec<AttentionRow> {
    let mut rows = Vec::new();
    for i in 0..8 {
        rows.push(row(
            &format!("ask-{i}"),
            &format!("sess-ask-{i}"),
            &format!("/work/proj-{i}"),
            "ask_user_question",
            NOW_MS - (i as i64) * 60_000,
            &ask(&format!("Ship change {i}?"), &["staging", "prod", "hold"]),
        ));
    }
    for i in 0..6 {
        rows.push(row(
            &format!("err-{i}"),
            &format!("sess-err-{i}"),
            &format!("/work/api-{i}"),
            "error",
            NOW_MS - (i as i64) * 120_000,
            &format!(
                r#"{{"kind":"ERR","context":{{"pattern":"rate_limited","snippet":"429 on api-{i}"}}}}"#
            ),
        ));
    }
    for i in 0..6 {
        rows.push(row(
            &format!("idle-{i}"),
            &format!("sess-idle-{i}"),
            &format!("/work/ui-{i}"),
            "waiting",
            NOW_MS - (i as i64) * 300_000,
            &format!(
                r#"{{"kind":"IDLE","context":{{"idle_minutes":{},"last_assistant_text":"finished ui-{i}"}}}}"#,
                (i + 1) * 5
            ),
        ));
    }
    rows
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

#[test]
fn empty_control_center_snapshot() {
    let state = ControlCenterState::default();
    let mut buf = WireBuffer::new(100, 24);
    render_control_center(&mut buf, 100, 0, 23, &state, NOW_MS);
    let full = glyph_map(&buf, 100);
    assert!(full.contains("Control"), "title:\n{full}");
    assert!(
        full.contains("no sessions need you"),
        "empty placeholder:\n{full}"
    );
    insta::assert_snapshot!(full);
}

#[test]
fn loaded_20_cards_snapshot() {
    let mut state = ControlCenterState::default();
    state.set_attention(&twenty_rows());
    let mut buf = WireBuffer::new(100, 30);
    render_control_center(&mut buf, 100, 0, 29, &state, NOW_MS);
    let full = glyph_map(&buf, 100);

    // The header counts every session + the needs-input subset.
    assert!(full.contains("20 sessions"), "session count:\n{full}");
    assert!(full.contains("8 need you"), "needs-input count:\n{full}");
    // An ASK (needs-input) shuffled to the top and its detail pane shows options.
    assert!(
        full.contains("waiting for your input"),
        "ASK status line:\n{full}"
    );
    assert!(full.contains("LAST REPLY"), "last-reply pane:\n{full}");
    assert!(full.contains("TIMELINE"), "timeline pane:\n{full}");
    assert!(full.contains("①"), "circled-digit option glyph:\n{full}");
    insta::assert_snapshot!(full);
}

#[test]
fn narrow_80col_snapshot() {
    let mut state = ControlCenterState::default();
    state.set_attention(&twenty_rows());
    let mut buf = WireBuffer::new(80, 24);
    render_control_center(&mut buf, 80, 0, 23, &state, NOW_MS);
    let full = glyph_map(&buf, 80);
    // Nothing bleeds past the 80-column floor.
    for line in full.lines() {
        assert!(line.chars().count() <= 80, "line over 80 cols: {line:?}");
    }
    assert!(full.contains("Control"), "title at 80col:\n{full}");
    insta::assert_snapshot!(full);
}

#[test]
fn ask_status_line_is_amber_and_selection_marker_is_green() {
    let mut state = ControlCenterState::default();
    state.set_attention(&twenty_rows());
    let mut buf = WireBuffer::new(100, 30);
    render_control_center(&mut buf, 100, 0, 29, &state, NOW_MS);

    // The "▶" selection marker is painted selection-green (non-vacuous).
    let green_marker = buf
        .cells
        .iter()
        .any(|(_, c)| c.symbol == "▶" && c.fg == Some(colors::SELECTION));
    assert!(
        green_marker,
        "the selected card's ▶ marker must be selection-green"
    );

    // The ASK status line's leading 'w' (of "waiting for your input") is amber.
    let amber_status = buf.cells.iter().any(|(_, c)| c.fg == Some(colors::WAIT));
    assert!(
        amber_status,
        "the ASK status line must be painted waiting-amber"
    );
}
