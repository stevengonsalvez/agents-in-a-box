//! ACCEPTANCE: dispatch reason codes (multica parity #12).
//!
//! *"an issue with no assignee, or an offline agent, records + surfaces a reason
//! code explaining non-dispatch (sqlite + wire)."*
//!
//! Everything here is driven through the REAL RPC handlers (`rpc::dispatch`)
//! against a real sqlite, then asserted in TWO places: the persisted
//! `dispatch_attempt` row, and the surface a client actually reads (the RPC reply
//! plus `IssueRow.last_dispatch_reason` on the next `issues_list`).
//!
//! # Mutation proof
//!
//! Delete the `record_dispatch_attempt(...)` call from the `run_card` wrapper →
//! every test in this file goes RED. Neuter the runtime-status pre-flight (make
//! `non_online_runtime_status` return `None`) → `offline_runtime_records_and_surfaces_runtime_offline`
//! alone goes RED.

use ainb_hangar_daemon::events::{EventBroker, EventSink};
use ainb_hangar_daemon::health_stats::HealthStats;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::bootstrap;
use ainb_hangar_store::repo::card_parity::CardParityRepo;
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
use std::sync::Arc;
use std::time::Instant;

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/dispatch-reason.sock".into(),
        pid: 1,
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: Arc::new(HealthStats::default()),
    }
}

fn sink() -> EventSink {
    EventBroker::new().sink()
}

fn req(method: &str, params: serde_json::Value) -> RpcRequest {
    RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: method.into(),
        params,
    }
}

/// A workspace with a runtime, ready for agents. Returns `(store, workspace_id)`.
/// The `TempDir` is returned so the caller keeps the database alive.
async fn fixture() -> (tempfile::TempDir, Store, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let ws = bootstrap::ensure_default_workspace(store.pool()).await.expect("workspace");
    bootstrap::ensure_runtime(store.pool(), &bootstrap::default_runtime_id(), 1)
        .await
        .expect("runtime");
    (dir, store, ws)
}

/// A runnable card: non-empty brief (so the brief-or-link guard passes) and a
/// pinned `scratch` repo (so the F2 repo guard passes) — every refusal under test
/// is therefore the one it claims to be, not one of those two.
async fn seed_card(store: &Store, ws: &str, id: &str) -> ainb_hangar_store::repo::issue::Issue {
    IssueRepo::insert(
        store.pool(),
        &NewIssue {
            id: id.to_string(),
            workspace_id: ws.to_string(),
            title: format!("card {id}"),
            description: Some("do the work".into()),
            state: "todo".into(),
            creator: ainb_hangar_core::actor::ActorRef::new(
                ainb_hangar_core::actor::ActorKind::Member,
                "stevie",
            )
            .expect("actor"),
            created_at: 1,
            priority: 0,
            assignee: None,
            due_date: None,
            labels: Vec::new(),
            parent_issue_id: None,
            stage: None,
            acceptance_criteria: Vec::new(),
            context_refs: Vec::new(),
        },
    )
    .await
    .expect("insert issue");
    CardParityRepo::set_issue_repo_agent(store.pool(), ws, id, Some("scratch"), None)
        .await
        .expect("pin repo");
    IssueRepo::get_by_id(store.pool(), id)
        .await
        .expect("get")
        .expect("issue present")
}

async fn issue_run(store: &Store, ws: &str, issue_id: &str) -> ainb_hangar_proto::RpcResponse {
    rpc::dispatch(
        store.pool(),
        &req(
            methods::HANGAR_ISSUE_RUN,
            serde_json::json!({ "workspace_id": ws, "issue_id": issue_id, "mode": "headless" }),
        ),
        &health(),
        &sink(),
    )
    .await
}

