//! Per-issue AGENT METADATA scratch bag (multica parity #17, migration 0066).
//!
//! The load-bearing assertion here is the ANTI-RACE rule, quoted from the
//! reference's own handler header: *"All mutations are single-key atomic.
//! `UpdateIssue` does NOT touch metadata — any whole-blob overwrite would race
//! with concurrent agent writes."*

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::properties::{
    MAX_METADATA_KEYS, MetadataValue, PropertyError, metadata_value_from_json,
};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::issue::{IssueFieldUpdate, IssueRepo};
use ainb_hangar_store::repo::issue_metadata::IssueMetadataRepo;
use ainb_hangar_store::repo::issue_property::PropertyRepoError;
use sqlx::SqlitePool;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::from_str(id).expect("non-empty workspace id")
}

async fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    for id in ["ws-a", "ws-b"] {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
            .bind(id)
            .bind(id)
            .bind(id)
            .execute(store.pool())
            .await
            .expect("seed workspace");
    }
    for (id, workspace) in [("iss-1", "ws-a"), ("iss-b", "ws-b")] {
        sqlx::query(
            "INSERT INTO issue \
             (id, workspace_id, title, state, creator_type, creator_id, created_at) \
             VALUES (?, ?, 't', 'open', 'member', 'm1', 0)",
        )
        .bind(id)
        .bind(workspace)
        .execute(store.pool())
        .await
        .expect("seed issue");
    }
    (dir, store)
}

async fn raw_bag(pool: &SqlitePool, issue_id: &str) -> String {
    sqlx::query_scalar("SELECT metadata FROM issue WHERE id = ?")
        .bind(issue_id)
        .fetch_one(pool)
        .await
        .expect("read metadata column")
}

#[tokio::test]
async fn set_get_delete_round_trips_all_three_primitives() {
    let (_dir, store) = setup().await;
    let pool = store.pool();
    for (key, value) in [
        ("pr_number", MetadataValue::Number("42".into())),
        ("pipeline_status", MetadataValue::Text("running".into())),
        ("blocked", MetadataValue::Bool(true)),
    ] {
        IssueMetadataRepo::set(pool, &ws("ws-a"), "iss-1", key, &value)
            .await
            .unwrap_or_else(|e| panic!("set {key}: {e}"));
    }

    let bag = IssueMetadataRepo::get(pool, &ws("ws-a"), "iss-1").await.expect("get");
    assert_eq!(bag.len(), 3);
    assert_eq!(
        bag.get("pr_number"),
        Some(&MetadataValue::Number("42".into())),
        "integer fidelity: 42 must not come back as 42.0"
    );
    assert!(
        raw_bag(pool, "iss-1").await.contains("\"pr_number\":42"),
        "the column stores a JSON NUMBER, not a string: {}",
        raw_bag(pool, "iss-1").await
    );
    assert_eq!(
        bag.get("pipeline_status"),
        Some(&MetadataValue::Text("running".into()))
    );
    assert_eq!(bag.get("blocked"), Some(&MetadataValue::Bool(true)));

    assert!(
        IssueMetadataRepo::delete(pool, &ws("ws-a"), "iss-1", "blocked")
            .await
            .expect("delete")
    );
    assert!(
        !IssueMetadataRepo::delete(pool, &ws("ws-a"), "iss-1", "blocked")
            .await
            .expect("second delete"),
        "deleting an absent key reports false, not an error"
    );
    let after = IssueMetadataRepo::get(pool, &ws("ws-a"), "iss-1").await.expect("get");
    assert_eq!(after.len(), 2, "delete removes exactly one key");
}

#[tokio::test]
async fn an_unrelated_issue_update_never_clobbers_metadata() {
    let (_dir, store) = setup().await;
    let pool = store.pool();
    IssueMetadataRepo::set(
        pool,
        &ws("ws-a"),
        "iss-1",
        "pr_number",
        &MetadataValue::Number("42".into()),
    )
    .await
    .expect("set pr_number");

    let updated = IssueRepo::update_fields(
        pool,
        "ws-a",
        "iss-1",
        &IssueFieldUpdate {
            title: Some("Ship #17 (v2)".into()),
            state: Some("in_progress".into()),
            ..Default::default()
        },
    )
    .await
    .expect("unrelated update");
    assert!(updated, "the update actually landed");

    let issue = IssueRepo::get_by_id(pool, "iss-1").await.expect("get").expect("exists");
    assert_eq!(issue.title, "Ship #17 (v2)");
    assert_eq!(
        issue.metadata.get("pr_number"),
        Some(&MetadataValue::Number("42".into())),
        "IssueRepo::update must NEVER touch the metadata bag"
    );
}

