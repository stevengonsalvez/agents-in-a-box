//! Custom-property catalog + per-issue value bag (multica parity #17,
//! migration 0066).
//!
//! Pins the four behaviours the reference exists for:
//!
//! 1. values are keyed by DEFINITION ID, so a rename touches zero issue rows,
//! 2. ARCHIVE never deletes — the stored value survives and comes back,
//! 3. typed validation against the catalog rejects bad writes,
//! 4. the caps (20 active definitions / workspace, 16 KB value bag) bite.

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::properties::{
    MAX_ACTIVE_PROPERTIES, PropertyError, PropertyKind, PropertyValue,
};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::issue::IssueRepo;
use ainb_hangar_store::repo::issue_property::{IssuePropertyRepo, PropertyRepoError};
use sqlx::SqlitePool;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::from_str(id).expect("non-empty workspace id")
}

fn opts(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

async fn seed_ws(pool: &SqlitePool, id: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .expect("seed workspace");
}

async fn seed_issue(pool: &SqlitePool, id: &str, workspace: &str) {
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
         VALUES (?, ?, 't', 'open', 'member', 'm1', 0)",
    )
    .bind(id)
    .bind(workspace)
    .execute(pool)
    .await
    .expect("seed issue");
}

/// One workspace `ws-a` with issue `iss-1`, plus a second tenant `ws-b` with
/// `iss-b` so every tenant-guard assertion has a real foreign row to aim at.
async fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_ws(store.pool(), "ws-a").await;
    seed_ws(store.pool(), "ws-b").await;
    seed_issue(store.pool(), "iss-1", "ws-a").await;
    seed_issue(store.pool(), "iss-b", "ws-b").await;
    (dir, store)
}

/// The raw `issue.properties` blob, so a test can assert on the JSON KEY (the
/// definition id) rather than the resolved view.
async fn raw_bag(pool: &SqlitePool, issue_id: &str) -> String {
    sqlx::query_scalar("SELECT properties FROM issue WHERE id = ?")
        .bind(issue_id)
        .fetch_one(pool)
        .await
        .expect("read properties column")
}

async fn define_sprint(store: &Store) -> String {
    IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "sprint",
        "Sprint",
        &PropertyKind::Select,
        &opts(&["S1", "S2"]),
        0,
        1,
    )
    .await
    .expect("define sprint")
    .id
}

#[tokio::test]
async fn define_lists_in_position_order_and_redefine_keeps_the_id() {
    let (_dir, store) = setup().await;
    let sprint_id = define_sprint(&store).await;
    IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "owner",
        "Owner",
        &PropertyKind::Text,
        &[],
        -1,
        2,
    )
    .await
    .expect("define owner");

    let listed = IssuePropertyRepo::list(store.pool(), &ws("ws-a"), false).await.expect("list");
    let keys: Vec<&str> = listed.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, vec!["owner", "sprint"], "ORDER BY position, key");

    // A second define of the same key is a resolve-or-UPDATE, not a new row.
    let again = IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "sprint",
        "Sprint",
        &PropertyKind::Select,
        &opts(&["S1", "S2", "S3"]),
        5,
        3,
    )
    .await
    .expect("redefine sprint");
    assert_eq!(again.id, sprint_id, "the id must survive a re-define");
    assert_eq!(again.options, opts(&["S1", "S2", "S3"]));
    assert_eq!(again.position, 5);
    assert_eq!(
        IssuePropertyRepo::list(store.pool(), &ws("ws-a"), false)
            .await
            .expect("list")
            .len(),
        2,
        "re-define never mints a second definition"
    );
}

#[tokio::test]
async fn renaming_a_property_touches_zero_issue_rows() {
    let (_dir, store) = setup().await;
    let sprint_id = define_sprint(&store).await;
    IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "sprint",
        &PropertyValue::Text("S2".into()),
    )
    .await
    .expect("set sprint");

    let before = raw_bag(store.pool(), "iss-1").await;
    assert!(
        before.contains(&sprint_id),
        "the JSON key is the DEFINITION ID, got {before}"
    );
    assert!(
        !before.contains("Sprint") && !before.contains("sprint"),
        "neither the display name nor the key may appear in the bag: {before}"
    );

    // The rename: a catalog-only write.
    IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "sprint",
        "Iteration",
        &PropertyKind::Select,
        &opts(&["S1", "S2"]),
        0,
        4,
    )
    .await
    .expect("rename sprint -> Iteration");

    assert_eq!(
        raw_bag(store.pool(), "iss-1").await,
        before,
        "a rename must leave the issue's value blob BYTE-IDENTICAL"
    );
    let resolved = IssuePropertyRepo::values_for(store.pool(), &ws("ws-a"), "iss-1")
        .await
        .expect("values_for");
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].0.name, "Iteration",
        "renders under the new name"
    );
    assert_eq!(resolved[0].1, PropertyValue::Text("S2".into()));
}

