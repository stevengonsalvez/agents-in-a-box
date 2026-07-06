//! P4.9 — cross-screen navigation tripwire: `1` → `4` → `,` → `1` walks the tabs.
//!
//! The flakiest of the six (per the P4.9 risk register), so each hop polls the
//! pane with a deadline. Each step pairs a POSITIVE marker for the destination
//! screen with a NEGATIVE check that the prior screen's distinctive content is
//! gone — never a substring-OR on shared chrome.
//!
//! Runs for real when tmux + binaries + staged plugin are present; SKIPs
//! gracefully otherwise (see `tripwire_p4_common.rs`).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, can_run_tripwire, prepare_pipeline, skip};

/// Press `key` (single-char nav, no Enter) until the destination screen renders:
/// its `positive` marker present AND the prior screen's `forbidden` marker gone.
///
/// A single lone keypress can be dropped on a loaded CI runner (the first frames
/// after a switch race the snapshot fetch), so — like the issue-list and
/// autopilots tripwires' [`switch_tab_until`] — the key is RE-SENT every ~1.5s
/// until the marker appears or the deadline passes. The nav keys here (`1`/`3`/
/// `,`) are idempotent tab switches, so re-pressing on the destination is a
/// harmless no-op. This replaces a brittle send-once + 12s-poll that flaked on
/// the loaded `hangar-e2e (ubuntu-latest)` leg once the board redesign made the
/// per-screen render heavier.
fn walk_to_screen(sess: &TuiSession, key: &str, positive: &str, forbidden: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30 * common::budget_scale());
    loop {
        sess.send_key(key);
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), |c| {
            c.contains(positive) && !c.contains(forbidden)
        }) {
            return c;
        }
        if Instant::now() >= deadline {
            panic!(
                "screen with {positive:?} never rendered after re-pressing {key:?} \
                 (forbidding {forbidden:?}):\n{}",
                sess.capture()
            );
        }
    }
}

#[test]
fn cross_screen_navigation_walks_tabs() {
    if !can_run_tripwire() {
        skip("cross_screen_navigation");
        return;
    }
    let pipe = prepare_pipeline();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());

    // 1 → issue list (positive: seeded issue; forbidden: skills chip `Unused`).
    let issues = walk_to_screen(&sess, "1", "Refactor API", "Unused");
    assert!(
        issues.contains("Todo (3)"),
        "issue counts missing:\n{issues}"
    );

    // 3 → skills (positive: seeded skill; forbidden: issue count).
    let skills = walk_to_screen(&sess, "3", "commit", "Todo (3)");
    assert!(skills.contains("Used"), "skills chip missing:\n{skills}");

    // , → settings (positive: Daemon; forbidden: skills chip `Unused`).
    let settings = walk_to_screen(&sess, ",", "Daemon", "Unused");
    assert!(
        settings.contains("Providers"),
        "settings sections missing:\n{settings}"
    );

    // Return all the way back to issue list (forbidding settings chrome).
    let back = walk_to_screen(&sess, "1", "Refactor API", "Providers");
    assert!(
        back.contains("Todo (3)"),
        "return nav lost the issue list:\n{back}"
    );
}
