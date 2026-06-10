//! P5.5 — workspace switch tripwire (tmux-driven, end-to-end).
//!
//! Drives the real `ainb tui` against a seeded two-workspace hangar:
//!
//! 1. open Settings (`,`),
//! 2. navigate to the Workspace pane (`j` to the 4th section),
//! 3. select the second workspace row (`J`),
//! 4. press `s` to set it active,
//! 5. assert the pane re-renders with the second workspace marked active
//!    (its `▶` slug indicator) and the daemon-side switch state followed.
//!
//! Honours the `tmux-ui-tripwire` HARD RULES via the shared P4 harness
//! ([`tripwire_p4_common`]): exact-name `tmux kill-session` only, `poll_capture`
//! (no bare sleep), single-char nav without `Enter`, POSITIVE paired with a
//! NEGATIVE assertion, and SKIP-not-fail when tmux / the binaries / the staged
//! plugin are missing.
//!
//! The seed is the P4 fixture PLUS a second workspace inserted here (the shared
//! fixture seeds one workspace; switching needs two). The second workspace's
//! slug is the on-screen marker the switch is asserted against.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, can_run_tripwire, prepare_pipeline, skip};

/// The second workspace's slug — the marker the switch is asserted against.
const SECOND_SLUG: &str = "acme";

/// Insert a second workspace into the seeded hangar.db so the Workspace pane has
/// something to switch to. Runs after `prepare_pipeline` seeded the P4 fixture.
fn seed_second_workspace(home: &std::path::Path) {
    let hangar_dir = home.join(".ainb");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open seed store");
        // A distinct ULID-style id + the `acme` slug (id != slug, mirroring real
        // workspaces and the slug/id resolution contract).
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("01JWSACME0000000000000000")
            .bind(SECOND_SLUG)
            .bind("Acme")
            .bind(1_700_000_000_000_i64)
            .execute(store.pool())
            .await
            .expect("insert second workspace");
    });
}

#[test]
fn workspace_switch_e2e() {
    if !can_run_tripwire() {
        skip("workspace_switch");
        return;
    }
    let pipe = prepare_pipeline();
    seed_second_workspace(pipe.home());

    let bin = common::ainb_bin().expect("gated by can_run_tripwire");
    // Stand the TUI up to the seeded hangar issue list. The environment is
    // already gated by `can_run_tripwire()`, so a render timeout here is a real
    // regression, not a missing prerequisite. This used to SKIP (citing the
    // long-resolved P4.9 render blocker), which silently masked the notifyd
    // first-run dialog swallowing the `g` nav on CI. Fail loud instead.
    let sess = TuiSession::spawn(&bin, pipe.home());
    assert!(
        sess.open_hangar_and_wait_ready().is_some(),
        "hangar issue list never rendered (precondition):\n{}",
        sess.capture()
    );

    // 1. Open Settings.
    sess.send_key(",");
    let settings = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("Daemon") && c.contains("Workspaces")
        })
        .expect("settings never rendered");
    // NEGATIVE: not on the issue list anymore.
    assert!(
        !settings.contains("Refactor API"),
        "still on the issue list:\n{settings}"
    );

    // 2. Navigate down to the Workspace pane (Daemon → Providers → Keys →
    //    Workspaces). A single `j` can be dropped by tmux on a busy first
    //    frame, so re-send until the active-section marker sits on `Workspaces`
    //    (`▶ Workspaces`), bounded by a deadline.
    let nav_deadline = Instant::now() + Duration::from_secs(20);
    let on_pane = loop {
        sess.send_key("j");
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(800), |c| {
            active_marker_on_slug(c, "Workspaces")
        }) {
            break Some(c);
        }
        if Instant::now() >= nav_deadline {
            break None;
        }
    };
    let on_pane = on_pane.unwrap_or_else(|| {
        panic!(
            "never reached the Workspace pane (`▶ Workspaces`):\n{}",
            sess.capture()
        )
    });
    assert!(
        active_marker_on_slug(&on_pane, "Workspaces"),
        "not on the Workspace pane:\n{on_pane}"
    );

    // Entering the pane triggers the host workspace-list fetch (`Refresh`);
    // poll until the second workspace `acme` (from the seed) appears.
    let listed = sess
        .poll_capture(Instant::now() + Duration::from_secs(10), |c| {
            c.contains(SECOND_SLUG)
        })
        .unwrap_or_else(|| {
            panic!(
                "second workspace `{SECOND_SLUG}` never listed:\n{}",
                sess.capture()
            )
        });
    assert!(
        listed.contains(SECOND_SLUG),
        "second workspace missing:\n{listed}"
    );

    // 3. Select the second workspace row.
    sess.send_key("J");

    // 4. Press `s` to set the selected workspace active.
    sess.send_key("s");

    // 5. POSITIVE: the pane re-renders with the second workspace marked active
    //    (the `▶` indicator now precedes its slug). Poll until the active
    //    indicator sits on the `acme` row.
    let switched = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            active_marker_on_slug(c, SECOND_SLUG)
        })
        .unwrap_or_else(|| {
            panic!(
                "workspace switch to `{SECOND_SLUG}` never reflected in the pane:\n{}",
                sess.capture()
            )
        });
    assert!(
        active_marker_on_slug(&switched, SECOND_SLUG),
        "active `▶` indicator not on `{SECOND_SLUG}`:\n{switched}"
    );
}

/// `true` when a rendered line carries the active `▶` indicator AND the given
/// slug — i.e. that workspace is the active one.
fn active_marker_on_slug(capture: &str, slug: &str) -> bool {
    capture.lines().any(|line| line.contains('▶') && line.contains(slug))
}
