//! e38.29 — create/mutate keystroke → RPC → DB → re-render round trips.
//!
//! Almost every other TUI tripwire SQL-seeds state and then asserts a render. The
//! real create/mutate paths (the `c` inline-create on the issue list, the
//! `Shift+→` card-move on the Kanban board) were only reducer-tested — no tripwire
//! proved a keystroke flows
//!
//! ```text
//!  keystroke ──▶ hangar-tui plugin ──RPC──▶ ainb-hangar-daemon ──▶ hangar.db
//!      ▲                                                              │
//!      └────────────── re-render ◀── event push / next snapshot ◀─────┘
//! ```
//!
//! all the way to the DB and back to the screen.
//!
//! Two legs, each pairing a POSITIVE on-screen marker with the **real proof** — a
//! direct read of the daemon's `hangar.db` (the same store the daemon writes) —
//! plus a NEGATIVE pre-condition asserting the DB did NOT already carry the change
//! before the keystroke:
//!
//! 1. **ISSUE CREATE**: open the Hangar issue list, press `c`, type a distinctive
//!    title, submit. NEGATIVE: before submit the DB has no issue with that title.
//!    POSITIVE: the new title renders in the pane AND a row with that title
//!    actually LANDS in `issue` (queried via [`ainb_hangar_store::Store`]).
//! 2. **KANBAN MOVE**: open the Kanban board (`K`), focus a known card in the
//!    `queued` column, `Shift+→` to drag it to `running`. NEGATIVE: before the
//!    move the task's DB status is `queued`. POSITIVE: after the move the task's
//!    `agent_task_queue.status` actually transitioned to `running` in the DB (not
//!    merely re-bucketed on screen).
//!
//! Honours the `tmux-ui-tripwire` HARD RULES via the shared P4 harness
//! ([`tripwire_p4_common`]): exact-name `tmux kill-session` only, `poll_capture`
//! (no bare sleep), single-char nav without `Enter`, POSITIVE paired with a
//! NEGATIVE, and SKIP-not-fail when tmux / the binaries / the staged plugin are
//! missing. `--test-threads=1` (the harness drives one tmux session at a time).

#![allow(clippy::duration_suboptimal_units)] // `from_secs` reads fine as a budget.

#[path = "tripwire_p4_common.rs"]
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{
    READY_MARKER, TuiSession, ainb_bin, can_run_tripwire, hangar_chrome_visible, prepare_pipeline,
    skip,
};

/// The distinctive issue title typed into the create flow — distinctive enough it
/// can never collide with the seeded fixture rows or any chrome string. Kept
/// short on purpose: the host forwards a bounded run of keystrokes to a plugin
/// screen between its periodic background ticks, so a long title can outrun that
/// window. Each char is still sent behind its own echo (the host re-renders the
/// create bar over a plugin-subprocess round-trip per keystroke).
const NEW_ISSUE_TITLE: &str = "ZQX7K";

/// The seeded `queued` card's task id, whose last 6 chars (`mvq001`) render as the
/// card identifier `#mvq001`. Distinctive so the on-screen card marker is
/// greppable and the DB status assertion targets exactly this row.
const MOVE_TASK_ID: &str = "task-move-mvq001";

/// Count `issue` rows carrying `title` in the daemon's `hangar.db` (the real
/// proof an issue create landed — not just rendered).
fn count_issues_with_title(home: &Path, title: &str) -> i64 {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("issue-count runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open issue-count store");
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM issue WHERE title = ?")
            .bind(title)
            .fetch_one(store.pool())
            .await
            .expect("count issues with title")
    })
}

/// Read one task's `status` from the daemon's `agent_task_queue`, or `None` when
/// the id is absent (the real proof a card-move transitioned the task in the DB).
fn task_status(home: &Path, task_id: &str) -> Option<String> {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("task-status runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open task-status store");
        sqlx::query_scalar::<_, String>("SELECT status FROM agent_task_queue WHERE id = ?")
            .bind(task_id)
            .fetch_optional(store.pool())
            .await
            .expect("fetch task status")
    })
}

/// Seed one `queued` task ([`MOVE_TASK_ID`]) on the fixture's
/// `default` workspace / `runtime-1` / `agent-1` so the Kanban board's `queued`
/// column holds a card with a known id to focus and move. Carries no `issue_id`
/// (`NULL`) so the partial-unique pending index can't collide with the fixture's
/// `issue-1` task. Runs after [`prepare_pipeline`].
fn seed_move_card(home: &Path) {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("move-seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir)
            .await
            .expect("open move-seed store");
        sqlx::query(
            "INSERT INTO agent_task_queue \
             (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
             VALUES (?, ?, ?, ?, NULL, 'queued', ?)",
        )
        .bind(MOVE_TASK_ID)
        .bind(ainb_hangar_daemon::seed::WS_ID)
        .bind("runtime-1")
        .bind("agent-1")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .expect("seed move card");
    });
}

