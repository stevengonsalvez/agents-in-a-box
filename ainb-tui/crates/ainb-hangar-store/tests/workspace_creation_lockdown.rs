//! Behavioural port of multica's `TestCreateWorkspace_DisabledByConfig`
//! (`90ddfb04e feat(self-host): DISABLE_WORKSPACE_CREATION`).
//!
//! Multica gates `CreateWorkspace` on an env-only flag and asserts two things at
//! once: the call is refused (403) AND `SELECT count(*) FROM workspace WHERE
//! slug=$1` is zero. Hangar's flag is the persisted `daemon_config` row
//! `workspace.creation_disabled` (the acceptance demands sqlite/config), so these
//! tests drive it through [`DaemonConfigRepo`] and assert the same two invariants
//! plus the two hangar-specific ones the store owes:
//!
//! - the gate writes NOTHING — not the `workspace` row and not the owner `member`
//!   row, which is a separate INSERT in the same transaction;
//! - the platform-owned bootstrap seed is NOT gated, so a locked-down instance
//!   still comes up on a fresh DB.
//!
//! The env override's precedence is proven purely in
//! `ainb_hangar_core::daemon_config`; these tests deliberately never touch process
//! env, which is shared across every test in this binary.

use ainb_hangar_core::daemon_config::KEY_WORKSPACE_CREATION_DISABLED;
use ainb_hangar_store::Store;
use ainb_hangar_store::bootstrap::ensure_default_workspace;
use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;
use ainb_hangar_store::repo::workspace::{WorkspaceRepo, WorkspaceRepoError};
use sqlx::SqlitePool;

/// A fresh store with the bootstrap seed applied (a default workspace + its owner
/// user), which is the state `WorkspaceRepo::create` needs to resolve an owner.
async fn seeded_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    ensure_default_workspace(store.pool()).await.expect("bootstrap seed");
    (dir, store)
}

async fn count_workspaces(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM workspace")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn count_workspaces_with_slug(pool: &SqlitePool, slug: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM workspace WHERE slug = ?")
        .bind(slug)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn count_members(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM member").fetch_one(pool).await.unwrap()
}

/// Baseline: with the knob unset (the shipped default), create works. Without
/// this the lockdown tests below could pass on a create that was broken for an
/// unrelated reason.
#[tokio::test]
async fn create_succeeds_when_flag_unset() {
    let (_dir, store) = seeded_store().await;
    let row = WorkspaceRepo::create(store.pool(), "acme", "Acme", None)
        .await
        .expect("create with the lockdown unset");
    assert_eq!(row.slug, "acme");
    assert_eq!(count_workspaces_with_slug(store.pool(), "acme").await, 1);
}

/// Multica's assertion, ported: flag on → refused, and NO row written.
///
/// The row check is the load-bearing half. A gate placed after the INSERT would
/// still return an error while leaving a workspace (and an owner member) behind,
/// so both counts are pinned, not just the error variant.
#[tokio::test]
async fn flag_on_refuses_create_and_writes_no_row() {
    let (_dir, store) = seeded_store().await;
    let workspaces_before = count_workspaces(store.pool()).await;
    let members_before = count_members(store.pool()).await;

    DaemonConfigRepo::set(store.pool(), KEY_WORKSPACE_CREATION_DISABLED, "true")
        .await
        .expect("set the lockdown");

    let err = WorkspaceRepo::create(store.pool(), "acme", "Acme", None)
        .await
        .expect_err("create must be refused under lockdown");
    assert!(
        matches!(err, WorkspaceRepoError::CreationDisabled),
        "expected CreationDisabled, got {err:?}"
    );

    assert_eq!(
        count_workspaces_with_slug(store.pool(), "acme").await,
        0,
        "the refused workspace must not exist"
    );
    assert_eq!(
        count_workspaces(store.pool()).await,
        workspaces_before,
        "the workspace table must be untouched"
    );
    assert_eq!(
        count_members(store.pool()).await,
        members_before,
        "the owner member insert must not have run either"
    );
}

/// The gate is the flag, not a one-way break: clearing the row restores create.
#[tokio::test]
async fn clearing_the_flag_restores_create() {
    let (_dir, store) = seeded_store().await;
    DaemonConfigRepo::set(store.pool(), KEY_WORKSPACE_CREATION_DISABLED, "true")
        .await
        .unwrap();
    assert!(WorkspaceRepo::create(store.pool(), "acme", "Acme", None).await.is_err());

    DaemonConfigRepo::set(store.pool(), KEY_WORKSPACE_CREATION_DISABLED, "false")
        .await
        .unwrap();
    WorkspaceRepo::create(store.pool(), "acme", "Acme", None)
        .await
        .expect("create after clearing the lockdown");
    assert_eq!(count_workspaces_with_slug(store.pool(), "acme").await, 1);
}

/// Platform-owned creation is NOT gated (multica leaves bootstrap untouched): a
/// locked-down instance must still seed its default workspace on a fresh DB, or
/// the flag would brick a first boot.
#[tokio::test]
async fn bootstrap_still_seeds_under_lockdown() {
    // A genuinely fresh DB — migrations applied, nothing seeded — with the
    // lockdown already on, which is the first-boot-of-a-locked-instance case.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    DaemonConfigRepo::set(store.pool(), KEY_WORKSPACE_CREATION_DISABLED, "true")
        .await
        .unwrap();
    assert_eq!(count_workspaces(store.pool()).await, 0);

    ensure_default_workspace(store.pool())
        .await
        .expect("bootstrap must still seed under lockdown");

    assert_eq!(
        count_workspaces(store.pool()).await,
        1,
        "the default workspace must be seeded despite the lockdown"
    );
    assert!(
        count_members(store.pool()).await >= 1,
        "the bootstrap owner member must be seeded too"
    );
}

/// A corrupt knob value falls back to the coded default (allowed). A malformed
/// row must never brick workspace creation — the same tolerance
/// `DaemonConfigRepo::get_bool` gives every other knob.
#[tokio::test]
async fn malformed_flag_value_is_treated_as_allowed() {
    let (_dir, store) = seeded_store().await;
    DaemonConfigRepo::set(store.pool(), KEY_WORKSPACE_CREATION_DISABLED, "maybe")
        .await
        .unwrap();
    WorkspaceRepo::create(store.pool(), "acme", "Acme", None)
        .await
        .expect("a malformed lockdown value must not brick create");
    assert_eq!(count_workspaces_with_slug(store.pool(), "acme").await, 1);
}