/// Every `dispatch_attempt` row for one card, newest first, as
/// `(reason, detail, task_id, source)`.
async fn attempts(
    store: &Store,
    issue_id: &str,
) -> Vec<(String, Option<String>, Option<String>, String)> {
    sqlx::query_as(
        "SELECT reason, detail, task_id, source FROM dispatch_attempt \
         WHERE issue_id = ? ORDER BY created_at DESC, id DESC",
    )
    .bind(issue_id)
    .fetch_all(store.pool())
    .await
    .expect("select attempts")
}

async fn task_count(store: &Store, issue_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?")
        .bind(issue_id)
        .fetch_one(store.pool())
        .await
        .expect("count tasks")
}

/// The wire `IssueRow` for one card, as raw JSON, straight off `hangar/issues_list`
/// — so absence of a key is assertable, not just its value.
async fn issue_row_json(store: &Store, ws: &str, issue_id: &str) -> serde_json::Value {
    let resp = rpc::dispatch(
        store.pool(),
        &req(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": ws }),
        ),
        &health(),
        &sink(),
    )
    .await;
    assert!(resp.error.is_none(), "issues_list failed: {:?}", resp.error);
    resp.result
        .expect("result")
        .get("issues")
        .expect("issues array")
        .as_array()
        .expect("array")
        .iter()
        .find(|r| r.get("id").and_then(serde_json::Value::as_str) == Some(issue_id))
        .cloned()
        .unwrap_or_else(|| panic!("issue {issue_id} missing from the list"))
}

fn error_reason(resp: &ainb_hangar_proto::RpcResponse) -> Option<String> {
    resp.error
        .as_ref()?
        .data
        .as_ref()?
        .get("reason")?
        .as_str()
        .map(ToOwned::to_owned)
}

// ---------------------------------------------------------------------------
// (a) no agent to dispatch to
// ---------------------------------------------------------------------------

/// The headline acceptance case #1: an issue with no assignee in a workspace with
/// NO agent. Before #12 this was an RPC error string that scrolled past and
/// nothing else; now it is persisted AND on the next snapshot of the card.
#[tokio::test]
async fn no_agent_records_and_surfaces_target_unavailable() {
    let (_dir, store, ws) = fixture().await;
    // Deliberately NO agent created.
    let issue = seed_card(&store, &ws, "dr-no-agent").await;

    let resp = issue_run(&store, &ws, issue.id.as_str()).await;
    assert!(resp.error.is_some(), "a card with no agent must be refused");
    assert_eq!(
        error_reason(&resp).as_deref(),
        Some("target_unavailable"),
        "the refusal reply carries the stable code in error.data.reason"
    );

    let rows = attempts(&store, issue.id.as_str()).await;
    assert_eq!(rows.len(), 1, "exactly one attempt recorded");
    let (reason, detail, task_id, source) = &rows[0];
    assert_eq!(reason, "target_unavailable");
    assert_eq!(
        detail.as_deref(),
        Some("no agent in this workspace to run on")
    );
    assert_eq!(*task_id, None, "a refusal writes no task row");
    assert_eq!(source, "manual");
    assert_eq!(task_count(&store, issue.id.as_str()).await, 0);

    // …and the card itself now says why it is not running.
    let row = issue_row_json(&store, &ws, issue.id.as_str()).await;
    assert_eq!(
        row.get("last_dispatch_reason").and_then(|v| v.as_str()),
        Some("target_unavailable")
    );
    assert_eq!(
        row.get("last_dispatch_detail").and_then(|v| v.as_str()),
        Some("no agent in this workspace to run on")
    );
    assert!(row.get("last_dispatch_at").is_some());
}

// ---------------------------------------------------------------------------
// (b) offline runtime — the invisible-but-queued case
// ---------------------------------------------------------------------------

