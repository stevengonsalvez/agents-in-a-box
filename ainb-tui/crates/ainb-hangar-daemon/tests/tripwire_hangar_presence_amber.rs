//! ACCEPTANCE tripwire (multica gap #6, availability): an agent whose runtime
//! stops beating renders **amber `◐ unstable`**, then **`○ offline`**, on the
//! real Agents screen — proven by poking `agent_runtime.last_seen_at` in sqlite.
//!
//! This is the whole parity item end to end through the real stack: real `ainb`
//! binary in tmux, real daemon, real staged plugin, real db. It covers BOTH
//! halves — the render (the snapshot fold turns heartbeat age into the dot) and
//! the writer (the daemon's presence sweeper moves the persisted `status`), so a
//! read-only implementation would fail the final assertion.
//!
//! The decaying agent is a GHOST runtime the daemon does not own
//! (`prepare_pipeline_ghost_runtime`): the daemon beats for its own runtime
//! every tick, so that one could never decay.
//!
//! Follows the `tmux-ui-tripwire` rules: SKIP without tmux/binaries/plugin,
//! `poll_capture` with a deadline (never a bare sleep), exact-name
//! `kill-session` only (via `TuiSession`'s drop), and per-ROW assertions rather
//! than whole-pane substring-ORs.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{
    GHOST_AGENT_NAME, GHOST_RUNTIME_ID, TuiSession, backdate_runtime_heartbeat, can_run_tripwire,
    prepare_pipeline_ghost_runtime, press_until, runtime_status, skip,
};

/// The ghost agent's own line in the pane, or `None` while it is not rendered.
/// Every assertion is made against THIS line, never the whole pane — a
/// pane-wide `contains("unstable")` would pass on any other row's chrome.
fn ghost_line(capture: &str) -> Option<&str> {
    capture.lines().find(|l| l.contains(GHOST_AGENT_NAME))
}

#[test]
fn agent_presence_decays_to_amber_then_offline_on_the_agents_screen() {
    if !can_run_tripwire() {
        skip("hangar_presence_amber");
        return;
    }
    // A 1s sweep cadence: several ticks land inside each poll budget below.
    let pipe = prepare_pipeline_ghost_runtime("1000");
    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    let (sess, _landing) = TuiSession::launch_to_hangar(&bin, pipe.home());

    // Agents tab (`A` in the plugin chrome tab strip).
    let baseline = press_until(&sess, "A", Instant::now() + Duration::from_secs(20), |c| {
        ghost_line(c).is_some_and(|l| l.contains("online"))
    })
    .unwrap_or_else(|| {
        panic!(
            "Agents screen never showed the ghost agent:\n{}",
            sess.capture()
        )
    });

    // POSITIVE baseline + the NEGATIVE that makes the later flip meaningful.
    let line = ghost_line(&baseline).expect("polled on it");
    assert!(
        line.contains('●'),
        "baseline should render the green online dot:\n{line}"
    );
    assert!(
        !line.contains("unstable"),
        "baseline must not already be amber, else the flip proves nothing:\n{line}"
    );

    // Poke sqlite: the ghost runtime last beat 7 minutes ago — inside the amber
    // band (>5min stale, <=10min).
    backdate_runtime_heartbeat(pipe.home(), GHOST_RUNTIME_ID, 7 * 60_000);

    let amber = sess
        .poll_capture(Instant::now() + Duration::from_secs(20), |c| {
            ghost_line(c).is_some_and(|l| l.contains("unstable"))
        })
        .unwrap_or_else(|| panic!("ghost agent never went amber:\n{}", sess.capture()));
    let line = ghost_line(&amber).expect("polled on it");
    assert!(
        line.contains('◐'),
        "amber band must render the half dot:\n{line}"
    );
    assert!(
        !line.contains("online"),
        "the online label must be gone once unstable:\n{line}"
    );

    // Poke again: 20 minutes stale is past the grace window.
    backdate_runtime_heartbeat(pipe.home(), GHOST_RUNTIME_ID, 20 * 60_000);

    let gone = sess
        .poll_capture(Instant::now() + Duration::from_secs(20), |c| {
            ghost_line(c).is_some_and(|l| l.contains("offline"))
        })
        .unwrap_or_else(|| panic!("ghost agent never went offline:\n{}", sess.capture()));
    let line = ghost_line(&gone).expect("polled on it");
    assert!(
        line.contains('○'),
        "offline band must render the hollow dot:\n{line}"
    );
    assert!(
        !line.contains("unstable"),
        "the amber label must be gone once offline:\n{line}"
    );

    // The WRITER half: the sweeper moved the persisted row, not just the render.
    // Poll — the flip lands on the next presence tick, not synchronously.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut stored = runtime_status(pipe.home(), GHOST_RUNTIME_ID);
    while stored.as_deref() != Some("offline") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        stored = runtime_status(pipe.home(), GHOST_RUNTIME_ID);
    }
    assert_eq!(
        stored.as_deref(),
        Some("offline"),
        "the presence sweeper must WRITE agent_runtime.status, not only derive it",
    );
}
