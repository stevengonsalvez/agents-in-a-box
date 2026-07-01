//! SyncPlanner (`plan_sync`) tests — bead v12.D.1.
//!
//! `plan_sync` is a **pure** planner: given a source, its effective
//! folder mappings, and a pre-observed snapshot of each unit's home/repo
//! state, it decides — per unit — whether to push the local edit up
//! (`ToRepo`), pull the upstream change down (`ToHome`), or do nothing
//! (`NoOp`). It performs no I/O: the impure work (hashing files, reading
//! mtimes, resolving the repo HEAD) is the caller's job and arrives as
//! [`UnitSnapshot`]s, so the decision logic is trivially testable.
//!
//! Decision precedence: git-ref comparison (`deployed_sha` + per-side
//! `content_sha`) when refs are available, falling back to mtime when a
//! `deployed_sha` is missing.

use std::path::PathBuf;

use ainb_skill_core::manifest::{SourceEntry, TargetMapping};
use ainb_skill_core::sync::{SideSnapshot, SyncDirection, UnitSnapshot, plan_sync};

// ---- fixtures -------------------------------------------------------------

/// A writable source with no explicit `target_layout` (so `resolve_pair`
/// falls back to the bootstrap defaults — `skills/*/SKILL.md` matches).
fn source() -> SourceEntry {
    SourceEntry {
        name: "test-source".into(),
        kind: Some("manifest".into()),
        uri: "gh:org/repo".into(),
        r#ref: "main".into(),
        enabled: true,
        read_only: false,
        target_layout: vec![],
    }
}

fn read_only_source() -> SourceEntry {
    SourceEntry {
        read_only: true,
        ..source()
    }
}

/// Side snapshot carrying a content hash (git-ref path).
fn sha(s: &str) -> SideSnapshot {
    SideSnapshot {
        content_sha: Some(s.into()),
        mtime: None,
    }
}

/// Side snapshot carrying only an mtime (fallback path).
fn at(mtime: i64) -> SideSnapshot {
    SideSnapshot {
        content_sha: None,
        mtime: Some(mtime),
    }
}

fn unit(
    name: &str,
    deployed_sha: Option<&str>,
    home: Option<SideSnapshot>,
    repo: Option<SideSnapshot>,
) -> UnitSnapshot {
    UnitSnapshot {
        unit_name: name.into(),
        // A path the bootstrap defaults cover, so eligibility passes with
        // an empty `target_layout`.
        unit_path: PathBuf::from(format!("skills/{name}/SKILL.md")),
        deployed_sha: deployed_sha.map(str::to_string),
        home,
        repo,
    }
}

/// Plan a single unit and return its sole action.
fn plan_one(
    src: &SourceEntry,
    mappings: &[TargetMapping],
    u: UnitSnapshot,
) -> (SyncDirection, String) {
    let plan = plan_sync(src, mappings, std::slice::from_ref(&u));
    assert_eq!(plan.len(), 1, "one action per unit");
    assert_eq!(plan[0].unit_name, u.unit_name, "action keyed to its unit");
    (plan[0].direction, plan[0].reason.clone())
}

// ---- the four mandated cases ---------------------------------------------

#[test]
fn home_modified_since_deploy_yields_to_repo() {
    // Local edit on home (home != deployed), repo untouched (repo == deployed).
    let u = unit("commit", Some("d"), Some(sha("h")), Some(sha("d")));
    let (dir, reason) = plan_one(&source(), &[], u);
    assert_eq!(dir, SyncDirection::ToRepo);
    assert!(!reason.is_empty(), "reason must explain the choice");
}

#[test]
fn repo_updated_upstream_yields_to_home() {
    // Upstream moved (repo != deployed), home still on the deployed sha.
    let u = unit("commit", Some("d"), Some(sha("d")), Some(sha("r")));
    let (dir, _) = plan_one(&source(), &[], u);
    assert_eq!(dir, SyncDirection::ToHome);
}

#[test]
fn identical_home_and_repo_yields_noop() {
    let u = unit("commit", Some("d"), Some(sha("x")), Some(sha("x")));
    let (dir, _) = plan_one(&source(), &[], u);
    assert_eq!(dir, SyncDirection::NoOp);
}

#[test]
fn missing_deployed_sha_falls_back_to_mtime() {
    // No deployed_sha → compare mtimes. Home newer ⇒ ToRepo.
    let newer_home = unit("commit", None, Some(at(200)), Some(at(100)));
    let (dir, reason) = plan_one(&source(), &[], newer_home);
    assert_eq!(dir, SyncDirection::ToRepo);
    assert!(
        reason.to_lowercase().contains("mtime"),
        "fallback reason should mention mtime, got: {reason}"
    );

    // Repo newer ⇒ ToHome.
    let newer_repo = unit("commit", None, Some(at(100)), Some(at(200)));
    let (dir, _) = plan_one(&source(), &[], newer_repo);
    assert_eq!(dir, SyncDirection::ToHome);

    // Same mtime ⇒ NoOp.
    let same = unit("commit", None, Some(at(150)), Some(at(150)));
    let (dir, _) = plan_one(&source(), &[], same);
    assert_eq!(dir, SyncDirection::NoOp);
}

