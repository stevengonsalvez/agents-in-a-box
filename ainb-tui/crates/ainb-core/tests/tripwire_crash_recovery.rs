//! Tripwire 7f.2: Crash recovery + quarantine semantics.
//!
//! Spawns a fixture plugin, SIGKILLs it via inject_kill, and asserts:
//! 1. Host lifecycle transitions through Backoff → respawn → Running.
//! 2. After 3 kills within the failure_window (60s), lifecycle reaches
//!    Quarantined.
//! 3. reload() clears quarantine back to Idle.
//!
//! Zero wasmi imports. All observation via RuntimeHandle non-blocking surface.

#[allow(unused)]
#[path = "tripwire_helpers.rs"]
mod tripwire_helpers;

use std::time::Duration;

use ainb_plugin_protocol::params::Viewport;
use ainb_plugin_runtime::types::LifecycleState;
use tripwire_helpers::{ensure_running, fast_runtime, register_plugin, sibling_bin, wait_for_state};

#[test]
fn single_crash_triggers_respawn() {
    let fixture_bin = sibling_bin("ainb-fixture-plugin");
    let (rt, handle) = fast_runtime();
    let id = register_plugin(&rt, "fixture", fixture_bin);

    ensure_running(&rt, &handle, &id);

    // Kill the plugin process.
    handle.inject_kill(&id).expect("inject_kill");

    // It should transition through Backoff and back to Running after respawn.
    // Give it time to detect the crash and respawn.
    std::thread::sleep(Duration::from_millis(200));

    // Trigger a render to force the runtime to attempt respawn.
    drop(handle.render(&id, Viewport::new(10, 1), 1));

    assert!(
        wait_for_state(&handle, &id, LifecycleState::Running, Duration::from_secs(5)),
        "plugin never recovered to Running after single crash; state = {:?}",
        handle.lifecycle_state(&id)
    );
}

#[test]
fn three_crashes_within_window_triggers_quarantine() {
    let fixture_bin = sibling_bin("ainb-fixture-plugin");
    let (rt, handle) = fast_runtime();
    let id = register_plugin(&rt, "fixture", fixture_bin);

    ensure_running(&rt, &handle, &id);

    // Inject 3 SIGKILLs in rapid succession (within the 60s failure window).
    for i in 0u64..3 {
        handle.inject_kill(&id).expect("inject_kill");
        // Allow time for the runtime to notice the crash.
        std::thread::sleep(Duration::from_millis(300));
        // Force respawn attempt by issuing a render.
        drop(handle.render(&id, Viewport::new(10, 1), i));
        std::thread::sleep(Duration::from_millis(300));
    }

    // Wait for quarantine.
    assert!(
        wait_for_state(
            &handle,
            &id,
            LifecycleState::Quarantined,
            Duration::from_secs(8)
        ),
        "plugin never quarantined after 3 crashes; state = {:?}",
        handle.lifecycle_state(&id)
    );
}

#[test]
fn reload_clears_quarantine() {
    let fixture_bin = sibling_bin("ainb-fixture-plugin");
    let (rt, handle) = fast_runtime();
    let id = register_plugin(&rt, "fixture", fixture_bin);

    ensure_running(&rt, &handle, &id);

    // Drive to quarantine.
    for i in 0u64..3 {
        handle.inject_kill(&id).expect("inject_kill");
        std::thread::sleep(Duration::from_millis(300));
        drop(handle.render(&id, Viewport::new(10, 1), i));
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(
        wait_for_state(
            &handle,
            &id,
            LifecycleState::Quarantined,
            Duration::from_secs(8)
        ),
        "precondition: not quarantined"
    );

    // Reload should clear quarantine.
    handle.reload(&id).expect("reload");
    assert!(
        wait_for_state(&handle, &id, LifecycleState::Idle, Duration::from_secs(2)),
        "reload didn't clear quarantine; state = {:?}",
        handle.lifecycle_state(&id)
    );
}