#[tokio::test]
async fn archiving_hides_the_value_without_destroying_it() {
    let (_dir, store) = setup().await;
    define_sprint(&store).await;
    IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "sprint",
        &PropertyValue::Text("S2".into()),
    )
    .await
    .expect("set sprint");
    let stored = raw_bag(store.pool(), "iss-1").await;

    assert!(
        IssuePropertyRepo::set_archived(store.pool(), &ws("ws-a"), "sprint", true, 9)
            .await
            .expect("archive")
    );
    assert!(
        IssuePropertyRepo::values_for(store.pool(), &ws("ws-a"), "iss-1")
            .await
            .expect("values_for")
            .is_empty(),
        "an archived definition stops rendering"
    );
    assert_eq!(
        raw_bag(store.pool(), "iss-1").await,
        stored,
        "…but the stored value is untouched on disk"
    );
    assert!(
        IssuePropertyRepo::list(store.pool(), &ws("ws-a"), true)
            .await
            .expect("list archived")
            .iter()
            .any(|p| p.key == "sprint" && !p.is_active()),
        "archive is a tombstone, never a DELETE"
    );

    // Un-archive and it comes back with its value intact.
    assert!(
        IssuePropertyRepo::set_archived(store.pool(), &ws("ws-a"), "sprint", false, 10)
            .await
            .expect("un-archive")
    );
    let back = IssuePropertyRepo::values_for(store.pool(), &ws("ws-a"), "iss-1")
        .await
        .expect("values_for");
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].1, PropertyValue::Text("S2".into()));

    // Archiving an unknown key is a `false`, not an error.
    assert!(
        !IssuePropertyRepo::set_archived(store.pool(), &ws("ws-a"), "nope", true, 11)
            .await
            .expect("unknown key")
    );
}

#[tokio::test]
async fn the_active_definition_cap_bites_and_archiving_frees_a_slot() {
    let (_dir, store) = setup().await;
    for i in 0..MAX_ACTIVE_PROPERTIES {
        IssuePropertyRepo::define(
            store.pool(),
            &ws("ws-a"),
            &format!("k{i}"),
            "N",
            &PropertyKind::Text,
            &[],
            i as i64,
            1,
        )
        .await
        .unwrap_or_else(|e| panic!("definition {i} within the cap: {e}"));
    }
    let over = IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "one-too-many",
        "N",
        &PropertyKind::Text,
        &[],
        99,
        1,
    )
    .await;
    assert!(
        matches!(over, Err(PropertyRepoError::TooManyProperties)),
        "the 21st ACTIVE definition is rejected, got {over:?}"
    );

    IssuePropertyRepo::set_archived(store.pool(), &ws("ws-a"), "k0", true, 2)
        .await
        .expect("archive frees a slot");
    IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "one-too-many",
        "N",
        &PropertyKind::Text,
        &[],
        99,
        3,
    )
    .await
    .expect("a freed slot admits a new definition");

    // Un-archiving back over the cap is rejected rather than silently exceeding.
    let back = IssuePropertyRepo::set_archived(store.pool(), &ws("ws-a"), "k0", false, 4).await;
    assert!(
        matches!(back, Err(PropertyRepoError::TooManyProperties)),
        "un-archive respects the cap, got {back:?}"
    );
}

#[tokio::test]
async fn values_are_validated_against_the_catalog() {
    let (_dir, store) = setup().await;
    define_sprint(&store).await;
    IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "blocked",
        "Blocked",
        &PropertyKind::Checkbox,
        &[],
        1,
        1,
    )
    .await
    .expect("define checkbox");

    let bad_option = IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "sprint",
        &PropertyValue::Text("S9".into()),
    )
    .await;
    assert!(
        matches!(
            bad_option,
            Err(PropertyRepoError::Value(PropertyError::NotAnOption(_)))
        ),
        "a select value outside its options is rejected, got {bad_option:?}"
    );

    let bad_bool = IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "blocked",
        &PropertyValue::Text("maybe".into()),
    )
    .await;
    assert!(
        matches!(
            bad_bool,
            Err(PropertyRepoError::Value(PropertyError::KindMismatch { .. }))
        ),
        "a checkbox set to text is rejected, got {bad_bool:?}"
    );

    // A `select` defined with no options is rejected at DEFINE time.
    let no_options = IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "risk",
        "Risk",
        &PropertyKind::Select,
        &[],
        2,
        1,
    )
    .await;
    assert!(
        matches!(
            no_options,
            Err(PropertyRepoError::Value(PropertyError::OptionsRequired))
        ),
        "got {no_options:?}"
    );

    // An unknown key is PropertyNotFound, and nothing is written.
    let unknown = IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "nope",
        &PropertyValue::Text("x".into()),
    )
    .await;
    assert!(matches!(unknown, Err(PropertyRepoError::PropertyNotFound)));
    assert_eq!(raw_bag(store.pool(), "iss-1").await, "{}");
}

