//! P4.9 — issue-list tripwire: `1` lands on the seeded issue list.
//!
//! Asserts the seeded `Refactor API` title and the `Todo (3)` status-group count
//! render (POSITIVE markers), paired with a NEGATIVE placeholder check that we are
//! not stuck on a loading/empty screen. Forward (`1` → issue list) is paired with
//! a return navigation (`4` → settings → `1` → back to issue list) so a one-way
//! key swallow can't pass silently.
//!
//! SKIPs (does not fail) until the P5 render pipeline is standable — see
//! `tripwire_p4_common.rs`.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{can_run_tripwire, seed_isolated_home, skip, TuiSession};

#[test]
fn issue_list_renders_seeded_issues() {
    if !can_run_tripwire() {
        skip("issue_list");
        return;
    }
    let home = seed_isolated_home();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let sess = TuiSession::spawn(&bin, home.path());

    let landing = sess.wait_ready().expect("issue list never rendered");
    // POSITIVE: seeded title + status count. NEGATIVE: not an empty/loading state.
    assert!(landing.contains("Refactor API"), "seeded issue missing:\n{landing}");
    assert!(landing.contains("Todo (3)"), "Todo count missing:\n{landing}");
    assert!(!landing.contains("Loading"), "stuck on loading:\n{landing}");
    assert!(!landing.contains("No issues"), "empty state shown:\n{landing}");

    // Return navigation: leave to settings (`4`) then back to issue list (`1`).
    sess.send_key("4");
    let settings = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| c.contains("Daemon"))
        .expect("settings never rendered on forward nav");
    assert!(!settings.contains("Refactor API"), "issue list bled into settings:\n{settings}");

    sess.send_key("1");
    let back = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| c.contains("Refactor API"))
        .expect("issue list never returned");
    assert!(back.contains("Todo (3)"), "return nav lost the issue list:\n{back}");
}
