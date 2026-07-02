//! P2 — control-center tripwire: a real `ainb tui`, dialled to a real
//! `ainb-hangar-daemon`, renders the fleet-wide attention board with the
//! auto-shuffle + inline ①②③ options the spec (P2, D9) promises.
//!
//! Companion to `crates/ainb-plugin-hangar/tests/attention_answer_over_socket.rs`
//! (which drives the plugin behind a MOCK daemon and proves the `1` key issues
//! `attention/answer`). This tripwire proves the other half end-to-end through
//! the REAL binaries + a REAL `hangar.db`: two open `attention` rows are seeded
//! directly into the database before the daemon boots (an older `waiting` row
//! and a NEWER `ask_user_question` row), so the daemon's very first
//! `attention/subscribe` snapshot already carries both. Pressing `C` from the
//! seeded issue-list landing screen must render:
//!
//! - the title's `N sessions` / `M need you` counts,
//! - the ASK card ABOVE the WAIT card despite being the OLDER row — proving the
//!   D9 urgency-rank shuffle ([`sort_cards`] in `control_center.rs`), not a
//!   recency coincidence,
//! - the seeded question text and the ①②③-glyph options.
//!
//! Forward nav (`C` → control center) is paired with return nav (`1` → issue
//! list) per the `tmux-ui-tripwire` skill's HARD RULE 6.
//!
//! Runs for real when tmux + the built binaries + the staged plugin are
//! present; SKIPs gracefully otherwise (see `tripwire_p4_common.rs`).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    ATTENTION_ASK_OPTION_1, ATTENTION_ASK_OPTION_2, ATTENTION_ASK_QUESTION, TuiSession,
    can_run_tripwire, prepare_pipeline_with_attention, skip,
};

#[test]
fn control_center_shuffles_ask_above_wait_with_inline_options() {
    if !can_run_tripwire() {
        skip("control_center_attention");
        return;
    }

    let pipe = prepare_pipeline_with_attention();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, landing) = TuiSession::launch_to_hangar(&bin, pipe.home());

    // Pre-press negative: the control-center title never appears on the issue
    // list landing screen.
    assert!(
        !landing.contains("need you"),
        "control-center chrome bled into the issue-list landing screen:\n{landing}"
    );

    // Forward nav: `C` opens the control center. Re-send until the seeded ASK
    // question text has round-tripped through the socket (the plugin's snapshot
    // fetch races the first render).
    let deadline = Instant::now() + Duration::from_secs(30 * common::budget_scale());
    let board = sess
        .switch_tab_until("C", deadline, |c| {
            c.contains(ATTENTION_ASK_QUESTION) && c.contains("need you")
        })
        .unwrap_or_else(|| panic!("control center never rendered:\n{}", sess.capture()));

    // POSITIVE: fleet-wide counts — 2 seeded rows, 1 of which needs a decision
    // (the WAIT row does not count toward "need you", only ASK/approval/Codex
    // request-user do per `AttentionKind::urgency_rank`).
    assert!(
        board.contains("2 sessions") && board.contains("1 need you"),
        "control-center title counts wrong:\n{board}"
    );

    // POSITIVE: the auto-shuffle — the ASK card (badge `ASK`, selected marker
    // `▶` since it lands first on the very first snapshot) renders ABOVE the
    // WAIT card in the card list, even though the ASK row's `created_at` is
    // OLDER than the WAIT row's. A purely-recency sort would put WAIT first;
    // seeing ASK first proves the urgency-rank shuffle, not a recency artifact.
    let ask_pos = board
        .find("▶ASK")
        .unwrap_or_else(|| panic!("selected ASK card marker `▶ASK` never rendered:\n{board}"));
    let wait_pos = board
        .find(" WAIT")
        .unwrap_or_else(|| panic!("WAIT card never rendered:\n{board}"));
    assert!(
        ask_pos < wait_pos,
        "ASK card did not shuffle above the older-by-urgency WAIT card \
         (ask_pos={ask_pos}, wait_pos={wait_pos}):\n{board}"
    );

    // POSITIVE: the inline ①②③ options on the auto-selected ASK card.
    assert!(
        board.contains(&format!("① {ATTENTION_ASK_OPTION_1}")),
        "circled-digit option ① missing:\n{board}"
    );
    assert!(
        board.contains(&format!("② {ATTENTION_ASK_OPTION_2}")),
        "circled-digit option ② missing:\n{board}"
    );

    // NEGATIVE: not stuck on the non-answerable placeholder copy.
    assert!(
        !board.contains("no inline options"),
        "ASK card rendered as non-answerable:\n{board}"
    );

    // Return nav: hop to Settings (`,`) first, THEN `1` back to the issue list.
    // A bare `1` here would NOT navigate — on the control center the digit keys
    // are intercepted to answer the selected ASK's inline option (①..⑨) rather
    // than switch tabs (`plugin.rs`'s `Screen::ControlCenter` digit guard), so
    // this mirrors `tripwire_p4_issue_list_renders.rs`'s settings-hop return.
    let settings = sess
        .switch_tab_until(
            ",",
            Instant::now() + Duration::from_secs(10 * common::budget_scale()),
            |c| c.contains("Daemon") && c.contains("Providers"),
        )
        .unwrap_or_else(|| panic!("settings never rendered on return nav:\n{}", sess.capture()));
    assert!(
        !settings.contains("need you"),
        "control-center chrome bled into settings:\n{settings}"
    );

    // Detect the issue-list BODY marker (`Refactor API`, the seeded P4 fixture
    // title), not just the persistent tab strip (which renders `[C]Control` on
    // every screen, including the one we're leaving).
    let back = sess
        .switch_tab_until(
            "1",
            Instant::now() + Duration::from_secs(10 * common::budget_scale()),
            |c| c.contains("Refactor API"),
        )
        .unwrap_or_else(|| panic!("issue list never returned:\n{}", sess.capture()));
    assert!(
        !back.contains("need you"),
        "control-center chrome bled into the returned issue list:\n{back}"
    );
}
