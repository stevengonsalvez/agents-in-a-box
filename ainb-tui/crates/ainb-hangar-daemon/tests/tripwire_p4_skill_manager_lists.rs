//! P4.9 — skill-manager tripwire: `4` lists skills + file tree.
//!
//! Asserts the seeded skill name, the `SKILL.md` file, and the `Used` filter chip
//! render (POSITIVE), paired with a NEGATIVE check that we are not on the issue
//! list. Forward (`4` → skills) is paired with a return (`1` → issue list).
//!
//! SKIPs until the P5 render pipeline is standable — see `tripwire_p4_common.rs`.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{can_run_tripwire, seed_isolated_home, skip, TuiSession};

#[test]
fn skill_manager_lists_skills() {
    if !can_run_tripwire() {
        skip("skill_manager");
        return;
    }
    let home = seed_isolated_home();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let sess = TuiSession::spawn(&bin, home.path());
    sess.wait_ready().expect("issue list never rendered");

    sess.send_key("4");
    let skills = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("SKILL.md") && c.contains("Used")
        })
        .expect("skill manager never rendered");

    // POSITIVE: seeded skill + file + chip. NEGATIVE: not the issue list.
    assert!(skills.contains("commit"), "seeded skill missing:\n{skills}");
    assert!(skills.contains("SKILL.md"), "SKILL.md file missing:\n{skills}");
    assert!(skills.contains("Used"), "Used chip missing:\n{skills}");
    assert!(!skills.contains("Todo (3)"), "still on the issue list:\n{skills}");

    // Return navigation.
    sess.send_key("1");
    let back = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| c.contains("Refactor API"))
        .expect("issue list never returned from skills");
    assert!(!back.contains("SKILL.md"), "skill tree bled into the issue list:\n{back}");
}
