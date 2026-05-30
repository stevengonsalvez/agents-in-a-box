//! P4.9 — settings tripwire: `,` opens the four-section settings screen.
//!
//! Asserts the `Daemon` section header, the socket-path basename, and the
//! `Providers` section render (POSITIVE), paired with a NEGATIVE check that we
//! are not on the issue list. Forward (`,` → settings) is paired with a return
//! (`1` → issue list).
//!
//! SKIPs until the P5 render pipeline is standable — see `tripwire_p4_common.rs`.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{can_run_tripwire, seed_isolated_home, skip, TuiSession};

#[test]
fn settings_renders_sections() {
    if !can_run_tripwire() {
        skip("settings");
        return;
    }
    let home = seed_isolated_home();
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let sess = TuiSession::spawn(&bin, home.path());
    sess.wait_ready().expect("issue list never rendered");

    sess.send_key(",");
    let settings = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("Daemon") && c.contains("Providers")
        })
        .expect("settings never rendered");

    // POSITIVE: section headers + socket basename. NEGATIVE: not the issue list.
    assert!(settings.contains("Daemon"), "Daemon section missing:\n{settings}");
    assert!(settings.contains("Providers"), "Providers section missing:\n{settings}");
    assert!(settings.contains("hangar.sock"), "socket basename missing:\n{settings}");
    assert!(!settings.contains("Todo (3)"), "still on the issue list:\n{settings}");

    // Return navigation.
    sess.send_key("1");
    let back = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| c.contains("Refactor API"))
        .expect("issue list never returned from settings");
    assert!(!back.contains("Providers"), "settings bled into the issue list:\n{back}");
}
