//! A card's "last run" must be the run that actually happened last.
//!
//! `issue_card_fields` picks the newest `agent_task_queue` row with
//! `ORDER BY created_at DESC, id DESC`. `created_at` is epoch MILLISECONDS, so
//! two runs enqueued in the same millisecond tie — and the tie-break used to be
//! the task id, which is not ordered by insertion within a millisecond. The card
//! then rendered an ARBITRARY one of the two as `last_run_status`, i.e. a user
//! could see `failed` on a card whose latest run was `running`, or vice versa.
//!
//! Sibling of the `dispatch_attempt` ordering fix (agents-in-a-box-37i); the
//! hazard itself is already documented in
//! `ainb-hangar-store::service::activity` — `ulid::Ulid::new()` is not monotonic
//! within a millisecond because its low bits are random, not a counter.

use ainb_hangar_daemon::rpc::snapshots::issues_list;
use ainb_hangar_daemon::seed::{WS_ID, seed_p4_fixture};
use ainb_hangar_store::Store;
use sqlx::SqlitePool;

/// Enqueue a task for `issue-1` at an exact millisecond.
async fn enqueue(pool: &SqlitePool, id: &str, status: &str, created_at: i64) {
    sqlx::query(
        "INSERT INTO agent_task_queue \
         (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
         VALUES (?, ?, 'runtime-1', 'agent-1', 'issue-1', ?, ?)",
    )
    .bind(id)
    .bind(WS_ID)
    .bind(status)
    .bind(created_at)
    .execute(pool)
    .await
    .expect("enqueue task");
}

async fn last_run_status(pool: &SqlitePool) -> Option<String> {
    issues_list(pool, WS_ID)
        .await
        .expect("issues_list snapshot")
        .iter()
        .find(|i| i.id.as_str() == "issue-1")
        .expect("issue-1 present in snapshot")
        .last_run_status
        .clone()
}

#[tokio::test]
async fn the_last_run_in_a_millisecond_is_the_one_inserted_last() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    let pool = store.pool();

    // Both enqueued in the SAME millisecond, far enough in the future to outrank
    // whatever the fixture seeded. The ids are chosen so a lexical tie-break
    // disagrees with insertion order: `task-z` sorts highest but ran FIRST, so
    // an id-ordered read reports its `failed` instead of the true latest.
    const SAME_MS: i64 = 4_102_444_800_000; // 2100-01-01, safely newest
    enqueue(pool, "task-z-first", "failed", SAME_MS).await;
    enqueue(pool, "task-a-second", "running", SAME_MS).await;

    assert_eq!(
        last_run_status(pool).await.as_deref(),
        Some("running"),
        "the card must show the status of the run enqueued last, not the one \
         whose id happens to sort highest"
    );
}
