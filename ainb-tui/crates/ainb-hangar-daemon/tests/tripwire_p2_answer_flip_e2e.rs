//! P11 (agents-in-a-box-43e) — the real answer-FLIP e2e tripwire.
//!
//! `tripwire_p2_control_center_attention` renders + navigates the control center
//! and shows the ①②③ options, but NEVER presses a digit to answer — so the flip
//! through the real daemon (answer RPC → C1 live-target resolve → verified tmux
//! delivery → `open`→`answered` → board refresh) was untested end to end. The
//! plugin's `attention_answer_over_socket` proves the `2` keystroke issues an
//! `attention/answer` RPC, but only against a MOCK daemon that fakes the
//! `delivered` outcome. Neither exercises the daemon's real answer router with a
//! live delivery target.
//!
//! That gap was first (and only) driven by the vhs recording harness
//! (`docs/hangar/assets/record-control-center.sh` + `seed_control_center.rs`):
//! a fake `ainb list` binds the seeded ASK's raising session (`s-deploy`) to a
//! real tmux pane and `AINB_FLEET_TRANSPORT=tmux-only` forces the last mile
//! through tmux, so pressing an option digit genuinely delivers the pick and the
//! row flips to `answered`, dropping off the open board. This tripwire turns that
//! recording technique into an assertion:
//!
//!   1. open the control center (`C`) — the seeded 3-option ASK is on top,
//!      `1 need you`, ①②③ options visible;
//!   2. press `2` (answer `prod`) — re-pressed ONLY while the unanswered ASK is
//!      still the selected/answerable card (a stray digit AFTER the flip would
//!      fall through to the tab router and navigate off the control center, per
//!      the `plugin.rs` `Screen::ControlCenter` digit guard);
//!   3. the board flips to `0 need you` (the `AttentionAnswered` delta round-trips
//!      back to the TUI — the daemon only keeps the row answered on a CONFIRMED
//!      tmux delivery, so `0 need you` proves resolve + flip + delivery);
//!   4. the picked option lands in the target pane (the last mile the board flip
//!      alone can't show);
//!   5. the DB row deterministically reads `answered` carrying `prod`.
//!
//! Runs for real when tmux + the built binaries + the staged plugin are present;
//! SKIPs gracefully otherwise (see `tripwire_p4_common.rs`).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    ATTENTION_ASK_ID, ATTENTION_ASK_OPTION_1, ATTENTION_ASK_OPTION_2, ATTENTION_ASK_OPTION_3,
    ATTENTION_ASK_QUESTION, DeliveryTarget, TuiSession, attention_row_state, budget_scale,
    can_run_tripwire, prepare_pipeline_answer_flip, skip,
};

#[test]
fn answering_the_control_center_ask_delivers_and_flips_the_board() {
    if !can_run_tripwire() {
        skip("answer_flip_e2e");
        return;
    }

    // The live delivery target — a plain shell the daemon send-keys the pick into.
    // Stood up BEFORE the daemon so the fake `ainb list` has a real session to bind.
    let target = DeliveryTarget::spawn();
    let pipe = prepare_pipeline_answer_flip(target.name());
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());

    let scale = budget_scale();

    // Open the control center; wait for the seeded ASK + the `1 need you` count.
    let open_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let board = sess
        .switch_tab_until("C", open_deadline, |c| {
            c.contains(ATTENTION_ASK_QUESTION) && c.contains("1 need you")
        })
        .unwrap_or_else(|| {
            panic!(
                "control center never rendered the seeded ASK:\n{}",
                sess.capture()
            )
        });

    // POSITIVE: the three inline options render with their ①②③ glyphs (proves
    // the full option span the answer picks from).
    assert!(
        board.contains(&format!("① {ATTENTION_ASK_OPTION_1}")),
        "circled-digit option ① missing:\n{board}"
    );
    assert!(
        board.contains(&format!("② {ATTENTION_ASK_OPTION_2}")),
        "circled-digit option ② missing:\n{board}"
    );
    assert!(
        board.contains(&format!("③ {ATTENTION_ASK_OPTION_3}")),
        "circled-digit option ③ missing:\n{board}"
    );

    // Answer option 2 (`prod`). Re-press `2` ONLY while the control center still
    // shows the unanswered ASK (a dropped first keystroke) — never after the flip,
    // where the selected card becomes the non-answerable WAIT row and a digit
    // falls through to the tab router (navigating off the control center). If a
    // stray press ever drifts us away before the flip registers, hop back with `C`.
    let flip_deadline = Instant::now() + Duration::from_secs(30 * scale);
    let mut pressed = false;
    let flipped = loop {
        let cur = sess.capture();
        // Success: the answered flip round-tripped back to the TUI.
        if cur.contains("0 need you") {
            break cur;
        }
        if Instant::now() >= flip_deadline {
            panic!(
                "ASK never flipped to `0 need you` on the board:\n{cur}\n\
                 --- target pane ---\n{}",
                target.capture()
            );
        }
        if cur.contains(ATTENTION_ASK_QUESTION) && cur.contains("need you") {
            // Still on the control center with the unanswered ASK selected → press.
            sess.send_key("2");
            pressed = true;
        } else if pressed && !cur.contains("need you") {
            // Drifted off the control center after a stray digit — return to it.
            sess.send_key("C");
        }
        std::thread::sleep(Duration::from_millis(300));
    };

    // NEGATIVE: the answered ASK is no longer counted as needing input.
    assert!(
        !flipped.contains("1 need you"),
        "board still reports the ASK as needing input after the answer:\n{flipped}"
    );

    // POSITIVE (last mile): the picked option landed in the target session's pane
    // via the verified send path — the delivery the board flip cannot itself show.
    let delivered = poll_until(Instant::now() + Duration::from_secs(10 * scale), || {
        target.capture().contains(ATTENTION_ASK_OPTION_2)
    });
    assert!(
        delivered,
        "picked option `{ATTENTION_ASK_OPTION_2}` never delivered into the target pane:\n{}",
        target.capture()
    );

    // POSITIVE (store truth): the row deterministically flipped to `answered`
    // carrying the delivered option (the daemon reopens on a failed delivery, so
    // an `answered` row is itself proof the last mile confirmed).
    let (state, answer) = poll_until_some(Instant::now() + Duration::from_secs(10 * scale), || {
        match attention_row_state(pipe.home(), ATTENTION_ASK_ID) {
            Some((s, a)) if s == "answered" => Some((s, a)),
            _ => None,
        }
    })
    .unwrap_or_else(|| {
        panic!(
            "attention row `{ATTENTION_ASK_ID}` never reached `answered` in the db (state={:?})",
            attention_row_state(pipe.home(), ATTENTION_ASK_ID)
        )
    });
    assert_eq!(state, "answered", "the ASK row must be answered");
    assert_eq!(
        answer.as_deref(),
        Some(ATTENTION_ASK_OPTION_2),
        "the stored answer must be the picked option `{ATTENTION_ASK_OPTION_2}`"
    );
}

/// Poll `pred` every ~200ms until it holds or `deadline` passes; returns whether
/// it held. No bare sleep before an observation.
fn poll_until(deadline: Instant, pred: impl Fn() -> bool) -> bool {
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Poll `f` every ~200ms until it returns `Some` or `deadline` passes.
fn poll_until_some<T>(deadline: Instant, f: impl Fn() -> Option<T>) -> Option<T> {
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
