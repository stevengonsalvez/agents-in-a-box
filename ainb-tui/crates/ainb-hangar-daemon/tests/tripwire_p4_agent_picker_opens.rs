//! P4.9 — agent-picker tripwire: `a` on a selected row opens the modal.
//!
//! Asserts the polymorphic actor list shows `claude-agent`, its `online`
//! presence, and the violet agent glyph `⬡` (POSITIVE), paired with a NEGATIVE
//! check that the modal is genuinely overlaid (the picker title is present).
//! Forward (`a` → picker) is paired with a return (`Esc` → issue list).
//!
//! SKIPs until the P5 render pipeline is standable — see `tripwire_p4_common.rs`.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{can_run_tripwire, seed_isolated_home, skip, TuiSession};

#[test]
fn agent_picker_opens_with_actors() {
    if !can_run_tripwire() {
        skip("agent_picker");
        return;
    }
    let home = seed_isolated_home();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let sess = TuiSession::spawn(&bin, home.path());
    sess.wait_ready().expect("issue list never rendered");

    // Open the agent picker for the selected row (single-char nav, no Enter).
    sess.send_key("a");
    let picker = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("Pick assignee") && c.contains("claude-agent")
        })
        .expect("agent picker never opened");

    // POSITIVE: actor + presence + agent glyph. NEGATIVE: still has the modal title.
    assert!(picker.contains("claude-agent"), "agent actor missing:\n{picker}");
    assert!(picker.contains("online"), "presence label missing:\n{picker}");
    assert!(picker.contains('⬡'), "violet agent glyph missing:\n{picker}");
    assert!(picker.contains("Pick assignee"), "modal title missing:\n{picker}");

    // Return navigation: Esc closes the modal back to the issue list.
    sess.send_key("Escape");
    let back = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| {
            c.contains("Refactor API") && !c.contains("Pick assignee")
        })
        .expect("modal never closed back to the issue list");
    assert!(!back.contains("Pick assignee"), "picker modal lingered:\n{back}");
}
