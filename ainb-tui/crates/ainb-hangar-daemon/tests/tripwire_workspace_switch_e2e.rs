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
//! 6. (e38.26) switch back to the Issues tab (`1`) and assert the issue list
//!    now shows the SECOND workspace's issue and never the first's — proving
//!    the switch re-scopes the DATA plane, not just the active `▶` marker.
//!
//! Honours the `tmux-ui-tripwire` HARD RULES via the shared P4 harness
//! ([`tripwire_p4_common`]): exact-name `tmux kill-session` only, `poll_capture`
//! (no bare sleep), single-char nav without `Enter`, POSITIVE paired with a
//! NEGATIVE assertion, and SKIP-not-fail when tmux / the binaries / the staged
//! plugin are missing.
//!
//! The seed is the P4 fixture PLUS a second workspace inserted here (the shared
//! fixture seeds one workspace; switching needs two). Workspace A (the fixture's
//! `default`) carries `Refactor API`; workspace B (`acme`) carries
//! `Acme Login Bug` — distinct issues so the post-switch data assertion can tell
//! the two tenants' issue lists apart. The second workspace's slug is the
//! on-screen marker the active switch is asserted against; with `id != slug`
//! (mirroring real workspaces) the assertion can't be masked by an `id == slug`
//! fixture.

use std::time::{Duration, Instant};

#[path = "tripwire_p4_common.rs"]
mod common;
use common::{TuiSession, can_run_tripwire, prepare_pipeline, skip};

/// The second workspace's slug — the marker the switch is asserted against.
const SECOND_SLUG: &str = "acme";

/// The second workspace's row id — deliberately distinct from its slug, so the
/// switch + data-scoping exercise the real slug→id resolution (an `id == slug`
/// fixture would mask a slug-keyed query bug).
const SECOND_WS_ID: &str = "01JWSACME0000000000000000";

/// The first workspace's issue title (from the P4 fixture) — must DISAPPEAR from
/// the issue list once the active workspace switches to the second one.
const FIRST_WS_ISSUE: &str = "Refactor API";

/// The second workspace's issue title — must APPEAR in the issue list once the
/// active workspace switches to it.
const SECOND_WS_ISSUE: &str = "Acme Login Bug";

/// Insert a second workspace (with its own `open` issue) into the seeded
/// hangar.db so the Workspace pane has something to switch to AND the post-switch
/// issue list has distinct data to prove the switch re-scopes the data plane.
/// Runs after `prepare_pipeline` seeded the P4 fixture.
fn seed_second_workspace(home: &std::path::Path) {
    use ainb_hangar_core::actor::{ActorKind, ActorRef};
    use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};

    let hangar_dir = home.join(".agents-in-a-box");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("seed runtime");
    rt.block_on(async {
        let store = ainb_hangar_store::Store::open_in(&hangar_dir).await.expect("open seed store");
        // A distinct ULID-style id + the `acme` slug (id != slug, mirroring real
        // workspaces and the slug/id resolution contract).
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(SECOND_WS_ID)
            .bind(SECOND_SLUG)
            .bind("Acme")
            .bind(1_700_000_000_000_i64)
            .execute(store.pool())
            .await
            .expect("insert second workspace");

        // The second workspace's own `open` issue. The creator is a member ref
        // (no agent/runtime needed for an issue-list row); `open` is one of the
        // states the issue-list snapshot surfaces. This is the row the
        // post-switch data assertion proves is now visible (and the first
        // workspace's `Refactor API` is gone).
        let creator = ActorRef::new(ActorKind::Member, "user-1").expect("member creator ref");
        IssueRepo::insert(
            store.pool(),
            &NewIssue {
                id: "issue-acme-1".into(),
                workspace_id: SECOND_WS_ID.into(),
                title: SECOND_WS_ISSUE.into(),
                description: Some("Login fails on the Acme tenant".into()),
                state: "open".into(),
                assignee: None,
                creator,
                created_at: 1_700_000_000_000,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                parent_issue_id: None,
                stage: None,
            },
        )
        .await
        .expect("insert second-workspace issue");
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
    // Match on section titles that ONLY the Settings screen renders
    // (`Providers` + `LLM Keys`). The earlier `Daemon`+`Workspaces` predicate
    // was not Settings-unique: `Daemon` is a permanent tab-strip label present
    // on every screen, and the host's transient `✓ Workspaces loaded` toast
    // paints `Workspaces` over the issue list — so the poll could return the
    // pre-switch issue-list frame and the negative assert below would fire.
    let settings = sess
        .poll_capture(Instant::now() + Duration::from_secs(15), |c| {
            c.contains("Providers") && c.contains("LLM Keys")
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

    // 6. (e38.26) The switch must re-scope the DATA plane, not just the active
    //    marker. Return to the Issues tab (`1`) and prove the issue list now
    //    reflects the SECOND workspace: it shows `Acme Login Bug` (POSITIVE) and
    //    no longer shows the first workspace's `Refactor API` (NEGATIVE). The tab
    //    key can race the snapshot re-fetch, so re-send `1` until the second
    //    workspace's issue appears, bounded by a deadline.
    let issues = sess
        .switch_tab_until("1", Instant::now() + Duration::from_secs(20), |c| {
            c.contains(SECOND_WS_ISSUE)
        })
        .unwrap_or_else(|| {
            panic!(
                "issue list never re-scoped to `{SECOND_SLUG}` (no `{SECOND_WS_ISSUE}`):\n{}",
                sess.capture()
            )
        });
    // POSITIVE: the second workspace's issue is visible.
    assert!(
        issues.contains(SECOND_WS_ISSUE),
        "issue list missing the second workspace's `{SECOND_WS_ISSUE}`:\n{issues}"
    );
    // NEGATIVE: the first workspace's issue must NOT leak across the switch.
    assert!(
        !issues.contains(FIRST_WS_ISSUE),
        "cross-tenant leak: first workspace's `{FIRST_WS_ISSUE}` still on screen \
         after switching to `{SECOND_SLUG}`:\n{issues}"
    );
}

/// `true` when a rendered line carries the active `▶` indicator AND the given
/// slug — i.e. that workspace is the active one.
fn active_marker_on_slug(capture: &str, slug: &str) -> bool {
    capture.lines().any(|line| line.contains('▶') && line.contains(slug))
}
