//! Behavioural proof of the archive AUDIT trail (migration 0052, multica gap #26).
//!
//! What an operator can now answer that they could not before: **who** archived
//! this agent / squad, and **when**. Each test drives the repo API with an
//! injected clock reading + actor and reads the answer back, rather than
//! asserting on SQL shape.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
use ainb_hangar_store::repo::squad::SquadRepo;
use sqlx::SqlitePool;

/// A fixed epoch-ms reading so assertions are exact, not "roughly now".
const T1: i64 = 1_700_000_000_000;
/// A LATER reading, to prove a re-archive re-stamps.
const T2: i64 = 1_800_000_000_000;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::from_str(id.to_string()).unwrap()
}

fn member(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Member, id).unwrap()
}

/// Seed `workspace → user → runtime → agent` for `ws_id`, returning the agent id.
async fn seed_agent(pool: &SqlitePool, ws_id: &str, agent_id: &str) -> String {
    sqlx::query("INSERT OR IGNORE INTO workspace (id, slug, name, created_at) VALUES (?,?,?,0)")
        .bind(ws_id)
        .bind(ws_id)
        .bind(ws_id)
        .execute(pool)
        .await
        .expect("workspace");
    sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES (?,?,0)")
        .bind(format!("user-{ws_id}"))
        .bind(format!("{ws_id}@x.com"))
        .execute(pool)
        .await
        .expect("user");
    sqlx::query(
        "INSERT OR IGNORE INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?,?,?,'claude','local','online')",
    )
    .bind(format!("rt-{ws_id}"))
    .bind(ws_id)
    .bind(format!("d-{ws_id}"))
    .execute(pool)
    .await
    .expect("runtime");

    let agent = Agent {
        id: agent_id.to_string(),
        workspace_id: ws_id.to_string(),
        name: agent_id.to_string(),
        runtime_id: format!("rt-{ws_id}"),
        visibility: "workspace".to_string(),
        owner_id: format!("user-{ws_id}"),
        ..Agent::default()
    };
    AgentRepo::insert(pool, &agent).await.expect("insert agent");
    agent_id.to_string()
}

/// Read the raw `(archived, archived_at, archived_by)` triple, so cross-tenant
/// assertions prove the ROW is untouched rather than trusting a return value.
async fn raw_agent_audit(pool: &SqlitePool, id: &str) -> (i64, Option<i64>, Option<String>) {
    sqlx::query_as("SELECT archived, archived_at, archived_by FROM agent WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read agent audit")
}

async fn raw_squad_audit(pool: &SqlitePool, id: &str) -> (i64, Option<i64>, Option<String>) {
    sqlx::query_as("SELECT archived, archived_at, archived_by FROM squad WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read squad audit")
}

/// Archiving an agent records WHO and WHEN; re-archiving with a different actor
/// re-stamps (last archiver wins); un-archiving clears BOTH columns.
#[tokio::test]
async fn agent_archive_records_who_and_when_and_restore_clears_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_agent(pool, "ws-1", "ag-1").await;

    // Before: an active agent carries no audit stamp.
    let before = AgentRepo::get(pool, "ag-1").await.unwrap().unwrap();
    assert!(!before.archived);
    assert_eq!(before.archived_at, None);
    assert_eq!(before.archived_by, None);

    // Archive.
    assert!(
        AgentRepo::set_archived(pool, "ws-1", "ag-1", true, Some(&member("user-1")), T1)
            .await
            .expect("archive")
    );
    let got = AgentRepo::get(pool, "ag-1").await.unwrap().unwrap();
    assert!(got.archived);
    assert_eq!(got.archived_at, Some(T1), "the archive stamped WHEN");
    assert_eq!(
        got.archived_by,
        Some(member("user-1")),
        "the archive stamped WHO, as a canonical actor-ref"
    );

    // Re-archive by a DIFFERENT actor at a later reading: last archiver wins.
    assert!(
        AgentRepo::set_archived(pool, "ws-1", "ag-1", true, Some(&member("user-2")), T2)
            .await
            .expect("re-archive is idempotent, not a not-found")
    );
    let got = AgentRepo::get(pool, "ag-1").await.unwrap().unwrap();
    assert_eq!(got.archived_at, Some(T2));
    assert_eq!(got.archived_by, Some(member("user-2")));

    // Restore clears BOTH audit columns (multica RestoreAgent parity).
    assert!(
        AgentRepo::set_archived(pool, "ws-1", "ag-1", false, Some(&member("user-2")), T2)
            .await
            .expect("unarchive")
    );
    let got = AgentRepo::get(pool, "ag-1").await.unwrap().unwrap();
    assert!(!got.archived);
    assert_eq!(
        (got.archived_at, got.archived_by),
        (None, None),
        "a restored agent carries no stale attribution"
    );
    assert_eq!(raw_agent_audit(pool, "ag-1").await, (0, None, None));
}

/// An archive with no attributable actor is still stamped with WHEN — an honest
/// partial record beats no record.
#[tokio::test]
async fn agent_archive_without_an_actor_still_records_when() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_agent(pool, "ws-1", "ag-1").await;

    AgentRepo::set_archived(pool, "ws-1", "ag-1", true, None, T1).await.expect("archive");
    let got = AgentRepo::get(pool, "ag-1").await.unwrap().unwrap();
    assert_eq!(got.archived_at, Some(T1));
    assert_eq!(got.archived_by, None, "unattributed, not fabricated");
}

