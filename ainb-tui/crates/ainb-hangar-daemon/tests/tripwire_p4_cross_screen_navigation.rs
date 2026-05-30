//! P4.9 — cross-screen navigation tripwire: `1` → `4` → `,` walks the tabs.
//!
//! The flakiest of the six (per the P4.9 risk register), so each hop polls the
//! pane up to 10× with a 200ms gap (the `poll_capture` deadline handles that).
//! Each step pairs a POSITIVE marker for the destination screen with a NEGATIVE
//! check that the prior screen's distinctive chrome is gone — never a
//! substring-OR on shared chrome.
//!
//! SKIPs until the P5 render pipeline is standable — see `tripwire_p4_common.rs`.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{can_run_tripwire, seed_isolated_home, skip, TuiSession};

/// Poll for a screen identified by a positive marker that must be absent of the
/// `forbidden` prior-screen marker.
fn expect_screen(sess: &TuiSession, positive: &str, forbidden: &str) -> String {
    sess.poll_capture(Instant::now() + Duration::from_secs(12), |c| {
        c.contains(positive) && !c.contains(forbidden)
    })
    .unwrap_or_else(|| panic!("screen with {positive:?} never rendered (forbidding {forbidden:?})"))
}

#[test]
fn cross_screen_navigation_walks_tabs() {
    if !can_run_tripwire() {
        skip("cross_screen_navigation");
        return;
    }
    let home = seed_isolated_home();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let sess = TuiSession::spawn(&bin, home.path());
    sess.wait_ready().expect("issue list never rendered");

    // 1 → issue list (positive: seeded issue; forbidden: skills file).
    sess.send_key("1");
    let issues = expect_screen(&sess, "Refactor API", "SKILL.md");
    assert!(issues.contains("Todo (3)"), "issue counts missing:\n{issues}");

    // 4 → skills (positive: SKILL.md; forbidden: issue count).
    sess.send_key("4");
    let skills = expect_screen(&sess, "SKILL.md", "Todo (3)");
    assert!(skills.contains("Used"), "skills chip missing:\n{skills}");

    // , → settings (positive: Daemon; forbidden: skills file).
    sess.send_key(",");
    let settings = expect_screen(&sess, "Daemon", "SKILL.md");
    assert!(settings.contains("Providers"), "settings sections missing:\n{settings}");

    // Return all the way back to issue list (forbidding settings chrome).
    sess.send_key("1");
    let back = expect_screen(&sess, "Refactor API", "Providers");
    assert!(back.contains("Todo (3)"), "return nav lost the issue list:\n{back}");
}
