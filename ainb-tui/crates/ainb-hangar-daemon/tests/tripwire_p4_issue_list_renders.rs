//! P4.9 — issue-list tripwire: `g` lands on the seeded issue list.
//!
//! Asserts the seeded `Refactor API` title and the `Todo (3)` status-group count
//! render (POSITIVE markers), paired with a NEGATIVE placeholder check that we are
//! not stuck on a loading/empty screen. Forward (`g` → hangar issue list) is
//! paired with a return navigation (`,` → settings → `1` → back to issue list)
//! so a one-way key swallow can't pass silently.
//!
//! Runs for real when tmux + the built binaries + the staged plugin are present;
//! SKIPs gracefully otherwise (see `tripwire_p4_common.rs`).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{can_run_tripwire, prepare_pipeline, skip, TuiSession};

#[test]
fn issue_list_renders_seeded_issues() {
    if !can_run_tripwire() {
        skip("issue_list");
        return;
    }
    let pipe = prepare_pipeline();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, landing) = TuiSession::launch_to_hangar(&bin, pipe.home());

    // POSITIVE: seeded title + status count. NEGATIVE: not an empty/loading state.
    assert!(landing.contains("Refactor API"), "seeded issue missing:\n{landing}");
    assert!(landing.contains("Todo (3)"), "Todo count missing:\n{landing}");
    assert!(!landing.contains("Loading"), "stuck on loading:\n{landing}");
    assert!(!landing.contains("No issues"), "empty state shown:\n{landing}");

    // Return navigation: leave to settings (`,`) then back to issue list (`1`).
    sess.send_key(",");
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
