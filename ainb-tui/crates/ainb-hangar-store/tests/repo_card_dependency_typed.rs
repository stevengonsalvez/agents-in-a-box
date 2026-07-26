//! Typed card links (multica parity #20, migration 0055).
//!
//! multica's `issue_dependency` carries `type IN ('blocks','blocked_by','related')`
//! and nothing else — no gating, no cycle guard, no auto-run. These tests pin the
//! semantics hangar layers on top of that kind dimension:
//!
//!   - a `blocked_by` link GATES a run until the blocker finishes (the acceptance,
//!     re-proven through the NEW typed write path);
//!   - a `blocks` link is the SAME relation authored from the other end and is
//!     normalised into a swapped `blocked_by` row — never stored as `'blocks'`;
//!   - a `related` link is symmetric, NEVER gates, and is exempt from the cycle
//!     guard.

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::card_dependency::{CardDependencyError, CardDependencyRepo, LinkKind};
use sqlx::SqlitePool;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::from_str(id.to_string()).unwrap()
}

async fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    (dir, store)
}

async fn seed_ws(pool: &SqlitePool, id: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
}

async fn seed_issue(pool: &SqlitePool, ws: &str, id: &str) {
    sqlx::query(
        "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
         VALUES (?, ?, ?, 'member', 'm1', 0)",
    )
    .bind(id)
    .bind(ws)
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

/// Seed the FK chain then one task on `issue_id` with `status`, so the
/// blocker-finished check has something to read.
async fn seed_task(pool: &SqlitePool, ws: &str, issue_id: &str, task_id: &str, status: &str) {
    sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES ('u','u@e.com',0)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT OR IGNORE INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) VALUES ('rt', ?, 'd','claude','local','online')")
        .bind(ws).execute(pool).await.unwrap();
    sqlx::query("INSERT OR IGNORE INTO agent (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) VALUES ('ag', ?, 'A','rt','x','workspace','u')")
        .bind(ws).execute(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES (?, ?, 'rt', 'ag', ?, ?, 0)",
    )
    .bind(task_id).bind(ws).bind(issue_id).bind(status)
    .execute(pool).await.unwrap();
}

async fn setup(ids: &[&str]) -> (tempfile::TempDir, Store) {
    let (dir, store) = open().await;
    seed_ws(store.pool(), "ws-a").await;
    for id in ids {
        seed_issue(store.pool(), "ws-a", id).await;
    }
    (dir, store)
}

/// The at-rest `link_type` values, deduplicated.
async fn stored_kinds(pool: &SqlitePool) -> Vec<String> {
    let mut v: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT link_type FROM card_dependency ORDER BY 1")
            .fetch_all(pool)
            .await
            .unwrap();
    v.sort();
    v
}

/// A `related` link NEVER gates a run — in either direction.
#[tokio::test]
async fn related_link_never_gates_a_run() {
    let (_d, store) = setup(&["a", "b"]).await;
    let pool = store.pool();

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "b", LinkKind::Related, 1)
        .await
        .unwrap();

    assert!(
        CardDependencyRepo::unfinished_blockers_of(pool, "a").await.unwrap().is_empty(),
        "a related link must not gate the authoring card"
    );
    assert!(
        CardDependencyRepo::unfinished_blockers_of(pool, "b").await.unwrap().is_empty(),
        "a related link must not gate the other card either"
    );
    assert!(
        CardDependencyRepo::blockers_of(pool, "a").await.unwrap().is_empty(),
        "a related link is invisible to the blocker adjacency"
    );
    assert!(
        CardDependencyRepo::dependents_of(pool, "b").await.unwrap().is_empty(),
        "a related link is invisible to the finalize-seam reverse lookup"
    );
}

