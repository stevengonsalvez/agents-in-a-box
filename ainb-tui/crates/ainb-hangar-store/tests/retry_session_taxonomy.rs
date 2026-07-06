//! e38.24 retry/resume poisoned-terminal taxonomy.
//!
//! Proves the session-inheritance contract a retry child carries, keyed on the
//! parent's terminal [`FailureReason`]. The taxonomy
//! ([`RetryService::retry_disposition`]) classifies every reason into one of
//! three dispositions, and [`RetryService::maybe_retry_failed`] honours it:
//!
//! - **`ResumeRetry`** (infra failure — `runtime_offline` / `runtime_recovery`):
//!   the child carries the parent's `session_id` so the new attempt *resumes*
//!   the same provider conversation. The runtime went away, not the model.
//! - **`FreshRetry`** (conversation-poisoning terminal — `iteration_limit` /
//!   `api_invalid_request` / `semantic_inactivity`): the child is spawned with
//!   `session_id = NULL` so the new attempt *starts fresh*. Resuming a wedged
//!   conversation would only re-fail (reference `GetLastTaskSession` exclusion).
//! - **`NoRetry`** (agent error / user cancel / timeout / unknown): no child.
//!
//! Each test uses an isolated tempdir store + a [`FixedClock`] so parallel cargo
//! threads do not share state and the child's `created_at` is deterministic.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::task::{NewTask, Task, TaskRepo};
use ainb_hangar_store::service::fail::{FailTaskService, FailureReason};
use ainb_hangar_store::service::retry::{RetryDecision, RetryDisposition, RetryService};

/// Frozen "now" used for the retry child's `created_at` assertions.
const NOW_MS: i64 = 1_700_002_000_000;

/// The provider session the parent run pinned, which a resume-retry must carry
/// forward and a fresh-retry must drop.
const PARENT_SESSION: &str = "sess-parent-abc";

/// Seed workspace + user + runtime + agent rows the task FKs require.
async fn seed_graph(store: &Store) {
    let pool = store.pool();
    sqlx::query("INSERT OR IGNORE INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-1")
        .bind("a@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("rt-1")
    .bind("ws-1")
    .bind("daemon-rt-1")
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, visibility, owner_id, max_concurrent_tasks) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-1")
    .bind("ws-1")
    .bind("Agent")
    .bind("rt-1")
    .bind("workspace")
    .bind("user-1")
    .bind(5_i64)
    .execute(pool)
    .await
    .expect("insert agent");
}

/// Enqueue one task, pin a `session_id` on it (the provider conversation the run
/// established), then fail it for `reason`. Returns the re-read failed [`Task`].
async fn seed_failed_task_with_session(store: &Store, id: &str, reason: FailureReason) -> Task {
    TaskRepo::insert(
        store.pool(),
        &NewTask {
            id: id.to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: None,
            work_dir: Some("/tmp/wd".to_string()),
            priority: 0,
            created_at: 1,
            autopilot_run_id: None,
            generation: 0,
        },
    )
    .await
    .expect("enqueue");
    // Pin the provider session_id (as a real run would on its way to terminal)
    // and move to `running` so the fail service has a legal source state.
    sqlx::query("UPDATE agent_task_queue SET session_id = ?, status = 'running' WHERE id = ?")
        .bind(PARENT_SESSION)
        .bind(id)
        .execute(store.pool())
        .await
        .expect("pin session + running");
    let clock = FixedClock(NOW_MS - 1);
    FailTaskService::fail(store.pool(), id, reason, &clock)
        .await
        .expect("fail seed task");
    let parent = TaskRepo::get_by_id(store.pool(), id).await.unwrap().unwrap();
    assert_eq!(
        parent.session_id.as_deref(),
        Some(PARENT_SESSION),
        "parent retains its session_id through fail()",
    );
    parent
}

async fn open_seeded() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    (dir, store)
}