// ---- edge cases -----------------------------------------------------------

#[test]
fn both_sides_diverged_is_conflict_noop() {
    // home != deployed AND repo != deployed AND home != repo → unresolvable
    // automatically; planner refuses to clobber either side.
    let u = unit("commit", Some("d"), Some(sha("h")), Some(sha("r")));
    let (dir, reason) = plan_one(&source(), &[], u);
    assert_eq!(dir, SyncDirection::NoOp);
    assert!(
        reason.to_lowercase().contains("conflict"),
        "diverged-both reason should flag a conflict, got: {reason}"
    );
}

#[test]
fn home_only_unit_yields_to_repo() {
    // Exists on home, absent in repo → publish upstream.
    let u = unit("commit", None, Some(sha("h")), None);
    let (dir, _) = plan_one(&source(), &[], u);
    assert_eq!(dir, SyncDirection::ToRepo);
}

#[test]
fn repo_only_unit_yields_to_home() {
    // Exists in repo, not deployed to home → pull down.
    let u = unit("commit", None, None, Some(sha("r")));
    let (dir, _) = plan_one(&source(), &[], u);
    assert_eq!(dir, SyncDirection::ToHome);
}

#[test]
fn absent_both_sides_yields_noop() {
    let u = unit("commit", None, None, None);
    let (dir, _) = plan_one(&source(), &[], u);
    assert_eq!(dir, SyncDirection::NoOp);
}

#[test]
fn read_only_source_never_pushes_to_repo() {
    // A would-be ToRepo (home edited) is downgraded to NoOp because the
    // source is externally managed.
    let u = unit("commit", Some("d"), Some(sha("h")), Some(sha("d")));
    let (dir, reason) = plan_one(&read_only_source(), &[], u);
    assert_eq!(dir, SyncDirection::NoOp);
    assert!(
        reason.to_lowercase().contains("read"),
        "read-only downgrade should explain itself, got: {reason}"
    );
}

#[test]
fn read_only_source_still_pulls_to_home() {
    // ToHome is fine against a read-only source — we only refuse writes
    // *to* the repo.
    let u = unit("commit", Some("d"), Some(sha("d")), Some(sha("r")));
    let (dir, _) = plan_one(&read_only_source(), &[], u);
    assert_eq!(dir, SyncDirection::ToHome);
}

#[test]
fn unit_outside_target_layout_is_noop() {
    // A path no mapping covers (not under skills/agents/commands) → the
    // planner can't place it, so it declines.
    let u = UnitSnapshot {
        unit_name: "stray".into(),
        unit_path: PathBuf::from("hooks/pre-commit.sh"),
        deployed_sha: Some("d".into()),
        home: Some(sha("h")),
        repo: Some(sha("d")),
    };
    let (dir, reason) = plan_one(&source(), &[], u);
    assert_eq!(dir, SyncDirection::NoOp);
    assert!(
        reason.to_lowercase().contains("mapping") || reason.to_lowercase().contains("layout"),
        "uncovered unit should mention the missing mapping, got: {reason}"
    );
}

#[test]
fn explicit_mappings_override_source_layout() {
    // Passing `mappings` scopes eligibility even when the source declares
    // its own (here empty) layout. A custom glob covers `widgets/*`.
    let mappings = vec![TargetMapping {
        glob: "widgets/*/UNIT.md".into(),
        home: PathBuf::from(".tool/widgets"),
        repo: PathBuf::from("pkg/widgets"),
    }];
    let u = UnitSnapshot {
        unit_name: "gauge".into(),
        unit_path: PathBuf::from("widgets/gauge/UNIT.md"),
        deployed_sha: Some("d".into()),
        home: Some(sha("h")),
        repo: Some(sha("d")),
    };
    let (dir, _) = plan_one(&source(), &mappings, u);
    assert_eq!(
        dir,
        SyncDirection::ToRepo,
        "custom mapping makes it eligible"
    );
}

#[test]
fn plan_preserves_unit_order_one_action_each() {
    let units = vec![
        unit("a", Some("d"), Some(sha("h")), Some(sha("d"))), // ToRepo
        unit("b", Some("d"), Some(sha("d")), Some(sha("r"))), // ToHome
        unit("c", Some("d"), Some(sha("x")), Some(sha("x"))), // NoOp
    ];
    let plan = plan_sync(&source(), &[], &units);
    assert_eq!(plan.len(), 3);
    assert_eq!(plan[0].unit_name, "a");
    assert_eq!(plan[0].direction, SyncDirection::ToRepo);
    assert_eq!(plan[1].unit_name, "b");
    assert_eq!(plan[1].direction, SyncDirection::ToHome);
    assert_eq!(plan[2].unit_name, "c");
    assert_eq!(plan[2].direction, SyncDirection::NoOp);
}