#[test]
fn issue_create_keystroke_lands_in_db() {
    if !can_run_tripwire() {
        skip("issue_create round trip");
        return;
    }
    let pipe = prepare_pipeline();
    let home = pipe.home();
    let bin = ainb_bin().expect("gated by can_run_tripwire");

    // NEGATIVE (pre-condition): the DB does NOT already carry the new title — the
    // create flow has not run yet, so this proves the later POSITIVE is the
    // keystroke's doing, not a pre-seeded row.
    assert_eq!(
        count_issues_with_title(home, NEW_ISSUE_TITLE),
        0,
        "issue with the create-flow title already in the DB before the keystroke"
    );

    let (sess, landing) = TuiSession::launch_to_hangar(&bin, home);
    // Sanity: on the issue-list landing, and the new title is not yet on screen.
    assert!(
        landing.contains(READY_MARKER),
        "expected the Hangar issue-list landing:\n{landing}"
    );
    assert!(
        !landing.contains(NEW_ISSUE_TITLE),
        "the create-flow title is on screen before pressing `c`:\n{landing}"
    );

    // Press `c` to open the inline create flow. Re-send until the compose prompt
    // appears (the first frame can race), bounded by a deadline.
    let compose_deadline = Instant::now() + Duration::from_secs(20 * common::budget_scale());
    let opened = loop {
        sess.send_key("c");
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), |c| {
            create_prompt_visible(c)
        }) {
            break Some(c);
        }
        if Instant::now() >= compose_deadline {
            break None;
        }
    };
    assert!(
        opened.is_some(),
        "create-issue compose prompt never appeared after `c`:\n{}",
        sess.capture()
    );

    // Type the distinctive title one char at a time, polling until each growing
    // prefix echoes into the compose bar before sending the next char. The host's
    // event loop re-renders the create bar over a plugin-subprocess round-trip on
    // every keystroke, so a fast burst (per-char OR a single `-l` literal) outruns
    // the loop and drops characters. Pacing each char behind its own echo makes
    // the full-title echo deterministic (no bare sleep — poll-until-reflected per
    // the tripwire HARD RULES).
    let mut typed = String::new();
    for ch in NEW_ISSUE_TITLE.chars() {
        sess.type_literal(&ch.to_string());
        typed.push(ch);
        let prefix = typed.clone();
        sess.poll_capture(
            Instant::now() + Duration::from_secs(10 * common::budget_scale()),
            |c| c.contains(&prefix),
        )
        .unwrap_or_else(|| {
            panic!(
                "create-input never echoed prefix `{prefix}`:\n{}",
                sess.capture()
            )
        });
    }
    // The full title is now echoed in the compose input.
    assert!(
        sess.capture().contains(NEW_ISSUE_TITLE),
        "typed title never fully echoed in the compose input:\n{}",
        sess.capture()
    );

    // Submit (Enter) — this fires the create RPC over the daemon socket.
    sess.send_enter();

    // POSITIVE (the real proof): a row with that title actually LANDS in the
    // daemon DB. Poll the DB (the RPC is async) until the row appears.
    let db_deadline = Instant::now() + Duration::from_secs(20 * common::budget_scale());
    let mut landed = 0;
    while Instant::now() < db_deadline {
        landed = count_issues_with_title(home, NEW_ISSUE_TITLE);
        if landed >= 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        landed >= 1,
        "issue create keystroke never reached the DB (no `{NEW_ISSUE_TITLE}` row in `issue`):\n{}",
        sess.capture()
    );

    // POSITIVE (on screen): the new issue row re-renders in the pane (the daemon's
    // IssueCreated push folds it into the cached rows). Re-press `1` to keep the
    // issue list focused while the snapshot reconciles.
    let rendered = sess
        .switch_tab_until(
            "1",
            Instant::now() + Duration::from_secs(20 * common::budget_scale()),
            |c| c.contains(NEW_ISSUE_TITLE),
        )
        .unwrap_or_else(|| {
            panic!(
                "new issue `{NEW_ISSUE_TITLE}` landed in the DB but never re-rendered:\n{}",
                sess.capture()
            )
        });
    assert!(
        rendered.contains(NEW_ISSUE_TITLE),
        "new issue not visible on the re-rendered list:\n{rendered}"
    );

    drop(sess);
    drop(pipe);
}

