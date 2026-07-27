//! WHO is accountable for an autopilot run (multica parity #14, migration
//! 0061).
//!
//! multica's model forks on **how dispatch was invoked, not just who created the
//! rule**: an unattended fire (cron / webhook / api) attributes to the RULE
//! OWNER — resolved from the newest rule version — while a manual "run now"
//! attributes to the DIRECT HUMAN who clicked. These tests pin that fork, plus
//! the two honesty guarantees: an unversioned rule fired unattended records
//! NOBODY rather than a fabricated actor, and a SKIPPED run is attributed
//! identically to a fired one (it is an accountable event too).

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, AutopilotId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{
    Autopilot, AutopilotRepo, ConcurrencyPolicy, ExecutionMode, NewAutopilot,
};
use ainb_hangar_store::repo::autopilot_run::{
    DispatchOutcome, RunAttribution, RunSource, dispatch_with_admission,
    dispatch_with_admission_as, fire_autopilot_tick_with_attribution,
};

const T0: i64 = 1_767_225_600_000;

fn ws1() -> WorkspaceId {
    WorkspaceId::from_str("ws-1").unwrap()
}
fn alice() -> ActorRef {
    ActorRef::new(ActorKind::Member, "user-alice").unwrap()
}
fn bob() -> ActorRef {
    ActorRef::new(ActorKind::Member, "user-bob").unwrap()
}

async fn seed_graph(store: &Store) {
    let pool = store.pool();
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-alice','alice@example.com',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-bob','bob@example.com',0)",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-alice')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

async fn open(dir: &tempfile::TempDir) -> Store {
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    store
}

/// Create an autopilot published by `actor` (or unattributed when `None`) and
/// read it back.
async fn seed_autopilot(
    store: &Store,
    name: &str,
    actor: Option<&ActorRef>,
    policy: ConcurrencyPolicy,
    max_concurrent_runs: i64,
) -> Autopilot {
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &NewAutopilot {
            workspace_id: ws1(),
            agent_id: AgentId::from_str("agent-1").unwrap(),
            name: name.to_string(),
            instructions: Some("do the thing".to_string()),
            cron_expr: "0 9 * * *".to_string(),
            max_concurrent_runs,
            execution_mode: ExecutionMode::RunOnly,
            concurrency_policy: policy,
            api_trigger_enabled: false,
        },
        actor,
    )
    .await
    .expect("create autopilot");
    AutopilotRepo::get(store.pool(), &ws1(), &id)
        .await
        .expect("get")
        .expect("present")
}

/// `(accountable_actor, attribution)` for one run row.
async fn attribution_of(store: &Store, run_id: &str) -> (Option<String>, Option<String>) {
    use sqlx::Row;
    let row = sqlx::query("SELECT accountable_actor, attribution FROM autopilot_run WHERE id = ?")
        .bind(run_id)
        .fetch_one(store.pool())
        .await
        .expect("run row present");
    (
        row.get::<Option<String>, _>("accountable_actor"),
        row.get::<Option<String>, _>("attribution"),
    )
}

/// D.1 — an UNATTENDED fire resolves the rule's publisher and stamps
/// `rule_owner`.
#[tokio::test]
async fn an_unattended_fire_attributes_to_the_rule_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let ap = seed_autopilot(
        &store,
        "nightly",
        Some(&alice()),
        ConcurrencyPolicy::Skip,
        1,
    )
    .await;

    // The scheduler path — `dispatch_with_admission` delegates with RuleOwner.
    let outcome = dispatch_with_admission(store.pool(), &FixedClock(T0), &ap, RunSource::Schedule)
        .await
        .expect("dispatch");
    let DispatchOutcome::Fired { run_id, .. } = outcome else {
        panic!("expected a fired dispatch, got {outcome:?}");
    };

    assert_eq!(
        attribution_of(&store, run_id.as_str()).await,
        (
            Some("member:user-alice".to_string()),
            Some("rule_owner".to_string())
        )
    );
}

/// D.2 — THE FORK. A named human firing "run now" is attributed `direct_human`
/// and names THEM (bob), not the rule's owner (alice).
#[tokio::test]
async fn a_manual_fire_attributes_to_the_clicking_human_not_the_rule_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let ap = seed_autopilot(
        &store,
        "nightly",
        Some(&alice()),
        ConcurrencyPolicy::Skip,
        1,
    )
    .await;

    let (run_id, _task_id) = fire_autopilot_tick_with_attribution(
        store.pool(),
        &FixedClock(T0),
        &ap,
        RunSource::Manual,
        &RunAttribution::DirectHuman(bob()),
    )
    .await
    .expect("manual fire");

    assert_eq!(
        attribution_of(&store, run_id.as_str()).await,
        (
            Some("member:user-bob".to_string()),
            Some("direct_human".to_string())
        ),
        "the human at the keyboard is accountable, not the rule's owner"
    );
}

