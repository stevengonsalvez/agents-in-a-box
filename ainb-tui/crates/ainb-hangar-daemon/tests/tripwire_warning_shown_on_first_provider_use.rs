//! P5.6 — danger-full-access first-run warning tripwire (tmux-driven, e2e).
//!
//! Drives the real `ainb tui` against a fresh, isolated `$HOME` (no recorded
//! `warnings_ack`), proving the user-visible first-run flow end to end:
//!
//! 1. launch `ainb tui` and open the Hangar screen (`g`),
//! 2. assert the `danger-full-access` warning modal appears in `capture-pane`
//!    within the budget (the marker the prompt greps for),
//! 3. send `y` to accept,
//! 4. assert the modal is dismissed (the marker is gone) AND the `first_run` ack
//!    was persisted to `~/.agents-in-a-box/hangar/state.toml::warnings_ack`.
//!
//! ## Placement note
//!
//! The prompt asks for this in `crates/ainb-plugin-hangar/tests/`, but the
//! tmux stand-up harness ([`tripwire_p4_common`]) seeds + spawns the daemon and
//! needs `ainb-hangar-store` / `ainb-hangar-daemon::seed` — deps the plugin
//! crate must NOT carry (the plugin owns zero domain data and never depends on
//! the daemon). So this lives beside the sibling P5.5
//! `tripwire_workspace_switch_e2e.rs` in the daemon crate, which already owns
//! the harness. The *per-provider* warning (warning on the live event stream) is
//! a daemon-side decision unit-tested in `src/warnings.rs`; surfacing it on a
//! live socket push waits on the daemon→plugin event bus (a later bead), so this
//! e2e asserts the FIRST-RUN modal — the user-visible surface that ships now.
//!
//! Honours the `tmux-ui-tripwire` HARD RULES via the shared P4 harness:
//! exact-name `tmux kill-session` only, `poll_capture` (no bare sleep),
//! single-char nav without `Enter`, POSITIVE paired with a NEGATIVE assertion,
//! and SKIP-not-fail when tmux / the binaries / the staged plugin are missing.

#![allow(clippy::duration_suboptimal_units)]

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, ainb_bin, can_run_tripwire, clear_first_run_ack, prepare_pipeline, skip};

/// The on-screen marker the first-run modal paints (its title + body carry it).
const WARNING_MARKER: &str = "danger-full-access";

#[test]
fn warning_shown_on_first_provider_use() {
    if !can_run_tripwire() {
        skip("first_run_warning");
        return;
    }
    // `prepare_pipeline` pre-acks `first_run` so the per-screen tripwires skip
    // the modal; clear it here so THIS launch is a genuine first run and the
    // danger-full-access modal must show.
    let pipe = prepare_pipeline();
    clear_first_run_ack(pipe.home());
    let bin = ainb_bin().expect("gated by can_run_tripwire");

    let sess = TuiSession::spawn(&bin, pipe.home());
    // Reaching the Hangar screen is a hard precondition, and the environment is
    // already gated by `can_run_tripwire()` — so a render timeout here is a
    // real regression, not a missing prerequisite. This used to SKIP (citing
    // the long-resolved P4.9 render blocker), which silently masked the notifyd
    // first-run dialog swallowing the `g` nav on CI. Fail loud instead.
    assert!(
        sess.open_hangar_and_wait_ready().is_some(),
        "hangar screen never rendered (precondition):\n{}",
        sess.capture()
    );

    // POSITIVE: the danger-full-access warning modal appears within 10s.
    let shown = sess.poll_capture(Instant::now() + Duration::from_secs(10), |c| {
        c.contains(WARNING_MARKER)
    });
    let Some(shown) = shown else {
        // The modal renders on the plugin's first paint after init. The screen
        // is already up (precondition above), so a missing modal is a real
        // regression — fail loud, don't skip-mask it.
        panic!(
            "danger-full-access modal never painted:\n{}",
            sess.capture()
        );
    };
    assert!(
        shown.contains(WARNING_MARKER),
        "warning not shown:\n{shown}"
    );
    // NEGATIVE: the accept hint proves it is the modal, not an incidental string.
    assert!(shown.contains("[y]"), "modal accept hint missing:\n{shown}");

    // Accept the warning (single char, no Enter), re-sending until the modal
    // actually goes away.
    //
    // The host forwards a BOUNDED run of keystrokes to a plugin screen between
    // its periodic background ticks, so a single `y` sent while the loop is
    // busy — which is exactly what happens under the full serial suite — is
    // dropped and the modal stays up. Re-sending behind the observable
    // condition (no bare sleep, per the tripwire HARD RULES) keeps this a real
    // regression guard: a modal that never dismisses still fails at the
    // deadline. Accepting twice is a no-op; the modal is gone after the first.
    let dismiss_deadline = Instant::now() + Duration::from_secs(20 * common::budget_scale());
    let dismissed = loop {
        sess.send_key("y");
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), |c| {
            !c.contains(WARNING_MARKER)
        }) {
            break Some(c);
        }
        if Instant::now() >= dismiss_deadline {
            break None;
        }
    };
    assert!(
        dismissed.is_some(),
        "warning modal never dismissed after `y`:\n{}",
        sess.capture()
    );

    // The `first_run` ack was persisted to state.toml (so a relaunch is quiet).
    let state = pipe.home().join(".agents-in-a-box").join("hangar").join("state.toml");
    let ack_written = sess.poll_capture(Instant::now() + Duration::from_secs(5), |_| {
        std::fs::read_to_string(&state).is_ok_and(|raw| raw.contains("first_run"))
    });
    assert!(
        ack_written.is_some(),
        "first_run ack not persisted to {}:\n{}",
        state.display(),
        std::fs::read_to_string(&state).unwrap_or_default()
    );
}