#[tokio::test]
async fn an_archived_definition_cannot_be_written_to() {
    let (_dir, store) = setup().await;
    define_sprint(&store).await;
    IssuePropertyRepo::set_archived(store.pool(), &ws("ws-a"), "sprint", true, 2)
        .await
        .expect("archive");
    let write = IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "sprint",
        &PropertyValue::Text("S1".into()),
    )
    .await;
    assert!(
        matches!(write, Err(PropertyRepoError::PropertyNotFound)),
        "only the ACTIVE catalog accepts writes, got {write:?}"
    );
}

#[tokio::test]
async fn a_foreign_tenant_issue_id_is_rejected_and_writes_nothing() {
    let (_dir, store) = setup().await;
    define_sprint(&store).await;
    let cross = IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-b", // lives in ws-b
        "sprint",
        &PropertyValue::Text("S2".into()),
    )
    .await;
    assert!(
        matches!(cross, Err(PropertyRepoError::IssueNotFound)),
        "got {cross:?}"
    );
    assert_eq!(raw_bag(store.pool(), "iss-b").await, "{}");
    assert_eq!(raw_bag(store.pool(), "iss-1").await, "{}");
    assert!(
        IssuePropertyRepo::values_for(store.pool(), &ws("ws-a"), "iss-b")
            .await
            .expect("values_for is workspace-scoped")
            .is_empty()
    );
}

#[tokio::test]
async fn a_value_bag_past_sixteen_kilobytes_is_rejected_and_the_prior_bag_survives() {
    let (_dir, store) = setup().await;
    IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "notes",
        "Notes",
        &PropertyKind::Text,
        &[],
        0,
        1,
    )
    .await
    .expect("define notes");
    IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "notes",
        &PropertyValue::Text("small".into()),
    )
    .await
    .expect("small value fits");
    let before = raw_bag(store.pool(), "iss-1").await;

    let huge = "x".repeat(20_000);
    let over = IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "notes",
        &PropertyValue::Text(huge),
    )
    .await;
    assert!(
        matches!(over, Err(PropertyRepoError::Value(PropertyError::TooLarge))),
        "got {over:?}"
    );
    assert_eq!(
        raw_bag(store.pool(), "iss-1").await,
        before,
        "the rejected write is fully rolled back"
    );
}

#[tokio::test]
async fn clear_removes_one_key_and_is_reported_when_absent() {
    let (_dir, store) = setup().await;
    define_sprint(&store).await;
    IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "owner",
        "Owner",
        &PropertyKind::Text,
        &[],
        1,
        1,
    )
    .await
    .expect("define owner");
    IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "sprint",
        &PropertyValue::Text("S1".into()),
    )
    .await
    .expect("set sprint");
    IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "owner",
        &PropertyValue::Text("amy".into()),
    )
    .await
    .expect("set owner");

    assert!(
        IssuePropertyRepo::clear_value(store.pool(), &ws("ws-a"), "iss-1", "sprint")
            .await
            .expect("clear")
    );
    let left = IssuePropertyRepo::values_for(store.pool(), &ws("ws-a"), "iss-1")
        .await
        .expect("values_for");
    assert_eq!(left.len(), 1, "clearing one key leaves the sibling");
    assert_eq!(left[0].0.key, "owner");
    assert!(
        !IssuePropertyRepo::clear_value(store.pool(), &ws("ws-a"), "iss-1", "sprint")
            .await
            .expect("second clear"),
        "clearing an unset key reports false, not an error"
    );
}

#[tokio::test]
async fn a_multi_select_round_trips_through_the_issue_read_path() {
    let (_dir, store) = setup().await;
    IssuePropertyRepo::define(
        store.pool(),
        &ws("ws-a"),
        "areas",
        "Areas",
        &PropertyKind::MultiSelect,
        &opts(&["tui", "daemon", "store"]),
        0,
        1,
    )
    .await
    .expect("define areas");
    IssuePropertyRepo::set_value(
        store.pool(),
        &ws("ws-a"),
        "iss-1",
        "areas",
        &PropertyValue::List(opts(&["tui", "store"])),
    )
    .await
    .expect("set areas");

    let issue = IssueRepo::get_by_id(store.pool(), "iss-1")
        .await
        .expect("get issue")
        .expect("issue exists");
    assert_eq!(issue.properties.len(), 1);
    assert_eq!(
        issue.properties.values().next(),
        Some(&PropertyValue::List(opts(&["tui", "store"]))),
        "the typed value survives the issue read path"
    );
}
