//! The autopilot WRITE PREDICATE (multica parity #27, migration 0064).
//!
//! `can_write` is the one definition of "may this actor write this rule". It
//! mirrors the reference's `creator OR workspace-owner OR workspace-admin OR
//! explicit collaborator`, with hangar's `access_mode = 'open'` short-circuit in
//! front so no existing install is silently locked out.
//!
//! Table-driven over the whole decision surface, because the interesting cases
//! are the NEGATIVE ones: a `viewer` grant must NOT grant write, and an
//! unversioned rule must NOT fabricate an owner just to let somebody in.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, AutopilotId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::{
    AccessMode, AutopilotEdit, AutopilotRepo, ConcurrencyPolicy, ExecutionMode, NewAutopilot,
};
use ainb_hangar_store::repo::autopilot_access::{
    AllowReason, AutopilotCollaboratorRepo, CollaboratorRole, WriteDecision, can_write,
};

const T0: i64 = 1_767_225_600_000;

fn ws1() -> WorkspaceId {
    WorkspaceId::from_str("ws-1").unwrap()
}
fn member(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Member, id).unwrap()
}

/// `user-creator` is a plain workspace `member` so that "owner of the rule" and
/// "owner of the workspace" are provably DIFFERENT reasons.
async fn seed_graph(store: &Store) {
    let pool = store.pool();
    for sql in [
        "INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','alpha','Alpha',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-creator','c@x.io',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-admin','a@x.io',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-stranger','s@x.io',0)",
        "INSERT INTO user (id, email, created_at) VALUES ('user-grantee','g@x.io',0)",
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','user-creator','member')",
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','user-admin','admin')",
        "INSERT INTO member (workspace_id, user_id, role) \
         VALUES ('ws-1','user-stranger','member')",
        "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','user-grantee','member')",
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES ('rt-1','ws-1','daemon-1','claude','local')",
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('agent-1','ws-1','Agent','rt-1','workspace','user-creator')",
    ] {
        sqlx::query(sql).execute(pool).await.expect(sql);
    }
}

fn new_autopilot(name: &str) -> NewAutopilot {
    NewAutopilot {
        workspace_id: ws1(),
        agent_id: AgentId::from_str("agent-1").unwrap(),
        name: name.to_string(),
        instructions: Some("sweep".to_string()),
        cron_expr: "0 3 * * *".to_string(),
        max_concurrent_runs: 1,
        execution_mode: ExecutionMode::RunOnly,
        concurrency_policy: ConcurrencyPolicy::Skip,
        api_trigger_enabled: false,
    }
}

/// Create a rule owned (v1-published) by `user-creator` and restrict it.
async fn restricted_rule(store: &Store, name: &str) -> AutopilotId {
    let clock = FixedClock(T0);
    let id = AutopilotRepo::create_as(
        store.pool(),
        &clock,
        &new_autopilot(name),
        Some(&member("user-creator")),
    )
    .await
    .expect("create");
    AutopilotRepo::update_as(
        store.pool(),
        &clock,
        &ws1(),
        &id,
        &AutopilotEdit {
            access_mode: Some(AccessMode::Restricted),
            ..AutopilotEdit::default()
        },
        Some(&member("user-creator")),
    )
    .await
    .expect("restrict");
    id
}

#[tokio::test]
async fn open_mode_admits_any_actor_in_the_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let id = AutopilotRepo::create_as(
        store.pool(),
        &FixedClock(T0),
        &new_autopilot("nightly"),
        Some(&member("user-creator")),
    )
    .await
    .expect("create");

    // The pre-0064 behaviour, unchanged: a stranger may still write.
    assert_eq!(
        can_write(store.pool(), &ws1(), &id, &member("user-stranger"))
            .await
            .expect("predicate"),
        WriteDecision::Allowed(AllowReason::ModeOpen),
    );
}

