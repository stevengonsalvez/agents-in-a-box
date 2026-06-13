//! e38.34 — `refresh_pr_status` auto-moves a merged PR's issue to Done.
//!
//! The done-stamp keys on the PR actually merging, not on a `bd`-side close sync.
//! These store-backed tests drive the refresh service with a **fake**
//! [`FakePrStatusProvider`] (never real `gh` / network) and assert the side effect
//! on the persisted issue row:
//!
//! - a `merged` PR moves its backing issue from `open` to `done` AND returns the
//!   re-read row (the event the handler pushes);
//! - an `open` / un-merged PR leaves the issue untouched and returns no row;
//! - an already-`done` issue is a no-op (idempotent — no spurious re-stamp/event);
//! - an issue with no bound PR answers an all-`Unknown` status + no transition,
//!   never spawning `gh`.
//!
//! The negative mutations (a merged PR that does NOT move the issue, or a refresh
//! that ignores the merge state) would flip the `state == "done"` assertions red.

use ainb_hangar_daemon::pr_status::FakePrStatusProvider;
use ainb_hangar_daemon::rpc::snapshots::refresh_pr_status;
use ainb_hangar_daemon::seed::{WS_ID, seed_p4_fixture};
use ainb_hangar_proto::pr_status::{CiRollup, MergeState, Mergeable, PrStatus};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::issue::IssueRepo;

/// Stamp `task-1`'s row to a completed task carrying `result.pr_url` so the issue
/// has a PR to refresh (mirrors the P9.2 capture fixture).
async fn complete_task_with_pr(pool: &sqlx::SqlitePool, task_id: &str, pr_url: &str) {
    let result = serde_json::json!({ "content": "done", "exit_code": 0, "pr_url": pr_url });
    sqlx::query(
        "UPDATE agent_task_queue SET status = 'done', result = ?, finished_at = ? WHERE id = ?",
    )
    .bind(result.to_string())
    .bind(1_700_000_100_000i64)
    .bind(task_id)
    .execute(pool)
    .await
    .expect("complete task with pr");
}

/// The current persisted lifecycle state of an issue.
async fn issue_state(pool: &sqlx::SqlitePool, issue_id: &str) -> String {
    IssueRepo::get_by_id(pool, issue_id)
        .await
        .expect("get issue")
        .expect("issue present")
        .state
}

/// A merged PR moves its backing issue from `open` to `done` and returns the
/// re-read row the handler announces as `IssueUpdated`.
#[tokio::test]
async fn merged_pr_moves_issue_to_done() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    complete_task_with_pr(store.pool(), "task-1", "https://example.com/pr/1").await;

    // Pre-condition: the seeded issue starts open, not done.
    assert_eq!(issue_state(store.pool(), "issue-1").await, "open");

    let provider = FakePrStatusProvider::new(PrStatus {
        ci: CiRollup::Pass,
        mergeable: Mergeable::Mergeable,
        state: MergeState::Merged,
    });
    let (status, row) = refresh_pr_status(store.pool(), WS_ID, "issue-1", &provider)
        .await
        .expect("refresh");

    // The fetched status is surfaced and the issue moved to Done.
    assert_eq!(status.state, MergeState::Merged);
    assert_eq!(
        issue_state(store.pool(), "issue-1").await,
        "done",
        "a merged PR must move the issue to Done"
    );
    // The transition returns the re-read row (the IssueUpdated payload) in `done`.
    let row = row.expect("a transition returns the re-read row");
    assert_eq!(row.id.as_str(), "issue-1");
    assert_eq!(row.state, "done");
}

/// An open (un-merged) PR leaves the issue exactly where it is — no transition,
/// no row to announce.
#[tokio::test]
async fn open_pr_leaves_issue_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    complete_task_with_pr(store.pool(), "task-1", "https://example.com/pr/1").await;

    let provider = FakePrStatusProvider::new(PrStatus {
        ci: CiRollup::Fail,
        mergeable: Mergeable::Conflicting,
        state: MergeState::Open,
    });
    let (status, row) = refresh_pr_status(store.pool(), WS_ID, "issue-1", &provider)
        .await
        .expect("refresh");

    assert_eq!(status.state, MergeState::Open);
    assert!(row.is_none(), "an open PR performs no transition");
    assert_eq!(
        issue_state(store.pool(), "issue-1").await,
        "open",
        "an open PR must not move the issue"
    );
}

/// An already-`done` issue is a no-op: a re-refresh of a merged PR returns no row
/// (no spurious re-stamp / duplicate `IssueUpdated`).
#[tokio::test]
async fn already_done_issue_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    complete_task_with_pr(store.pool(), "task-1", "https://example.com/pr/1").await;
    IssueRepo::update_state(store.pool(), "issue-1", "done").await.unwrap();

    let provider = FakePrStatusProvider::new(PrStatus {
        ci: CiRollup::Pass,
        mergeable: Mergeable::Mergeable,
        state: MergeState::Merged,
    });
    let (status, row) = refresh_pr_status(store.pool(), WS_ID, "issue-1", &provider)
        .await
        .expect("refresh");

    assert_eq!(status.state, MergeState::Merged);
    assert!(
        row.is_none(),
        "an already-done issue performs no re-transition"
    );
    assert_eq!(issue_state(store.pool(), "issue-1").await, "done");
}

/// An issue with no bound PR answers an all-`Unknown` status + no transition (a
/// read), and the fake provider is never even consulted in a way that would move
/// the issue.
#[tokio::test]
async fn no_bound_pr_yields_unknown_and_no_transition() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();
    // issue-2 has no PR-bearing task in the fixture.

    // Even a fake that *would* report merged must not move an issue with no PR,
    // because the bound-PR resolve short-circuits before the provider is queried.
    let provider = FakePrStatusProvider::new(PrStatus {
        ci: CiRollup::Pass,
        mergeable: Mergeable::Mergeable,
        state: MergeState::Merged,
    });
    let (status, row) = refresh_pr_status(store.pool(), WS_ID, "issue-2", &provider)
        .await
        .expect("refresh");

    assert_eq!(status, PrStatus::default(), "no PR → all-unknown status");
    assert!(row.is_none(), "no PR → no transition");
    assert_eq!(
        issue_state(store.pool(), "issue-2").await,
        "open",
        "an issue with no PR is never moved"
    );
}