/// The headline acceptance case #2, and the one that was worse than silent:
/// nothing checked `agent_runtime.status` before enqueuing, so a run keyed to an
/// offline runtime sat `queued` until the 2h TTL relabelled it `timeout`, with
/// no statement of why anywhere.
///
/// hangar's divergence D1 from the reference: the run is still ENQUEUED (the row
/// exists, `task_id` is set) — only the observability is added.
#[tokio::test]
async fn offline_runtime_records_and_surfaces_runtime_offline() {
    let (_dir, store, ws) = fixture().await;
    let agent = bootstrap::create_agent(store.pool(), &ws, "bot", "claude", None)
        .await
        .expect("agent");
    sqlx::query("UPDATE agent_runtime SET status = 'offline' WHERE id = ?")
        .bind(&agent.runtime_id)
        .execute(store.pool())
        .await
        .expect("mark runtime offline");
    let issue = seed_card(&store, &ws, "dr-offline").await;

    let resp = issue_run(&store, &ws, issue.id.as_str()).await;
    assert!(
        resp.error.is_none(),
        "an offline runtime does NOT refuse in hangar (divergence D1): {:?}",
        resp.error
    );
    let result = resp.result.expect("result");
    assert_eq!(
        result.get("reason").and_then(|v| v.as_str()),
        Some("runtime_offline"),
        "the handler serializes the code the service decided"
    );

    let rows = attempts(&store, issue.id.as_str()).await;
    assert_eq!(rows.len(), 1);
    let (reason, detail, task_id, _source) = &rows[0];
    assert_eq!(reason, "runtime_offline");
    assert!(task_id.is_some(), "the task row IS written (divergence D1)");
    let detail = detail.as_deref().expect("a detail naming the runtime");
    assert!(
        detail.contains(&agent.runtime_id) && detail.contains("offline"),
        "detail must name the runtime and its status, got {detail:?}"
    );
    assert_eq!(task_count(&store, issue.id.as_str()).await, 1);

    let row = issue_row_json(&store, &ws, issue.id.as_str()).await;
    assert_eq!(
        row.get("last_dispatch_reason").and_then(|v| v.as_str()),
        Some("runtime_offline"),
        "an offline runtime is a DECLINE for surfacing even though the row exists"
    );
}

/// `unstable` (the heartbeat-gap band, not yet fully offline) records the same
/// code, with the detail naming the ACTUAL status — so the surface never claims
/// `offline` for a runtime that is merely lagging.
#[tokio::test]
async fn unstable_runtime_records_runtime_offline_naming_the_real_status() {
    let (_dir, store, ws) = fixture().await;
    let agent = bootstrap::create_agent(store.pool(), &ws, "bot", "claude", None)
        .await
        .expect("agent");
    sqlx::query("UPDATE agent_runtime SET status = 'unstable' WHERE id = ?")
        .bind(&agent.runtime_id)
        .execute(store.pool())
        .await
        .expect("mark runtime unstable");
    let issue = seed_card(&store, &ws, "dr-unstable").await;

    let resp = issue_run(&store, &ws, issue.id.as_str()).await;
    assert!(resp.error.is_none(), "{:?}", resp.error);

    let rows = attempts(&store, issue.id.as_str()).await;
    assert_eq!(rows[0].0, "runtime_offline");
    let detail = rows[0].1.as_deref().expect("detail");
    assert!(detail.contains("unstable"), "got {detail:?}");
}

// ---------------------------------------------------------------------------
// (c) blocked → deferred (divergence D2)
// ---------------------------------------------------------------------------

/// A card with an unfinished blocker records `deferred`, NOT a refusal code:
/// hangar genuinely promotes it later when the blocker finishes
/// (`board::auto_run_dependent`).
#[tokio::test]
async fn blocked_card_records_deferred() {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

    let (_dir, store, ws) = fixture().await;
    bootstrap::create_agent(store.pool(), &ws, "bot", "claude", None)
        .await
        .expect("agent");
    let blocker = seed_card(&store, &ws, "dr-blocker").await;
    let dependent = seed_card(&store, &ws, "dr-dependent").await;
    let ws_id = WorkspaceId::from_str(ws.clone()).expect("ws id");
    CardDependencyRepo::add_edge(
        store.pool(),
        &ws_id,
        dependent.id.as_str(),
        blocker.id.as_str(),
        1,
    )
    .await
    .expect("add edge");

    let resp = issue_run(&store, &ws, dependent.id.as_str()).await;
    assert!(resp.error.is_some(), "a blocked card refuses to run");
    assert_eq!(error_reason(&resp).as_deref(), Some("deferred"));

    let rows = attempts(&store, dependent.id.as_str()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "deferred");
    let detail = rows[0].1.as_deref().expect("detail");
    assert!(detail.starts_with("blocked by "), "got {detail:?}");
    assert_eq!(task_count(&store, dependent.id.as_str()).await, 0);

    let row = issue_row_json(&store, &ws, dependent.id.as_str()).await;
    assert_eq!(
        row.get("last_dispatch_reason").and_then(|v| v.as_str()),
        Some("deferred")
    );
}