/// (a) A POISONED terminal (`iteration_limit`) retries, but the child must start
/// FRESH: `session_id` is NOT inherited, so the new attempt does not resume the
/// wedged conversation.
#[tokio::test]
async fn poisoned_reason_retry_starts_fresh_no_session() {
    let (_dir, store) = open_seeded().await;
    let parent =
        seed_failed_task_with_session(&store, "t-poison", FailureReason::IterationLimit).await;
    let clock = FixedClock(NOW_MS);

    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-fresh", &clock)
        .await
        .expect("retry ok");
    assert_eq!(
        decision,
        RetryDecision::Spawned {
            new_task_id: "child-fresh".to_string()
        },
        "a poisoned terminal still retries (just fresh)",
    );

    let child = TaskRepo::get_by_id(store.pool(), "child-fresh")
        .await
        .unwrap()
        .expect("child row exists");
    assert_eq!(child.parent_task_id.as_deref(), Some("t-poison"));
    assert_eq!(child.attempt, parent.attempt + 1);
    assert_eq!(child.status, "queued");
    assert_eq!(
        child.session_id, None,
        "poisoned retry must NOT inherit the parent session_id (fresh conversation)",
    );
}

/// (b) An INFRA terminal (`runtime_offline`) retries and RESUMES: the child
/// carries the parent's `session_id`, so the new attempt continues the same
/// provider conversation (the runtime went away, not the model).
#[tokio::test]
async fn infra_reason_retry_resumes_with_session() {
    let (_dir, store) = open_seeded().await;
    let parent =
        seed_failed_task_with_session(&store, "t-infra", FailureReason::RuntimeOffline).await;
    let clock = FixedClock(NOW_MS);

    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-resume", &clock)
        .await
        .expect("retry ok");
    assert_eq!(
        decision,
        RetryDecision::Spawned {
            new_task_id: "child-resume".to_string()
        },
    );

    let child = TaskRepo::get_by_id(store.pool(), "child-resume")
        .await
        .unwrap()
        .expect("child row exists");
    assert_eq!(child.parent_task_id.as_deref(), Some("t-infra"));
    assert_eq!(
        child.session_id.as_deref(),
        Some(PARENT_SESSION),
        "infra retry must RESUME by carrying the parent session_id",
    );
}

/// (c) A non-retryable terminal (`agent_error`) produces NO child.
#[tokio::test]
async fn non_retryable_reason_produces_no_child() {
    let (_dir, store) = open_seeded().await;
    let parent = seed_failed_task_with_session(&store, "t-agent", FailureReason::AgentError).await;
    let clock = FixedClock(NOW_MS);

    let decision = RetryService::maybe_retry_failed(store.pool(), &parent, "child-none", &clock)
        .await
        .expect("decision ok");
    assert_eq!(decision, RetryDecision::DoNotRetry);
    assert!(
        TaskRepo::get_by_id(store.pool(), "child-none").await.unwrap().is_none(),
        "a non-retryable reason spawns no child row",
    );
}

/// The taxonomy function itself, exercised directly so the classification is
/// pinned independently of the spawn path. Mutating any arm (e.g. classifying
/// `IterationLimit` as `ResumeRetry`) breaks the fresh-retry guarantee above.
#[test]
fn retry_disposition_classifies_every_reason() {
    use FailureReason::*;
    use RetryDisposition::*;

    // Infra failures resume the session.
    assert_eq!(RetryService::retry_disposition(RuntimeOffline), ResumeRetry);
    assert_eq!(
        RetryService::retry_disposition(RuntimeRecovery),
        ResumeRetry
    );

    // Conversation-poisoning terminals retry FRESH.
    assert_eq!(RetryService::retry_disposition(IterationLimit), FreshRetry);
    assert_eq!(
        RetryService::retry_disposition(ApiInvalidRequest),
        FreshRetry
    );
    assert_eq!(
        RetryService::retry_disposition(SemanticInactivity),
        FreshRetry
    );

    // Agent / user / sweeper terminals do not retry.
    assert_eq!(RetryService::retry_disposition(AgentError), NoRetry);
    assert_eq!(RetryService::retry_disposition(UserCancel), NoRetry);
    assert_eq!(RetryService::retry_disposition(Timeout), NoRetry);
    assert_eq!(RetryService::retry_disposition(Unknown), NoRetry);
}