#[tokio::test]
async fn concurrent_single_key_writes_both_survive() {
    let (_dir, store) = setup().await;
    let pool = store.pool();
    let workspace = ws("ws-a");
    let pr_number = MetadataValue::Number("42".into());
    let status = MetadataValue::Text("running".into());
    let (a, b) = tokio::join!(
        IssueMetadataRepo::set(pool, &workspace, "iss-1", "pr_number", &pr_number),
        IssueMetadataRepo::set(pool, &workspace, "iss-1", "pipeline_status", &status),
    );
    a.expect("first write");
    b.expect("second write");

    let bag = IssueMetadataRepo::get(pool, &ws("ws-a"), "iss-1").await.expect("get");
    assert_eq!(
        bag.len(),
        2,
        "two DIFFERENT keys written concurrently must both survive, got {bag:?}"
    );
}

#[tokio::test]
async fn the_key_cap_bites_only_on_a_new_key() {
    let (_dir, store) = setup().await;
    let pool = store.pool();
    for i in 0..MAX_METADATA_KEYS {
        IssueMetadataRepo::set(
            pool,
            &ws("ws-a"),
            "iss-1",
            &format!("k{i}"),
            &MetadataValue::Number(i.to_string()),
        )
        .await
        .unwrap_or_else(|e| panic!("key {i} within the cap: {e}"));
    }
    let over = IssueMetadataRepo::set(
        pool,
        &ws("ws-a"),
        "iss-1",
        "one_too_many",
        &MetadataValue::Bool(true),
    )
    .await;
    assert!(
        matches!(
            over,
            Err(PropertyRepoError::Value(PropertyError::TooManyKeys))
        ),
        "the 51st key is rejected, got {over:?}"
    );
    // Overwriting an EXISTING key while at the cap still succeeds.
    IssueMetadataRepo::set(
        pool,
        &ws("ws-a"),
        "iss-1",
        "k0",
        &MetadataValue::Text("rewritten".into()),
    )
    .await
    .expect("an overwrite at the cap is fine");
}

#[tokio::test]
async fn the_byte_cap_bites_and_the_prior_bag_survives() {
    let (_dir, store) = setup().await;
    let pool = store.pool();
    IssueMetadataRepo::set(
        pool,
        &ws("ws-a"),
        "iss-1",
        "note",
        &MetadataValue::Text("small".into()),
    )
    .await
    .expect("small value fits");
    let before = raw_bag(pool, "iss-1").await;

    let over = IssueMetadataRepo::set(
        pool,
        &ws("ws-a"),
        "iss-1",
        "note",
        &MetadataValue::Text("x".repeat(10_000)),
    )
    .await;
    assert!(
        matches!(over, Err(PropertyRepoError::Value(PropertyError::TooLarge))),
        "got {over:?}"
    );
    assert_eq!(
        raw_bag(pool, "iss-1").await,
        before,
        "the rejected write is fully rolled back"
    );
}

#[tokio::test]
async fn bad_keys_and_non_primitive_values_are_rejected() {
    let (_dir, store) = setup().await;
    let pool = store.pool();
    for bad in ["9lives", "a b", &"a".repeat(65)] {
        let res =
            IssueMetadataRepo::set(pool, &ws("ws-a"), "iss-1", bad, &MetadataValue::Bool(true))
                .await;
        assert!(
            matches!(res, Err(PropertyRepoError::Value(PropertyError::BadKey(_)))),
            "{bad:?} must be rejected, got {res:?}"
        );
    }
    assert_eq!(raw_bag(pool, "iss-1").await, "{}");

    // The primitive-only contract lives in the value decoder, with the
    // reference's exact wording.
    assert_eq!(
        metadata_value_from_json("null"),
        Err(PropertyError::NullValue)
    );
    assert_eq!(
        metadata_value_from_json("[1,2]"),
        Err(PropertyError::NotPrimitive)
    );
    assert_eq!(
        metadata_value_from_json(r#"{"a":1}"#),
        Err(PropertyError::NotPrimitive)
    );
}

#[tokio::test]
async fn a_foreign_tenant_issue_id_is_rejected_and_writes_nothing() {
    let (_dir, store) = setup().await;
    let pool = store.pool();
    let cross = IssueMetadataRepo::set(
        pool,
        &ws("ws-a"),
        "iss-b",
        "pr_number",
        &MetadataValue::Number("42".into()),
    )
    .await;
    assert!(
        matches!(cross, Err(PropertyRepoError::IssueNotFound)),
        "got {cross:?}"
    );
    assert_eq!(raw_bag(pool, "iss-b").await, "{}");
    assert!(
        matches!(
            IssueMetadataRepo::get(pool, &ws("ws-a"), "iss-b").await,
            Err(PropertyRepoError::IssueNotFound)
        ),
        "a read is workspace-scoped too"
    );
}