// ---------------------------------------------------------------------------
// (d) second run of a live card
// ---------------------------------------------------------------------------

#[tokio::test]
async fn second_run_records_already_active_without_a_second_task() {
    let (_dir, store, ws) = fixture().await;
    bootstrap::create_agent(store.pool(), &ws, "bot", "claude", None)
        .await
        .expect("agent");
    let issue = seed_card(&store, &ws, "dr-twice").await;

    let first = issue_run(&store, &ws, issue.id.as_str()).await;
    assert!(first.error.is_none(), "{:?}", first.error);
    let second = issue_run(&store, &ws, issue.id.as_str()).await;
    assert!(second.error.is_some(), "the second run must be refused");
    assert_eq!(error_reason(&second).as_deref(), Some("already_active"));

    let rows = attempts(&store, issue.id.as_str()).await;
    assert_eq!(rows.len(), 2, "both attempts recorded");
    assert_eq!(rows[0].0, "already_active", "newest first");
    assert_eq!(rows[1].0, "queued");
    assert_eq!(
        task_count(&store, issue.id.as_str()).await,
        1,
        "still exactly one task"
    );
}

// ---------------------------------------------------------------------------
// (e) invocation gate
// ---------------------------------------------------------------------------

/// A PRIVATE agent invoked by a non-owner member: `invocation_not_allowed`, and
/// NO task row. The code is deliberately generic — it says nothing about whether
/// the agent exists — so it can never be an existence oracle.
#[tokio::test]
async fn private_agent_records_invocation_not_allowed_and_writes_no_task() {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::member::{MemberRepo, MemberRole};

    let (_dir, store, ws) = fixture().await;
    // `create_agent` yields a PRIVATE agent (default permission mode).
    bootstrap::create_agent(store.pool(), &ws, "secret-bot", "claude", None)
        .await
        .expect("agent");
    let ws_id = WorkspaceId::from_str(ws.clone()).expect("ws id");
    let bob = MemberRepo::add(store.pool(), &ws_id, "bob@example.com", MemberRole::Member)
        .await
        .expect("member");
    let issue = seed_card(&store, &ws, "dr-private").await;

    let resp = rpc::dispatch(
        store.pool(),
        &req(
            methods::HANGAR_ISSUE_RUN,
            serde_json::json!({
                "workspace_id": ws,
                "issue_id": issue.id.as_str(),
                "mode": "headless",
                "invoker_user_id": bob.user_id,
            }),
        ),
        &health(),
        &sink(),
    )
    .await;
    assert!(
        resp.error.is_some(),
        "a non-owner must not invoke a private agent"
    );
    assert_eq!(
        error_reason(&resp).as_deref(),
        Some("invocation_not_allowed")
    );

    let rows = attempts(&store, issue.id.as_str()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "invocation_not_allowed");
    assert_eq!(rows[0].2, None, "no task id on a denied attempt");
    assert_eq!(task_count(&store, issue.id.as_str()).await, 0);
}

// ---------------------------------------------------------------------------
// (f) the healthy path
// ---------------------------------------------------------------------------