#[tokio::test]
async fn restricted_mode_resolves_every_reason_and_denies_the_rest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let id = restricted_rule(&store, "nightly").await;

    // An `editor` grant, and a `viewer` grant that must NOT grant write.
    AutopilotCollaboratorRepo::add(
        store.pool(),
        ws1().as_str(),
        id.as_str(),
        &member("user-grantee"),
        CollaboratorRole::Editor,
        None,
        T0,
    )
    .await
    .expect("grant editor");
    AutopilotCollaboratorRepo::add(
        store.pool(),
        ws1().as_str(),
        id.as_str(),
        &member("user-stranger"),
        CollaboratorRole::Viewer,
        None,
        T0,
    )
    .await
    .expect("grant viewer");

    let cases: [(&str, WriteDecision); 4] = [
        // v1 publisher — a plain workspace member, so this can only be Owner.
        ("user-creator", WriteDecision::Allowed(AllowReason::Owner)),
        (
            "user-admin",
            WriteDecision::Allowed(AllowReason::WorkspaceAdmin),
        ),
        (
            "user-grantee",
            WriteDecision::Allowed(AllowReason::Collaborator),
        ),
        // A viewer grant is visibility, not permission.
        ("user-stranger", WriteDecision::Denied),
    ];
    for (user, expected) in cases {
        let got = can_write(store.pool(), &ws1(), &id, &member(user)).await.expect("predicate");
        assert_eq!(got, expected, "actor {user}");
    }
}

#[tokio::test]
async fn restricted_mode_denies_an_actor_with_no_grant_at_all() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let id = restricted_rule(&store, "nightly").await;

    assert_eq!(
        can_write(store.pool(), &ws1(), &id, &member("user-stranger"))
            .await
            .expect("predicate"),
        WriteDecision::Denied,
    );
    // Revoking a grant takes the write away again.
    AutopilotCollaboratorRepo::add(
        store.pool(),
        ws1().as_str(),
        id.as_str(),
        &member("user-grantee"),
        CollaboratorRole::Editor,
        None,
        T0,
    )
    .await
    .expect("grant");
    assert!(
        can_write(store.pool(), &ws1(), &id, &member("user-grantee"))
            .await
            .expect("predicate")
            .is_allowed()
    );
    AutopilotCollaboratorRepo::remove(
        store.pool(),
        ws1().as_str(),
        id.as_str(),
        &member("user-grantee"),
    )
    .await
    .expect("revoke");
    assert_eq!(
        can_write(store.pool(), &ws1(), &id, &member("user-grantee"))
            .await
            .expect("predicate"),
        WriteDecision::Denied,
    );
}

/// An unversioned (pre-0061) or unattributed rule has NO owner. The predicate
/// must report that honestly — never invent one to let somebody in.
#[tokio::test]
async fn an_unversioned_restricted_rule_has_no_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;

    // A rule as a pre-0061 install would have left it: row present, ledger empty.
    sqlx::query(
        "INSERT INTO autopilot \
         (id, workspace_id, agent_id, name, cron_expr, max_concurrent_runs, execution_mode, \
          concurrency_policy, next_tick_at, enabled, api_trigger_enabled, access_mode, created_at) \
         VALUES ('ap-legacy','ws-1','agent-1','legacy','0 3 * * *',1,'run_only','skip', \
                 999999999999,1,0,'restricted',0)",
    )
    .execute(store.pool())
    .await
    .expect("seed legacy autopilot");
    let id = AutopilotId::from_str("ap-legacy").unwrap();

    assert_eq!(
        can_write(store.pool(), &ws1(), &id, &member("user-creator"))
            .await
            .expect("predicate"),
        WriteDecision::Denied,
        "no ledger row means no owner — not 'everybody is the owner'"
    );
    // The workspace admin is still the escape hatch, so a legacy rule is never
    // permanently unmanageable.
    assert_eq!(
        can_write(store.pool(), &ws1(), &id, &member("user-admin"))
            .await
            .expect("predicate"),
        WriteDecision::Allowed(AllowReason::WorkspaceAdmin),
    );
}

#[tokio::test]
async fn a_rule_in_another_workspace_is_denied_not_leaked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','b','B',0)")
        .execute(store.pool())
        .await
        .expect("second workspace");
    let id = restricted_rule(&store, "nightly").await;

    assert_eq!(
        can_write(
            store.pool(),
            &WorkspaceId::from_str("ws-2").unwrap(),
            &id,
            &member("user-creator"),
        )
        .await
        .expect("predicate"),
        WriteDecision::Denied,
    );
}

/// Flipping `access_mode` is a SUBSTANTIVE publish: it mints a rule version.
#[tokio::test]
async fn restricting_a_rule_mints_a_rule_version() {
    use ainb_hangar_store::repo::autopilot_rule_version::AutopilotRuleVersionRepo;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let id = restricted_rule(&store, "nightly").await;

    let latest = AutopilotRuleVersionRepo::latest(store.pool(), &ws1(), &id)
        .await
        .expect("latest")
        .expect("a version exists");
    assert_eq!(
        latest.version, 2,
        "create minted v1, the restrict minted v2"
    );
    assert_eq!(latest.change_kind, "access");
    assert_eq!(latest.published_by.as_deref(), Some("member:user-creator"));
}