/// D.3 — an UNVERSIONED rule fired unattended names NOBODY. An honest unknown
/// beats a fabricated actor.
#[tokio::test]
async fn an_unversioned_rule_fired_unattended_records_no_actor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    // The legacy delegate mints v1 with `published_by = NULL`.
    let ap = seed_autopilot(&store, "legacy", None, ConcurrencyPolicy::Skip, 1).await;

    let outcome = dispatch_with_admission(store.pool(), &FixedClock(T0), &ap, RunSource::Schedule)
        .await
        .expect("dispatch");
    let DispatchOutcome::Fired { run_id, .. } = outcome else {
        panic!("expected a fired dispatch, got {outcome:?}");
    };

    assert_eq!(
        attribution_of(&store, run_id.as_str()).await,
        (None, None),
        "no publisher ⇒ no accountable actor AND no attribution token"
    );
}

/// D.4 — a SKIPPED run is attributed identically. Attribution is not a
/// fire-only concern: a declined dispatch is still an accountable event.
#[tokio::test]
async fn a_skipped_run_is_attributed_identically_to_a_fired_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let ap = seed_autopilot(
        &store,
        "nightly",
        Some(&alice()),
        ConcurrencyPolicy::Skip,
        1,
    )
    .await;

    // Fill the single in-flight slot.
    let first = dispatch_with_admission(store.pool(), &FixedClock(T0), &ap, RunSource::Schedule)
        .await
        .expect("first dispatch");
    assert!(matches!(first, DispatchOutcome::Fired { .. }));

    // Unattended skip → rule_owner.
    let skipped = dispatch_with_admission(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ap,
        RunSource::Schedule,
    )
    .await
    .expect("second dispatch");
    let DispatchOutcome::Skipped { run_id, .. } = skipped else {
        panic!("expected a skipped dispatch, got {skipped:?}");
    };
    assert_eq!(
        attribution_of(&store, run_id.as_str()).await,
        (
            Some("member:user-alice".to_string()),
            Some("rule_owner".to_string())
        ),
        "a skipped run carries the same accountability as a fired one"
    );

    // Manual skip → direct_human (bob), still not the rule owner.
    let skipped_by_bob = dispatch_with_admission_as(
        store.pool(),
        &FixedClock(T0 + 2_000),
        &ap,
        RunSource::Manual,
        &RunAttribution::DirectHuman(bob()),
    )
    .await
    .expect("third dispatch");
    let DispatchOutcome::Skipped { run_id, .. } = skipped_by_bob else {
        panic!("expected a skipped dispatch, got {skipped_by_bob:?}");
    };
    assert_eq!(
        attribution_of(&store, run_id.as_str()).await,
        (
            Some("member:user-bob".to_string()),
            Some("direct_human".to_string())
        )
    );
}

/// The attribution a run records is the one the ledger held AT FIRE TIME —
/// re-publishing later re-points future runs without rewriting history.
#[tokio::test]
async fn a_later_republish_does_not_rewrite_an_earlier_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = open(&dir).await;
    let ap = seed_autopilot(
        &store,
        "nightly",
        Some(&alice()),
        ConcurrencyPolicy::Queue,
        5,
    )
    .await;

    let first = dispatch_with_admission(store.pool(), &FixedClock(T0), &ap, RunSource::Schedule)
        .await
        .expect("first dispatch");
    let DispatchOutcome::Fired {
        run_id: first_run, ..
    } = first
    else {
        panic!("expected a fired dispatch");
    };

    // Bob re-publishes the rule (a substantive edit).
    let id = AutopilotId::from_str(ap.id.clone()).unwrap();
    AutopilotRepo::update_as(
        store.pool(),
        &FixedClock(T0 + 1_000),
        &ws1(),
        &id,
        &ainb_hangar_store::repo::autopilot::AutopilotEdit {
            instructions: Some(Some("v2".to_string())),
            ..Default::default()
        },
        Some(&bob()),
    )
    .await
    .expect("republish");

    let reloaded = AutopilotRepo::get(store.pool(), &ws1(), &id)
        .await
        .expect("get")
        .expect("present");
    let second = dispatch_with_admission(
        store.pool(),
        &FixedClock(T0 + 2_000),
        &reloaded,
        RunSource::Schedule,
    )
    .await
    .expect("second dispatch");
    let DispatchOutcome::Fired {
        run_id: second_run, ..
    } = second
    else {
        panic!("expected a fired dispatch");
    };

    assert_eq!(
        attribution_of(&store, first_run.as_str()).await.0,
        Some("member:user-alice".to_string()),
        "the earlier run keeps ITS accountable human — the ledger is append-only"
    );
    assert_eq!(
        attribution_of(&store, second_run.as_str()).await.0,
        Some("member:user-bob".to_string()),
        "the later run follows the NEWEST rule version"
    );
}