/// A healthy run records `queued` WITH the task id — and the card's wire row
/// grows by ZERO keys, because the surfacing fields mean "why this is NOT
/// running". Absence is asserted on the raw JSON, so a `null` would fail.
#[tokio::test]
async fn healthy_run_records_queued_and_the_card_grows_no_keys() {
    let (_dir, store, ws) = fixture().await;
    bootstrap::create_agent(store.pool(), &ws, "bot", "claude", None)
        .await
        .expect("agent");
    let issue = seed_card(&store, &ws, "dr-healthy").await;

    let resp = issue_run(&store, &ws, issue.id.as_str()).await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    let result = resp.result.expect("result");
    assert_eq!(
        result.get("reason").and_then(|v| v.as_str()),
        Some("queued")
    );
    let task_id = result.get("task_id").and_then(|v| v.as_str()).expect("task id").to_string();

    let rows = attempts(&store, issue.id.as_str()).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "queued");
    assert_eq!(rows[0].2.as_deref(), Some(task_id.as_str()));
    assert_eq!(
        rows[0].1.as_deref(),
        Some(format!("task {task_id}").as_str())
    );

    let row = issue_row_json(&store, &ws, issue.id.as_str()).await;
    let obj = row.as_object().expect("object");
    for key in [
        "last_dispatch_reason",
        "last_dispatch_detail",
        "last_dispatch_at",
    ] {
        assert!(
            !obj.contains_key(key),
            "{key} must be ABSENT (not null) on a healthy card: {row}"
        );
    }
}

// ---------------------------------------------------------------------------
// (g) the read side
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_attempts_list_is_newest_first_limited_and_tenant_scoped() {
    let (_dir, store, ws) = fixture().await;
    bootstrap::create_agent(store.pool(), &ws, "bot", "claude", None)
        .await
        .expect("agent");
    let issue = seed_card(&store, &ws, "dr-feed").await;

    // queued, then already_active, then already_active again.
    for _ in 0..3 {
        let _ = issue_run(&store, &ws, issue.id.as_str()).await;
    }

    let list = |params: serde_json::Value| {
        let pool = store.pool();
        async move {
            let resp = rpc::dispatch(
                pool,
                &req(methods::HANGAR_DISPATCH_ATTEMPTS_LIST, params),
                &health(),
                &sink(),
            )
            .await;
            assert!(resp.error.is_none(), "list failed: {:?}", resp.error);
            resp.result
                .expect("result")
                .get("attempts")
                .expect("attempts")
                .as_array()
                .expect("array")
                .clone()
        }
    };

    let all = list(serde_json::json!({ "workspace_id": ws, "issue_id": issue.id.as_str() })).await;
    assert_eq!(all.len(), 3);
    let codes: Vec<&str> = all
        .iter()
        .map(|a| a.get("reason").and_then(serde_json::Value::as_str).expect("reason"))
        .collect();
    assert_eq!(
        codes,
        ["already_active", "already_active", "queued"],
        "newest first"
    );
    assert_eq!(
        all[0].get("source").and_then(serde_json::Value::as_str),
        Some("manual")
    );

    // `limit` is honoured, and still newest-first.
    let capped =
        list(serde_json::json!({ "workspace_id": ws, "issue_id": issue.id.as_str(), "limit": 1 }))
            .await;
    assert_eq!(capped.len(), 1);
    assert_eq!(
        capped[0].get("reason").and_then(serde_json::Value::as_str),
        Some("already_active")
    );

    // The workspace-wide feed sees them too.
    let ws_feed = list(serde_json::json!({ "workspace_id": ws })).await;
    assert_eq!(ws_feed.len(), 3);

    // A foreign workspace is rejected, never silently answered with someone
    // else's rows.
    let foreign = rpc::dispatch(
        store.pool(),
        &req(
            methods::HANGAR_DISPATCH_ATTEMPTS_LIST,
            serde_json::json!({ "workspace_id": "ws-does-not-exist" }),
        ),
        &health(),
        &sink(),
    )
    .await;
    assert!(
        foreign.error.is_some(),
        "an unknown workspace must be rejected"
    );
}
