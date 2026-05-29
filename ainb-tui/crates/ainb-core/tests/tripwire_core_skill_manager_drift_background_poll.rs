//! Tripwire: GoToSkillManager triggers a background DriftDetector
//! scan; the result lands in `SkillsScreenData::drift_cache` on the
//! next AppState tick.
//!
//! Catches: the drift-poll producer being dropped from the
//! GoToSkillManager handler; the receiver going stale; the tick not
//! draining drift results into the screen state; a future refactor
//! that swaps the channel kind without updating the consumer.
//!
//! The detector is mocked so the test never hits `git ls-remote`.
//!
//! Bead: v12.E.4 — background drift poll on screen-enter.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ainb_skill_core::drift::{DriftStatus, MockBackend};
use ainb_skill_core::lockfile::{LockedSource, LockedUnit, Lockfile};
use ainb_skill_core::manifest::{Manifest, SourceEntry};
use ainb_skill_core::paths::{lockfile_path_in, manifest_path_in};

fn seed_state(home: &Path) {
    let manifest = Manifest {
        schema_version: 1,
        sources: vec![SourceEntry {
            name: "acme".to_string(),
            kind: None,
            uri: "gh:org/acme".to_string(),
            r#ref: "main".to_string(),
            enabled: true,
            read_only: false,
            target_layout: Vec::new(),
        }],
        units: Vec::new(),
        defaults: None,
        options: None,
    };
    manifest.save_to(&manifest_path_in(home)).expect("save manifest");

    let lockfile = Lockfile {
        schema_version: 2,
        generated_at: None,
        sources: vec![LockedSource {
            name: "acme".to_string(),
            uri: "gh:org/acme".to_string(),
            declared_ref: "main".to_string(),
            resolved_sha: None,
            fetched_at: None,
            fetched_path: None,
        }],
        units: vec![LockedUnit {
            uri: "gh:org/acme@main/skills/foo".to_string(),
            declared_uri: "gh:org/acme@main/skills/foo".to_string(),
            kind: "skill".to_string(),
            sha: Some("deadbeef".to_string()),
            deployed: BTreeMap::new(),
            usage: Default::default(),
        }],
    };
    lockfile.save_to(&lockfile_path_in(home)).expect("save lockfile");
}

#[tokio::test]
async fn drift_background_poll_populates_cache_on_tick() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    seed_state(home);

    // Mocked detector: any (acme, deadbeef) query returns Outdated{4}.
    let mut mock = MockBackend::new();
    mock.expect(
        "acme",
        "deadbeef",
        DriftStatus::Outdated { behind: 4 },
    );

    let mut state = ainb::app::AppState::new();
    // Pre-condition: cache empty.
    assert!(state.skill_manager_state.drift_cache.is_empty());

    // Kick off the background poll the way GoToSkillManager would.
    let spawned = state.start_background_drift_load(home, Arc::new(mock));
    assert!(spawned, "first start should actually spawn a task");

    // The worker runs on the tokio pool; give it a moment to land in
    // the receiver, then poll.
    for _ in 0..50 {
        if state.check_drift_load_complete() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(
        state.skill_manager_state.drift_cache.len(),
        1,
        "expected one drift entry to land, got: {:?}",
        state.skill_manager_state.drift_cache
    );
    assert_eq!(
        state
            .skill_manager_state
            .drift_cache
            .get("gh:org/acme@main/skills/foo"),
        Some(&DriftStatus::Outdated { behind: 4 })
    );
}

#[tokio::test]
async fn drift_background_poll_coalesces_in_flight_calls() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    seed_state(home);

    let mut mock = MockBackend::new();
    mock.expect("acme", "deadbeef", DriftStatus::InSync);

    let mut state = ainb::app::AppState::new();
    let first = state.start_background_drift_load(home, Arc::new(mock.clone()));
    let second = state.start_background_drift_load(home, Arc::new(mock));
    assert!(first, "first call should spawn");
    assert!(
        !second,
        "second call must coalesce while the first is in flight"
    );

    for _ in 0..50 {
        if state.check_drift_load_complete() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(state.skill_manager_state.drift_cache.len(), 1);
}
