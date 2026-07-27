//! The autopilot SUBSCRIBER FAN-OUT onto spawned issues (multica parity #27,
//! migration 0064).
//!
//! Before this item, an autopilot-spawned issue had an EMPTY `issue_subscriber`
//! set — not even its agent creator — because the `create_issue` fire path
//! writes the issue with a raw in-transaction `INSERT` that bypasses
//! `IssueRepo::insert`'s `auto_subscribe_on_create`. So there was nobody to
//! notify about a recurring automation's occurrences.
//!
//! These tests pin the behaviour that matters, not the wiring: after a tick,
//! the SPAWNED issue carries the agent creator plus the rule's standing
//! subscriber list, each tick gets its OWN full set, and a `run_only` rule
//! spawns no issue and therefore no subscriber rows.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, AutopilotId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{
    Autopilot, AutopilotRepo, ConcurrencyPolicy, ExecutionMode, NewAutopilot,
};
use ainb_hangar_store::repo::autopilot_access::AutopilotSubscriberRepo;
use ainb_hangar_store::repo::autopilot_run::{
    RunAttribution, RunSource, fire_autopilot_tick_with_attribution,
};
use ainb_hangar_store::repo::issue_subscriber::{IssueSubscriberRepo, SubscribeReason};

const T0: i64 = 1_767_225_600_000;

fn ws1() -> WorkspaceId {
    WorkspaceId::from_str("ws-1").unwrap()
}
fn member(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Member, id).unwrap()
}
fn agent1() -> ActorRef {
    ActorRef::new(ActorKind::Agent, "agent-1").unwrap()
}

async fn seed_graph(store: &Store) {
    let pool = store.pool();
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-a','a@x.io',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-b','b@x.io',0)",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-a')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

fn new_autopilot(name: &str, mode: ExecutionMode) -> NewAutopilot {
    NewAutopilot {
        workspace_id: ws1(),
        agent_id: AgentId::from_str("agent-1").unwrap(),
        name: name.to_string(),
        instructions: Some("nightly sweep".to_string()),
        cron_expr: "0 3 * * *".to_string(),
        max_concurrent_runs: 10,
        execution_mode: mode,
        concurrency_policy: ConcurrencyPolicy::Queue,
        api_trigger_enabled: false,
    }
}

async fn load(store: &Store, id: &AutopilotId) -> Autopilot {
    AutopilotRepo::get(store.pool(), &ws1(), id)
        .await
        .expect("get")
        .expect("present")
}

/// Every issue this autopilot spawned, oldest first.
async fn spawned_issues(store: &Store, autopilot_id: &AutopilotId) -> Vec<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT id FROM issue WHERE origin_type = 'autopilot' AND origin_id = ? \
         ORDER BY created_at, id",
    )
    .bind(autopilot_id.as_str())
    .fetch_all(store.pool())
    .await
    .expect("spawned issues")
}

/// `(actor, reason)` pairs on one issue, in the repo's stable order.
async fn subscribers(store: &Store, issue_id: &str) -> Vec<(String, String)> {
    IssueSubscriberRepo::list(store.pool(), issue_id)
        .await
        .expect("list subscribers")
        .into_iter()
        .map(|s| (s.actor.to_string(), s.reason_raw))
        .collect()
}

#[tokio::test]
async fn a_spawned_issue_carries_the_agent_creator_and_the_rules_subscribers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_autopilot("nightly", ExecutionMode::CreateIssue),
    )
    .await
    .expect("create");
    for (actor, at) in [(member("user-a"), T0), (member("user-b"), T0 + 1)] {
        AutopilotSubscriberRepo::add(store.pool(), ws1().as_str(), id.as_str(), &actor, None, at)
            .await
            .expect("subscribe");
    }

    let autopilot = load(&store, &id).await;
    fire_autopilot_tick_with_attribution(
        store.pool(),
        &clock,
        &autopilot,
        RunSource::Schedule,
        &RunAttribution::RuleOwner,
    )
    .await
    .expect("first tick");

    let issues = spawned_issues(&store, &id).await;
    assert_eq!(issues.len(), 1, "one tick spawned one issue");
    let mut got = subscribers(&store, &issues[0]).await;
    got.sort();
    let mut expected = vec![
        (
            agent1().to_string(),
            SubscribeReason::Creator.as_db_str().to_string(),
        ),
        (
            member("user-a").to_string(),
            SubscribeReason::Autopilot.as_db_str().to_string(),
        ),
        (
            member("user-b").to_string(),
            SubscribeReason::Autopilot.as_db_str().to_string(),
        ),
    ];
    expected.sort();
    assert_eq!(got, expected, "creator + the whole standing list");

    // A SECOND tick gets its OWN full set — following the rule means being
    // notified per occurrence, not once ever.
    let autopilot = load(&store, &id).await;
    fire_autopilot_tick_with_attribution(
        store.pool(),
        &clock,
        &autopilot,
        RunSource::Schedule,
        &RunAttribution::RuleOwner,
    )
    .await
    .expect("second tick");

    let issues = spawned_issues(&store, &id).await;
    assert_eq!(issues.len(), 2);
    for issue in &issues {
        assert_eq!(
            subscribers(&store, issue).await.len(),
            3,
            "issue {issue} has its own full subscriber set"
        );
    }
}

/// An agent that is BOTH the creator and a standing subscriber keeps `creator`
/// — first-reason-wins, which is why the creator row is written first.
#[tokio::test]
async fn the_creating_agent_keeps_the_creator_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_autopilot("nightly", ExecutionMode::CreateIssue),
    )
    .await
    .expect("create");
    AutopilotSubscriberRepo::add(
        store.pool(),
        ws1().as_str(),
        id.as_str(),
        &agent1(),
        None,
        T0,
    )
    .await
    .expect("subscribe the agent to its own rule");

    let autopilot = load(&store, &id).await;
    fire_autopilot_tick_with_attribution(
        store.pool(),
        &clock,
        &autopilot,
        RunSource::Schedule,
        &RunAttribution::RuleOwner,
    )
    .await
    .expect("tick");

    let issues = spawned_issues(&store, &id).await;
    assert_eq!(
        subscribers(&store, &issues[0]).await,
        vec![(
            agent1().to_string(),
            SubscribeReason::Creator.as_db_str().to_string()
        )],
        "one row, and its reason is creator not autopilot"
    );
}

#[tokio::test]
async fn a_run_only_rule_spawns_no_issue_and_no_subscriber_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let clock = FixedClock(T0);

    let id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &new_autopilot("bg", ExecutionMode::RunOnly),
    )
    .await
    .expect("create");
    AutopilotSubscriberRepo::add(
        store.pool(),
        ws1().as_str(),
        id.as_str(),
        &member("user-a"),
        None,
        T0,
    )
    .await
    .expect("subscribe");

    let autopilot = load(&store, &id).await;
    fire_autopilot_tick_with_attribution(
        store.pool(),
        &clock,
        &autopilot,
        RunSource::Schedule,
        &RunAttribution::RuleOwner,
    )
    .await
    .expect("tick");

    assert!(
        spawned_issues(&store, &id).await.is_empty(),
        "run_only spawns no issue"
    );
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue_subscriber")
        .fetch_one(store.pool())
        .await
        .expect("count");
    assert_eq!(total, 0, "and therefore no subscriber rows anywhere");
}
