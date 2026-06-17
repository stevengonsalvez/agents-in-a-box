//! P4.9 — agent-picker tripwire: `a` on a selected row opens the modal.
//!
//! Asserts the polymorphic actor list shows `claude-agent`, its `online`
//! presence, and the violet agent glyph `⬡` (POSITIVE), paired with the modal
//! title `Pick assignee` to prove the modal is genuinely overlaid. Forward (`a` →
//! picker) is paired with a return (Esc leaves the modal; the title is gone).
//!
//! Esc is host-reserved (it pops the plugin screen back to the host home screen
//! rather than reaching the plugin), so the return leg asserts the picker title
//! is *gone* after Esc — a one-way swallow of `a` could not satisfy both legs.
//!
//! Runs for real when tmux + binaries + staged plugin are present; SKIPs
//! gracefully otherwise (see `tripwire_p4_common.rs`).

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, can_run_tripwire, prepare_pipeline, skip};

#[test]
fn agent_picker_opens_with_actors() {
    if !can_run_tripwire() {
        skip("agent_picker");
        return;
    }
    let pipe = prepare_pipeline();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());

    // Open the agent picker for the selected row (single-char nav, no Enter).
    sess.send_key("a");
    let picker = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("Pick assignee") && c.contains("claude-agent")
        })
        .expect("agent picker never opened");

    // POSITIVE: actor + presence + agent glyph + modal title.
    assert!(
        picker.contains("claude-agent"),
        "agent actor missing:\n{picker}"
    );
    assert!(
        picker.contains("online"),
        "presence label missing:\n{picker}"
    );
    assert!(
        picker.contains('⬡'),
        "violet agent glyph missing:\n{picker}"
    );
    assert!(
        picker.contains("Pick assignee"),
        "modal title missing:\n{picker}"
    );

    // Return leg: Esc leaves the modal (host pops the screen). The picker title
    // must be gone — proving the `a` open round-tripped rather than wedging.
    sess.send_key("Escape");
    let post_esc = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| {
            !c.contains("Pick assignee")
        })
        .expect("modal never closed");
    assert!(
        !post_esc.contains("Pick assignee"),
        "picker modal lingered:\n{post_esc}"
    );
}
