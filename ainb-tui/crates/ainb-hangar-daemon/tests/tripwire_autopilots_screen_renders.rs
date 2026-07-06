//! P9.x e2e tripwire — the Autopilots manager screen (`4`) renders a real
//! seeded autopilot row when the user presses the hotkey in `ainb tui`.
//!
//! ```text
//!  seed P4 fixture + 1 autopilot (daily-triage, "0 9 * * *", enabled)
//!         │
//!         ▼
//!  ainb-hangar-daemon (RPC only) ◀── hangar/autopilots_list snapshot
//!         ▲                                   │
//!  ainb tui (tmux) ──`g`──▶ Hangar ──`4`──▶ Autopilots manager
//!                                            │
//!         poll_capture: name `daily-triage` + cron `0 9 * * *` + table header
//! ```
//!
//! Reuses the proven P4 pipeline ([`tripwire_p4_common`]): an isolated `$HOME`
//! whose `hangar.db` is seeded with the P4 fixture, an RPC-only daemon spawned
//! against it (`HANGAR_DAEMON_DISABLE_CLAIM=1`), and `ainb tui` launched under
//! the same `$HOME`. On top of the fixture this seeds one cron-scheduled
//! autopilot ([`seed_autopilot`]) via the P7.2 `AutopilotRepo::create` path, so
//! the `hangar/autopilots_list` RPC the screen pulls returns it. `create`
//! computes a strictly-future `next_tick_at` for `0 9 * * *`, so the daemon's
//! real-clock scheduler parks until that future instant and never fires the
//! autopilot inside the test window.
//!
//! The autopilots screen (`screen/autopilots.rs`) renders each row as
//! `NAME CRON NEXT TICK LAST RUN STATUS`, the cron expression verbatim. So the
//! positive markers are the row's name (`daily-triage`) and its cron
//! (`0 9 * * *`), plus the `NAME` / `CRON` table header; the negative half is
//! the absence of the issue-list landing marker (proving the `5` switch).
//!
//! SKIP (never fail) when tmux / the built binaries / the staged plugin are
//! absent, per the `tmux-ui-tripwire` skill.

#![allow(clippy::duration_suboptimal_units)] // `from_secs` reads fine as a budget.

#[path = "tripwire_p4_common.rs"]
mod common;

use std::time::{Duration, Instant};

use common::{
    READY_MARKER, TuiSession, ainb_bin, can_run_tripwire, hangar_chrome_visible,
    prepare_pipeline_with_autopilot, skip,
};

/// The seeded autopilot's name + cron, exactly as the screen renders them.
const AUTOPILOT_NAME: &str = "daily-triage";
const AUTOPILOT_CRON: &str = "0 9 * * *";

#[test]
fn autopilots_screen_renders_seeded_autopilot() {
    if !can_run_tripwire() {
        skip("autopilots screen tripwire");
        return;
    }

    // Seed the P4 fixture + one autopilot into the database BEFORE the RPC-only
    // daemon spawns, so the manager has a row to render. Seeding the autopilot
    // pre-spawn (not via a second live connection after the daemon is up) keeps
    // the daemon's first issue snapshot off a concurrency race that wedges it on
    // slow CI runners.
    let pipeline = prepare_pipeline_with_autopilot();

    let bin = ainb_bin().expect("gated by can_run_tripwire");
    let (session, landing) = TuiSession::launch_to_hangar(&bin, pipeline.home());

    // Sanity / negative half: the landing is the Hangar issue list, and it does
    // NOT already show the autopilot row (so a later match proves a switch).
    assert!(
        landing.contains(READY_MARKER),
        "expected the Hangar issue-list landing before pressing 4:\n{landing}"
    );
    assert!(
        !landing.contains(AUTOPILOT_NAME),
        "the issue list must not already show the seeded autopilot name:\n{landing}"
    );

    // Press `4` (single-char nav, no Enter) until the manager shows the seeded
    // autopilot's name AND cron. 45s budget covers the snapshot round-trip.
    let deadline = Instant::now() + Duration::from_secs(45);
    let pane = session
        .switch_tab_until("4", deadline, |c| {
            hangar_chrome_visible(c) && c.contains(AUTOPILOT_NAME) && c.contains(AUTOPILOT_CRON)
        })
        .unwrap_or_else(|| {
            panic!(
                "Autopilots manager never rendered the seeded autopilot:\n{}",
                session.capture()
            )
        });

    // POSITIVE: the seeded autopilot's name + cron rendered.
    assert!(
        pane.contains(AUTOPILOT_NAME),
        "seeded autopilot name {AUTOPILOT_NAME:?} missing from the manager:\n{pane}"
    );
    assert!(
        pane.contains(AUTOPILOT_CRON),
        "seeded autopilot cron {AUTOPILOT_CRON:?} missing from the manager:\n{pane}"
    );

    // POSITIVE: the manager body chrome rendered (not just a stray row), so a
    // match is the real Autopilots manager and not incidental text. 63l.6
    // replaced the NAME/CRON table with the card-board + a per-autopilot
    // run-history pane (`─ Recent runs (<name>) ─`); that pane header is
    // autopilots-body-specific (absent from the persistent tab strip).
    assert!(
        pane.contains("Recent runs"),
        "the autopilots manager body chrome (run-history pane) is missing:\n{pane}"
    );

    // NEGATIVE: no longer on the issue-list landing (the `5` switch happened).
    assert!(
        !pane.contains(READY_MARKER),
        "still on the issue list after pressing 4 (no screen switch):\n{pane}"
    );

    drop(session);
    drop(pipeline);
}
