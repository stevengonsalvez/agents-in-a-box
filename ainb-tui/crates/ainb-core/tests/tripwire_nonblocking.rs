//! Tripwire 7f.1: Non-blocking render thread invariant.
//!
//! Spawns a deliberately slow fixture plugin (200ms per render) and
//! drives the RuntimeHandle's try_recv_render() in a tight loop
//! simulating the TUI render thread. Asserts that each poll of
//! try_recv_render completes in <16ms (60fps budget), proving the
//! render thread never blocks waiting for a slow plugin.

#[allow(unused)]
#[path = "tripwire_helpers.rs"]
mod tripwire_helpers;

use std::time::{Duration, Instant};

use ainb_plugin_protocol::params::Viewport;
use tripwire_helpers::{ensure_running, fast_runtime, register_plugin, sibling_bin};

#[test]
fn render_thread_never_blocks_on_slow_plugin() {
    let slow_bin = sibling_bin("ainb-slow-fixture-plugin");
    let (rt, handle) = fast_runtime();
    let id = register_plugin(&rt, "slow-fixture", slow_bin);

    ensure_running(&rt, &handle, &id);

    // Issue a render request (will take 200ms+ to complete on plugin side).
    let _rx = handle.render(&id, Viewport::new(80, 24), 1);

    // Simulate 20 TUI render frames. Each frame polls try_recv_render.
    // The critical invariant: try_recv_render NEVER blocks — even when
    // the plugin hasn't replied yet, each poll must complete in <16ms.
    let mut max_frame_duration = Duration::ZERO;
    let frame_budget = Duration::from_millis(16);

    for frame in 0..20 {
        let frame_start = Instant::now();

        // This is what the real TUI render loop does — non-blocking poll.
        let _maybe_buffer = handle.try_recv_render(&id);

        let elapsed = frame_start.elapsed();
        if elapsed > max_frame_duration {
            max_frame_duration = elapsed;
        }

        assert!(
            elapsed < frame_budget,
            "frame {frame} took {elapsed:?} — exceeds 16ms budget. \
             try_recv_render is blocking!"
        );

        // Simulate 16ms frame interval.
        std::thread::sleep(Duration::from_millis(8));
    }

    // Also verify we eventually get the render result (plugin is slow, not dead).
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got_result = false;
    while Instant::now() < deadline {
        if handle.try_recv_render(&id).is_some() {
            got_result = true;
            break;
        }
        // Keep issuing render requests so the cache gets populated.
        drop(handle.render(&id, Viewport::new(80, 24), 2));
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        got_result,
        "slow plugin never delivered a render result within 3s"
    );

    drop(handle);
    drop(rt);
}
