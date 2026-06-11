//! P9.x e2e tripwire — the Logs screen (`L`) renders the daemon's real
//! structured-log file when the user presses the hotkey in `ainb tui`.
//!
//! ```text
//!  seed P4 fixture + spawn daemon ──▶ daemon writes daemon.<date> JSONL
//!         │                                   ▲ (file read, not an RPC)
//!         ▼ append known marker lines         │
//!  {home}/.ainb/hangar/logs/daemon.<date>  ◀──┘
//!         ▲
//!  ainb tui (tmux) ──`g`──▶ Hangar ──`L`──▶ Logs pane
//!                                            │
//!         poll_capture: marker message + its level + the chip row
//! ```
//!
//! The Logs screen (`screen/logs.rs`) is a **file read**, not a daemon RPC: it
//! resolves `{home}/.ainb/hangar/logs` via
//! [`ainb_hangar_core::logs::default_log_dir`] and tails the newest `daemon.*`
//! file. So this tripwire seeds the daemon's own log artefact directly —
//! [`seed_logs`] appends three known JSON lines (one `INFO` carrying
//! [`LOGS_TRIPWIRE_MARKER`], one `WARN`, one `ERROR`) in the P8.1 wire shape to
//! the newest dated file — then drives `ainb tui` to the Logs pane and asserts
//! the marker message + its `INFO` level token render, plus the level-filter
//! chips. The negative half proves the screen actually switched away from the
//! issue-list landing.
//!
//! SKIP (never fail) when tmux / the built binaries / the staged plugin are
//! absent, per the `tmux-ui-tripwire` skill.

#![allow(clippy::duration_suboptimal_units)] // `from_secs` reads fine as a budget.

#[path = "tripwire_p4_common.rs"]
mod common;

use std::time::{Duration, Instant};

use common::{
    LOGS_TRIPWIRE_MARKER, READY_MARKER, TuiSession, ainb_bin, can_run_tripwire,
    hangar_chrome_visible, prepare_pipeline, seed_logs, skip,
};

/// The four level-filter chip labels the Logs screen paints across its chip row
/// (`screen/logs.rs` `CHIPS`). All four must render — proof the filter UI is
/// present, not just raw log text.
const CHIP_LABELS: [&str; 4] = ["all", "info", "warn", "error"];

#[test]
fn logs_screen_renders_seeded_daemon_log_lines() {
    if !can_run_tripwire() {
        skip("logs screen tripwire");
        return;
    }

    // Seed the P4 fixture + spawn the RPC-only daemon (which writes its own
    // daemon.<date> JSONL on boot), then append our known marker lines to the
    // newest dated file so the Logs screen has deterministic content to tail.
    let pipeline = prepare_pipeline();
    seed_logs(pipeline.home());

    let bin = ainb_bin().expect("gated by can_run_tripwire");
    let (session, landing) = TuiSession::launch_to_hangar(&bin, pipeline.home());

    // Sanity / negative half: the landing is the Hangar issue list, and it does
    // NOT already show the seeded log marker (so a later match proves a switch).
    assert!(
        landing.contains(READY_MARKER),
        "expected the Hangar issue-list landing before pressing L:\n{landing}"
    );
    assert!(
        !landing.contains(LOGS_TRIPWIRE_MARKER),
        "the issue list must not already show the seeded log marker:\n{landing}"
    );

    // Press `L` (single-char nav, no Enter) until the Logs pane shows the seeded
    // marker line AND its INFO level token. 45s budget covers the first render's
    // file read.
    let deadline = Instant::now() + Duration::from_secs(45);
    let pane = session
        .switch_tab_until("L", deadline, |c| {
            hangar_chrome_visible(c) && c.contains(LOGS_TRIPWIRE_MARKER) && c.contains("INFO")
        })
        .unwrap_or_else(|| {
            panic!(
                "Logs screen never rendered the seeded marker line:\n{}",
                session.capture()
            )
        });

    // POSITIVE: the seeded marker message rendered, with its INFO level token.
    assert!(
        pane.contains(LOGS_TRIPWIRE_MARKER),
        "seeded log marker {LOGS_TRIPWIRE_MARKER:?} missing from the Logs pane:\n{pane}"
    );
    assert!(
        pane.contains("INFO"),
        "the seeded INFO level token missing from the Logs pane:\n{pane}"
    );

    // POSITIVE: every level-filter chip label rendered (the chip row is present,
    // not just raw log text).
    for label in CHIP_LABELS {
        assert!(
            pane.contains(label),
            "level-filter chip {label:?} missing from the Logs pane:\n{pane}"
        );
    }

    // NEGATIVE: no longer on the issue-list landing (the `L` switch happened).
    assert!(
        !pane.contains(READY_MARKER),
        "still on the issue list after pressing L (no screen switch):\n{pane}"
    );

    drop(session);
    drop(pipeline);
}
