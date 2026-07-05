//! ccc (agents-in-a-box-8hx) — host global single-key shortcuts must NOT swallow
//! keystrokes typed into a hangar plugin overlay.
//!
//! The bug: the host intercepts its global single-character shortcuts (`H`/`?`
//! help toggle, `W` statusline-wire) BEFORE forwarding a key to the focused
//! plugin. So typing a card title containing `H` / `?` / `W` into the Boards
//! card-title input lost those characters — `H`/`?` toggled the host help
//! overlay and the letters never reached the input. The fix: the plugin reports
//! `RenderResult.captures_text` while an input surface is focused, the host
//! stashes it per screen, and `is_text_input_context` + the plugin key-forwarder
//! read it to suppress the globals and forward the keys verbatim.
//!
//! ```text
//!  open Boards (B) ─▶ c: open card-title input ─▶ type "QHW?zz"
//!         │                                            │
//!         ▼                                            ▼
//!  plugin reports captures_text=true       host forwards H/?/W into the input
//!         │                                            │
//!         ▼                                            ▼
//!  title reads "QHW?zz" verbatim ── NO help overlay, still on the Boards screen
//! ```
//!
//! This is the user-visible proof of the host-side fix: the adversarial title
//! packs all three host shortcut chars (`H`, `W`, `?`); if any were swallowed
//! the title echo would be missing that char (POSITIVE assertion), the help
//! overlay would appear (NEGATIVE assertion "Press ? or Esc to close"), or a
//! bare letter would have navigated off the board (NEGATIVE "Board: Delivery").
//! SKIPs cleanly when tmux / the binaries / the staged plugin are absent.
//! Follows the `tmux-ui-tripwire` HARD RULES: exact-name kills only,
//! deadline-bounded polls, POSITIVE + NEGATIVE assertions.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, budget_scale, can_run_tripwire, prepare_pipeline_board_run, skip};

/// The adversarial card title: every host global single-char shortcut (`H`, `W`,
/// `?`) plus distinctive `Q`/`zz` framing so it can never alias a column header
/// (`Todo`/`Done`) or chrome. If the host swallowed any shortcut this exact
/// string could not appear in the input echo.
const ADVERSARIAL_TITLE: &str = "QHW?zz";

#[test]
fn typing_host_shortcut_chars_into_the_card_title_lands_verbatim() {
    if !can_run_tripwire() {
        skip("ccc_board_card_title_host_shortcuts");
        return;
    }

    let pipe = prepare_pipeline_board_run();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());
    let scale = budget_scale();

    // Open the Boards screen; wait for the seeded "Delivery" board.
    let boards_deadline = Instant::now() + Duration::from_secs(30 * scale);
    sess.switch_tab_until("B", boards_deadline, |c| {
        c.contains("Board: Delivery") && c.contains("Todo") && c.contains("Done")
    })
    .unwrap_or_else(|| panic!("Boards screen never rendered:\n{}", sess.capture()));

    // `c` opens the card-title input on the focused Todo column (re-sent until it
    // opens — the first frames after a tab switch can drop a lone keystroke).
    let title_deadline = Instant::now() + Duration::from_secs(20 * scale);
    let opened = press_until(&sess, "c", title_deadline, |c| c.contains("New card title"))
        .unwrap_or_else(|| {
            panic!(
                "card-title input never opened after `c`:\n{}",
                sess.capture()
            )
        });
    assert!(
        opened.contains("New card title"),
        "the `c` key must open the card-title input:\n{opened}"
    );

    // Type the adversarial title as one literal run so tmux never coalesces /
    // drops a char. Every char is a standalone key event; `H`/`W`/`?` are exactly
    // the host global shortcuts that used to be swallowed here.
    sess.type_literal(ADVERSARIAL_TITLE);

    // POSITIVE: the whole title — including `H`, `W`, `?` — echoes verbatim in
    // the input. If the host had eaten any shortcut char the substring match
    // would fail.
    let post = sess
        .poll_capture(Instant::now() + Duration::from_secs(15 * scale), |c| {
            c.contains(ADVERSARIAL_TITLE)
        })
        .unwrap_or_else(|| {
            panic!(
                "the card title '{ADVERSARIAL_TITLE}' (with host shortcut chars H/W/?) never \
                 echoed verbatim into the input — a global shortcut swallowed a keystroke:\n{}",
                sess.capture()
            )
        });

    // Kill the TUI session by exact name before the assertions.
    drop(sess);

    assert!(
        post.contains(ADVERSARIAL_TITLE),
        "the card title must contain every typed char verbatim, got:\n{post}"
    );
    // NEGATIVE: `H`/`?` did NOT toggle the host help overlay (its title is
    // "Help - Press ? or Esc to close").
    assert!(
        !post.contains("Press ? or Esc to close"),
        "typing `H`/`?` into the card title must NOT open the host help overlay:\n{post}"
    );
    // NEGATIVE: a bare letter did NOT navigate off the board (the host's generic
    // shortcut fallthrough would have jumped to Stats/Abtop/etc.).
    assert!(
        post.contains("Board: Delivery"),
        "the view must stay on the Boards screen while typing the title:\n{post}"
    );
}

/// Re-send single-char `key` every ~1.5s until `pred` holds on the pane or
/// `deadline` passes (the first frames after a tab switch can drop a lone
/// keystroke). Returns the matching capture, or `None` on timeout.
fn press_until(
    sess: &TuiSession,
    key: &str,
    deadline: Instant,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    loop {
        sess.send_key(key);
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), &pred) {
            return Some(c);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}
