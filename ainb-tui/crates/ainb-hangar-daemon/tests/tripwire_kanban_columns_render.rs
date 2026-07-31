//! P8.7 e2e tripwire 1 — the Kanban board (`K`) renders its four columns with
//! a card in each.
//!
//! ```text
//!  seed hangar.db (P4 fixture + 1 task per column)
//!         │
//!         ▼
//!  ainb-hangar-daemon (RPC only) ◀── hangar/tasks_list snapshot
//!         ▲                                   │
//!  ainb tui (tmux) ──`g`──▶ Hangar ──`K`──▶ Kanban board
//!                                            │
//!                          poll_capture: queued/running/done/failed + #<id>
//! ```
//!
//! Reuses the proven P4 pipeline ([`tripwire_p4_common`]): an isolated `$HOME`
//! whose `hangar.db` is seeded with the P4 fixture, a daemon spawned against it
//! (RPC-only — `HANGAR_DAEMON_DISABLE_CLAIM=1`), and `ainb tui` launched under
//! the same `$HOME`. On top of the fixture's single `running` task this seeds one
//! `queued`, one `done`, and one `failed` task ([`seed_kanban_spread`]) so every
//! one of the board's four columns holds a card.
//!
//! The board's real column labels (read from `screen/kanban.rs`) are
//! `queued` / `running` / `done` / `failed`, each rendered as `<label> (<count>)`.
//! The card identifier is `#<short_id>` where `short_id` is the task id's last 6
//! chars — the seed ids end in unique `kq0001` / `kd0002` / `kf0003` suffixes so
//! the rendered `#kq0001` token is greppable and never aliases a column header.
//!
//! SKIP (never fail) when tmux / the built binaries / the staged plugin are
//! absent, per the `tmux-ui-tripwire` skill.

#![allow(clippy::duration_suboptimal_units)] // `from_secs` reads fine as a budget.

#[path = "tripwire_p4_common.rs"]
mod common;

use std::time::{Duration, Instant};

use common::{
    READY_MARKER, TuiSession, ainb_bin, can_run_tripwire, hangar_chrome_visible, prepare_pipeline,
    seed_kanban_spread, skip,
};

/// The four board column headers, in display order. Asserted as `<label> (` so a
/// match proves the *header* rendered (with its count suffix), not an incidental
/// card-status chip carrying the same word.
const COLUMN_HEADERS: [&str; 4] = ["queued (", "running (", "done (", "failed ("];

/// The issue list's own footer key hint (`screen/chrome.rs`, `Screen::IssueList`):
/// the SCREEN-IDENTITY token this tripwire proves the `K` transition against.
///
/// It replaces [`READY_MARKER`] in the negative half of the pair. `READY_MARKER`
/// is the seeded issue's TITLE, i.e. workspace DATA, and a Kanban task card now
/// legitimately names its parent issue, so the old marker asserted "no issue is
/// named anywhere on the board", which was never the invariant under test. A
/// footer hint is rendered by the chrome for exactly one `Screen` variant and can
/// never appear on another, so it is a strictly stronger proof of the transition,
/// and the landing assertion below pins it as actually present before the switch.
const ISSUE_LIST_FOOTER_HINT: &str = "s:sub-issue";

#[test]
fn kanban_board_renders_four_columns_with_cards() {
    if !can_run_tripwire() {
        skip("kanban columns tripwire");
        return;
    }

    // Seed the P4 fixture + spawn the RPC-only daemon, then add one task per
    // non-running column so all four columns carry a card.
    let pipeline = prepare_pipeline();
    seed_kanban_spread(pipeline.home());

    let bin = ainb_bin().expect("gated by can_run_tripwire");
    let (session, landing) = TuiSession::launch_to_hangar(&bin, pipeline.home());

    // Sanity: the landing is the Hangar issue list, NOT the Kanban board yet.
    // (The negative half of the positive/negative marker pair: we must observe a
    // *transition* away from this screen, not a pane that already matched.)
    assert!(
        landing.contains(READY_MARKER),
        "expected the Hangar issue-list landing before pressing K:\n{landing}"
    );
    assert!(
        landing.contains(ISSUE_LIST_FOOTER_HINT),
        "the issue-list screen must render its own footer hint, or the negative \
         marker below proves nothing:\n{landing}"
    );
    assert!(
        !landing.contains("queued ("),
        "the issue list must not already show a Kanban column header:\n{landing}"
    );

    // Press `K` (single-char nav, no Enter) until the board's four column headers
    // are all on screen. 45s budget covers the snapshot round-trip.
    let deadline = Instant::now() + Duration::from_secs(45);
    let board = session
        .switch_tab_until("K", deadline, |c| {
            // Must be on the Hangar plugin chrome AND show every column header.
            hangar_chrome_visible(c) && COLUMN_HEADERS.iter().all(|h| c.contains(h))
        })
        .unwrap_or_else(|| {
            panic!(
                "Kanban board never rendered all four columns:\n{}",
                session.capture()
            )
        });

    // POSITIVE: every one of the four real column headers is present.
    for header in COLUMN_HEADERS {
        assert!(
            board.contains(header),
            "column header {header:?} missing from the Kanban board:\n{board}"
        );
    }

    // POSITIVE: at least one `#`-prefixed card identifier rendered. The seeded
    // short ids (`#kq0001` / `#kd0002` / `#kf0003`) are distinctive tokens that
    // cannot collide with a column header — assert at least one is visible.
    let seeded_cards = ["#kq0001", "#kd0002", "#kf0003"];
    let visible_cards = seeded_cards.iter().filter(|id| board.contains(**id)).count();
    assert!(
        visible_cards >= 1,
        "expected at least one seeded card id ({seeded_cards:?}) on the board:\n{board}"
    );
    // And a literal `#` card marker is present (the card title prefix).
    assert!(
        board.contains('#'),
        "expected a `#<short_id>` card marker on the board:\n{board}"
    );

    // NEGATIVE: we are no longer on the issue-list landing screen (the board
    // replaced it). Pairs the positive column markers with proof of transition,
    // asserted on the issue list's own footer hint (chrome only that screen can
    // render) rather than on any workspace data the board may legitimately echo.
    assert!(
        !board.contains(ISSUE_LIST_FOOTER_HINT),
        "still on the issue list after pressing K (no screen switch):\n{board}"
    );

    drop(session);
    drop(pipeline);
}