/// Authoring `A blocks B` stores the SAME row as `B blocked_by A`.
#[tokio::test]
async fn blocks_is_normalised_into_the_reverse_blocked_by_edge() {
    let (_d, store) = setup(&["a", "b"]).await;
    let pool = store.pool();

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "b", LinkKind::Blocks, 1)
        .await
        .unwrap();

    assert_eq!(
        CardDependencyRepo::unfinished_blockers_of(pool, "b").await.unwrap(),
        vec!["a"],
        "`a blocks b` gates b, not a"
    );
    assert!(CardDependencyRepo::unfinished_blockers_of(pool, "a").await.unwrap().is_empty());
    assert_eq!(
        CardDependencyRepo::blocks_of(pool, "a").await.unwrap(),
        vec!["b"],
        "the reverse read direction renders a's `blocks` link"
    );
    assert_eq!(stored_kinds(pool).await, vec!["blocked_by"]);
}

/// The item's acceptance at the store layer, authored with an EXPLICIT
/// `LinkKind::BlockedBy`: the dependent is gated until the blocker's latest
/// generation drains with a `done`.
#[tokio::test]
async fn blocked_by_link_gates_until_the_blocker_finishes() {
    let (_d, store) = setup(&["blk", "dep"]).await;
    let pool = store.pool();

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "dep", "blk", LinkKind::BlockedBy, 1)
        .await
        .unwrap();
    assert_eq!(
        CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap(),
        vec!["blk"],
        "a never-ran blocker gates the dependent"
    );

    seed_task(pool, "ws-a", "blk", "t1", "running").await;
    assert_eq!(
        CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap(),
        vec!["blk"],
        "an in-flight blocker still gates"
    );

    sqlx::query("UPDATE agent_task_queue SET status = 'done' WHERE id = 't1'")
        .execute(pool)
        .await
        .unwrap();
    assert!(
        CardDependencyRepo::unfinished_blockers_of(pool, "dep")
            .await
            .unwrap()
            .is_empty(),
        "the blocker drained with a done — the dependent is runnable"
    );
}

/// `related` is symmetric: authoring it both ways yields ONE row, readable from
/// either end.
#[tokio::test]
async fn related_is_symmetric_and_idempotent() {
    let (_d, store) = setup(&["a", "b"]).await;
    let pool = store.pool();

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "b", LinkKind::Related, 1)
        .await
        .unwrap();
    CardDependencyRepo::add_link(pool, &ws("ws-a"), "b", "a", LinkKind::Related, 2)
        .await
        .unwrap();

    assert_eq!(
        CardDependencyRepo::links_of_workspace(pool, &ws("ws-a")).await.unwrap().len(),
        1,
        "the mirrored re-add did not create a second row"
    );
    assert_eq!(
        CardDependencyRepo::related_of(pool, "a").await.unwrap(),
        vec!["b"]
    );
    assert_eq!(
        CardDependencyRepo::related_of(pool, "b").await.unwrap(),
        vec!["a"]
    );

    // And it can be removed from either end.
    CardDependencyRepo::remove_link(pool, &ws("ws-a"), "b", "a", LinkKind::Related)
        .await
        .unwrap();
    assert!(CardDependencyRepo::related_of(pool, "a").await.unwrap().is_empty());
}

/// A `related` pair in both orientations is fine (it gates nothing so it cannot
/// deadlock); the same pair as `blocked_by` is still a rejected cycle.
#[tokio::test]
async fn related_links_are_exempt_from_the_cycle_guard() {
    let (_d, store) = setup(&["a", "b", "c", "d"]).await;
    let pool = store.pool();

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "b", LinkKind::Related, 1)
        .await
        .unwrap();
    CardDependencyRepo::add_link(pool, &ws("ws-a"), "b", "a", LinkKind::Related, 2)
        .await
        .expect("a symmetric related pair is not a cycle");

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "c", "d", LinkKind::BlockedBy, 3)
        .await
        .unwrap();
    let err = CardDependencyRepo::add_link(pool, &ws("ws-a"), "d", "c", LinkKind::BlockedBy, 4)
        .await
        .unwrap_err();
    assert!(matches!(err, CardDependencyError::Cycle), "got {err:?}");
}