#[test]
fn kanban_move_keystroke_transitions_db() {
    if !can_run_tripwire() {
        skip("kanban move round trip");
        return;
    }
    let pipe = prepare_pipeline();
    let home = pipe.home();
    seed_move_card(home);
    let bin = ainb_bin().expect("gated by can_run_tripwire");

    // NEGATIVE (pre-condition): before the move the seeded task is `queued`.
    assert_eq!(
        task_status(home, MOVE_TASK_ID).as_deref(),
        Some("queued"),
        "seeded move card is not `queued` before the move"
    );

    let (sess, landing) = TuiSession::launch_to_hangar(&bin, home);
    assert!(
        landing.contains(READY_MARKER),
        "expected the Hangar issue-list landing before pressing K:\n{landing}"
    );

    // Open the Kanban board (`K`) and wait for the seeded card to render. The card
    // identifier is `#mvq001` (the task id's last 6 chars).
    let card_marker = format!("#{}", &MOVE_TASK_ID[MOVE_TASK_ID.len() - 6..]);
    let board = sess
        .switch_tab_until(
            "K",
            Instant::now() + Duration::from_secs(45 * common::budget_scale()),
            |c| hangar_chrome_visible(c) && c.contains("queued (") && c.contains(&card_marker),
        )
        .unwrap_or_else(|| {
            panic!(
                "Kanban board never rendered the seeded queued card `{card_marker}`:\n{}",
                sess.capture()
            )
        });
    assert!(
        board.contains(&card_marker),
        "seeded card `{card_marker}` missing from the board:\n{board}"
    );

    // The board snaps focus to the first non-empty column. With only the seeded
    // `queued` card present (the fixture's `running` task-1 is also there), focus
    // may land on either column; drive focus to the `queued` column + our card.
    // Press `Left` (focus-left) a few times to reach the leftmost (queued) column,
    // then ensure the focus `▶` marker sits on our card's line.
    let focus_deadline = Instant::now() + Duration::from_secs(15 * common::budget_scale());
    let focused = loop {
        sess.send_key("Left");
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(800), |c| {
            focus_on_card(c, &card_marker)
        }) {
            break Some(c);
        }
        if Instant::now() >= focus_deadline {
            break None;
        }
    };
    let focused = focused.unwrap_or_else(|| {
        panic!(
            "never focused the seeded card `{card_marker}` (no heavy-border focus on `{card_marker}`):\n{}",
            sess.capture()
        )
    });
    assert!(
        focus_on_card(&focused, &card_marker),
        "focus marker not on the seeded card:\n{focused}"
    );

    // Drag the focused card one column right (queued → running): `Shift+→`.
    sess.send_key("S-Right");

    // POSITIVE (the real proof): the task's DB status transitioned to `running`.
    // Poll the DB (the RPC is async) until it flips.
    let db_deadline = Instant::now() + Duration::from_secs(20 * common::budget_scale());
    let mut status = task_status(home, MOVE_TASK_ID);
    while Instant::now() < db_deadline {
        status = task_status(home, MOVE_TASK_ID);
        if status.as_deref() == Some("running") {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        status.as_deref(),
        Some("running"),
        "kanban move keystroke never transitioned the task in the DB \
         (status is {status:?}, want `running`):\n{}",
        sess.capture()
    );

    drop(sess);
    drop(pipe);
}

/// `true` when the issue-list inline-create compose prompt is on screen. The
/// create bar renders a distinctive prompt label so the tripwire can detect the
/// compose mode is active before typing.
fn create_prompt_visible(capture: &str) -> bool {
    capture.contains("New issue") || capture.contains("Title:")
}

/// `true` when the focused card's line carries the given card marker — i.e. that
/// card is the focused one. 63l.6 replaced the old `▶` focus glyph with the
/// card-board's heavy clay border (`┃`, U+2503) drawn around the focused card; an
/// unfocused card uses the light rounded border (`│`, U+2502). So the focused
/// card's id line is `┃#id…┃` while an unfocused one is `│#id…│` — gating on the
/// heavy bar AND the marker on the same line pins it to the focused card.
fn focus_on_card(capture: &str, card_marker: &str) -> bool {
    capture.lines().any(|line| line.contains('┃') && line.contains(card_marker))
}