/// Archiving another tenant's agent touches NO row — asserted against raw SQL,
/// not just the boolean return.
#[tokio::test]
async fn agent_archive_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_agent(pool, "ws-1", "ag-1").await;
    seed_agent(pool, "ws-2", "ag-2").await;

    let touched = AgentRepo::set_archived(pool, "ws-1", "ag-2", true, Some(&member("user-1")), T1)
        .await
        .expect("cross-tenant archive");
    assert!(!touched, "a foreign workspace must archive no row");
    assert_eq!(
        raw_agent_audit(pool, "ag-2").await,
        (0, None, None),
        "the foreign tenant's row is byte-untouched — flag AND audit columns"
    );
}

/// A corrupt `archived_by` cell degrades to `None` rather than failing the whole
/// agent read: the audit sidecar must never make an agent unreadable.
#[tokio::test]
async fn a_malformed_archiver_decodes_to_none_rather_than_failing_the_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_agent(pool, "ws-1", "ag-1").await;

    sqlx::query("UPDATE agent SET archived = 1, archived_at = ?, archived_by = 'garbage' WHERE id = 'ag-1'")
        .bind(T1)
        .execute(pool)
        .await
        .expect("write a corrupt cell");

    let got = AgentRepo::get(pool, "ag-1").await.expect("read still succeeds").unwrap();
    assert!(got.archived);
    assert_eq!(got.archived_at, Some(T1));
    assert_eq!(got.archived_by, None, "tolerant decode");
}

/// The squad half: archive records who + when, the squad leaves the active list
/// but stays in the audit list, and restore clears the stamp.
#[tokio::test]
async fn squad_archive_records_audit_and_leaves_the_active_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_agent(pool, "ws-1", "ag-1").await;
    SquadRepo::create(pool, &ws("ws-1"), "sq-1", "alpha", &member("user-1"), 0)
        .await
        .expect("create squad");

    assert_eq!(SquadRepo::list(pool, &ws("ws-1")).await.unwrap().len(), 1);

    SquadRepo::set_archived(pool, &ws("ws-1"), "sq-1", true, Some(&member("user-1")), T1)
        .await
        .expect("archive squad");

    assert!(
        SquadRepo::list(pool, &ws("ws-1")).await.unwrap().is_empty(),
        "an archived squad leaves the active list"
    );
    let all = SquadRepo::list_including_archived(pool, &ws("ws-1")).await.unwrap();
    assert_eq!(all.len(), 1, "the audit list still returns it");
    assert!(all[0].archived);
    assert_eq!(all[0].archived_at, Some(T1));
    assert_eq!(all[0].archived_by, Some(member("user-1")));

    // `get` stays UNFILTERED: an archived squad must remain resolvable so the
    // audit read and the reject-assignment guard can name it.
    let got = SquadRepo::get(pool, &ws("ws-1"), "sq-1").await.unwrap().expect("still resolvable");
    assert!(got.archived);
    assert_eq!(got.archived_by, Some(member("user-1")));
    assert!(SquadRepo::is_archived(pool, &ws("ws-1"), "sq-1").await.unwrap());

    // Re-archive by a different actor at a later reading: last archiver wins.
    SquadRepo::set_archived(pool, &ws("ws-1"), "sq-1", true, Some(&member("user-2")), T2)
        .await
        .expect("re-archive");
    assert_eq!(
        raw_squad_audit(pool, "sq-1").await,
        (1, Some(T2), Some("member:user-2".to_string()))
    );

    // Restore clears both and returns it to the active list.
    SquadRepo::set_archived(pool, &ws("ws-1"), "sq-1", false, Some(&member("user-2")), T2)
        .await
        .expect("restore");
    assert_eq!(raw_squad_audit(pool, "sq-1").await, (0, None, None));
    assert_eq!(SquadRepo::list(pool, &ws("ws-1")).await.unwrap().len(), 1);
    assert!(!SquadRepo::is_archived(pool, &ws("ws-1"), "sq-1").await.unwrap());
}

/// Archiving a squad through the WRONG workspace is a `NotFound` and writes
/// nothing — the tenant guard, proved against raw SQL.
#[tokio::test]
async fn squad_archive_is_workspace_scoped() {
    use ainb_hangar_store::repo::squad::SquadRepoError;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let pool = store.pool();
    seed_agent(pool, "ws-1", "ag-1").await;
    seed_agent(pool, "ws-2", "ag-2").await;
    SquadRepo::create(pool, &ws("ws-1"), "sq-1", "alpha", &member("user-1"), 0)
        .await
        .expect("create squad");

    let err = SquadRepo::set_archived(pool, &ws("ws-2"), "sq-1", true, Some(&member("x")), T1)
        .await
        .expect_err("a foreign tenant must not archive the squad");
    assert!(matches!(err, SquadRepoError::NotFound), "got {err:?}");
    assert_eq!(
        raw_squad_audit(pool, "sq-1").await,
        (0, None, None),
        "the row is untouched"
    );
    // A foreign / unknown id reads as not-archived (never another tenant's state).
    assert!(!SquadRepo::is_archived(pool, &ws("ws-2"), "sq-1").await.unwrap());
    assert!(!SquadRepo::is_archived(pool, &ws("ws-1"), "nope").await.unwrap());
}
