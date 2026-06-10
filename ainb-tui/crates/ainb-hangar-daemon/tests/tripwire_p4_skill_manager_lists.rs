//! P4.9 — skill-manager tripwire: `4` lists the workspace skills + filter chips.
//!
//! Asserts the seeded `commit` skill name and the `Used` filter chip render
//! (POSITIVE), paired with a NEGATIVE check that we are not on the issue list.
//! Forward (`4` → skills) is paired with a return (`1` → issue list).
//!
//! Runs for real when tmux + binaries + staged plugin are present; SKIPs
//! gracefully otherwise (see `tripwire_p4_common.rs`).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, can_run_tripwire, prepare_pipeline, skip};

#[test]
fn skill_manager_lists_skills() {
    if !can_run_tripwire() {
        skip("skill_manager");
        return;
    }
    let pipe = prepare_pipeline();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());

    sess.send_key("4");
    let skills = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("commit") && c.contains("Used")
        })
        .expect("skill manager never rendered");

    // POSITIVE: seeded skill name + filter chip. NEGATIVE: not the issue list.
    assert!(skills.contains("commit"), "seeded skill missing:\n{skills}");
    assert!(skills.contains("Used"), "Used chip missing:\n{skills}");
    assert!(
        !skills.contains("Todo (3)"),
        "still on the issue list:\n{skills}"
    );

    // Return navigation.
    sess.send_key("1");
    let back = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| {
            c.contains("Refactor API")
        })
        .expect("issue list never returned from skills");
    assert!(
        !back.contains("Used"),
        "skill chips bled into the issue list:\n{back}"
    );
}