/// A self-link is rejected for EVERY kind, not just the gating one.
#[tokio::test]
async fn a_self_link_is_rejected_for_every_kind() {
    let (_d, store) = setup(&["a"]).await;
    let pool = store.pool();
    for kind in [LinkKind::Blocks, LinkKind::BlockedBy, LinkKind::Related] {
        let err = CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "a", kind, 1)
            .await
            .unwrap_err();
        assert!(
            matches!(err, CardDependencyError::SelfDependency),
            "{kind:?} self-link: got {err:?}"
        );
    }
}

/// The at-rest domain invariant: authoring all three kinds never persists a
/// `'blocks'` row.
#[tokio::test]
async fn no_blocks_row_is_ever_persisted() {
    let (_d, store) = setup(&["a", "b", "c", "d"]).await;
    let pool = store.pool();

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "b", LinkKind::Blocks, 1)
        .await
        .unwrap();
    CardDependencyRepo::add_link(pool, &ws("ws-a"), "c", "d", LinkKind::BlockedBy, 2)
        .await
        .unwrap();
    CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "c", LinkKind::Related, 3)
        .await
        .unwrap();

    let kinds = stored_kinds(pool).await;
    assert_eq!(
        kinds,
        vec!["blocked_by", "related"],
        "the at-rest domain is {{blocked_by, related}} — `blocks` is a read direction"
    );
}

/// Every pre-0055 row IS a blocked_by edge: the column default backfills it, it
/// still gates, and `related_of` ignores it.
#[tokio::test]
async fn pre_0055_rows_read_back_as_blocked_by_and_still_gate() {
    let (_d, store) = setup(&["blk", "dep"]).await;
    let pool = store.pool();

    // Exactly the INSERT shape a pre-0055 hangar wrote — no `link_type` column.
    sqlx::query(
        "INSERT INTO card_dependency (workspace_id, dependent_issue_id, blocker_issue_id, created_at) \
         VALUES ('ws-a', 'dep', 'blk', 7)",
    )
    .execute(pool)
    .await
    .unwrap();

    assert_eq!(stored_kinds(pool).await, vec!["blocked_by"]);
    assert_eq!(
        CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap(),
        vec!["blk"],
        "a legacy row still gates"
    );
    assert!(
        CardDependencyRepo::related_of(pool, "dep").await.unwrap().is_empty(),
        "a legacy row is not a related link"
    );
}

/// The composite PK holds ONE relation per ordered pair, so re-adding with a new
/// kind retypes the row (and stops it gating).
#[tokio::test]
async fn re_adding_a_pair_with_a_new_kind_replaces_the_kind() {
    let (_d, store) = setup(&["a", "b"]).await;
    let pool = store.pool();

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "b", LinkKind::BlockedBy, 1)
        .await
        .unwrap();
    assert_eq!(
        CardDependencyRepo::unfinished_blockers_of(pool, "a").await.unwrap(),
        vec!["b"]
    );

    CardDependencyRepo::add_link(pool, &ws("ws-a"), "a", "b", LinkKind::Related, 2)
        .await
        .unwrap();

    let links = CardDependencyRepo::links_of_workspace(pool, &ws("ws-a")).await.unwrap();
    assert_eq!(links.len(), 1, "still one row for the pair");
    assert_eq!(links[0].kind, LinkKind::Related);
    assert!(
        CardDependencyRepo::unfinished_blockers_of(pool, "a").await.unwrap().is_empty(),
        "retyped to related — no longer gating"
    );
}

/// `LinkKind::parse` accepts every token the wire / CLI can send.
#[test]
fn link_kind_parses_wire_and_cli_tokens() {
    assert_eq!(LinkKind::parse("blocks"), Some(LinkKind::Blocks));
    assert_eq!(LinkKind::parse("blocked_by"), Some(LinkKind::BlockedBy));
    assert_eq!(LinkKind::parse("blocked-by"), Some(LinkKind::BlockedBy));
    assert_eq!(LinkKind::parse("RELATED"), Some(LinkKind::Related));
    assert_eq!(LinkKind::parse("nonsense"), None);
    assert_eq!(LinkKind::default(), LinkKind::BlockedBy);
    assert!(!LinkKind::Related.is_gating());
    assert!(LinkKind::Blocks.is_gating() && LinkKind::BlockedBy.is_gating());
}
