//! Behavioural coverage for the parity-#25 squad levers (migration 0053):
//! per-member `role` and per-squad `instructions`.
//!
//! Every case here is a CONTRACT the leader briefing and the RPC layer depend
//! on, not an implementation detail:
//!
//! - `set_member_role` on a non-member is a rejection, never a silent insert;
//! - both levers are tenant-guarded and write nothing across a workspace;
//! - a plain `add_member` re-add PRESERVES a role, `add_member_with_role`
//!   overwrites it (explicit role ⇒ explicit intent);
//! - `remove_member` drops the role with the row;
//! - instructions round-trip VERBATIM (embedded newlines included) and `""`
//!   clears;
//! - `member_agent_ids` (the fan-out query) is role-BLIND — D3, role never gates
//!   dispatch;
//! - a workspace delete still cascades `squad_member`.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::squad::{SquadRepo, SquadRepoError};
use sqlx::SqlitePool;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::from_str(id.to_string()).unwrap()
}

fn agent(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Agent, id).unwrap()
}

fn human(id: &str) -> ActorRef {
    ActorRef::new(ActorKind::Member, id).unwrap()
}

async fn seed_ws(pool: &SqlitePool, id: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .expect("insert workspace");
}

async fn open(dir: &tempfile::TempDir) -> Store {
    Store::open_in(dir.path()).await.expect("open store")
}

async fn member_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM squad_member")
        .fetch_one(pool)
        .await
        .expect("count memberships")
}

/// `set_member_role` on an actor that is NOT a member returns `false` and writes
/// nothing — the caller can reject rather than answering a silent success, and no
/// membership is minted as a side effect.
#[tokio::test]
async fn set_member_role_on_a_non_member_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir).await;
    let pool = store.pool();
    seed_ws(pool, "ws-a").await;
    SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1).await.unwrap();
    SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
    let before = member_count(pool).await;

    let updated = SquadRepo::set_member_role(pool, &ws("ws-a"), "s1", &agent("ghost"), "anything")
        .await
        .expect("tenant guard passes, the row simply does not match");
    assert!(!updated, "a non-member must report no update");
    assert_eq!(
        member_count(pool).await,
        before,
        "no membership may be inserted as a side effect"
    );
}

/// `set_member_role` actually WRITES: it sets a role on an existing membership,
/// replaces it, and clears it with `""` — round-tripping through BOTH read paths.
/// (The neuter-the-UPDATE mutation test lands here.)
#[tokio::test]
async fn set_member_role_persists_replaces_and_clears() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir).await;
    let pool = store.pool();
    seed_ws(pool, "ws-a").await;
    SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1).await.unwrap();
    SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
    SquadRepo::add_member(pool, &ws("ws-a"), "s1", &human("u-1")).await.unwrap();

    // Set.
    assert!(
        SquadRepo::set_member_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "owns the migrations")
            .await
            .unwrap(),
        "an existing membership reports an update"
    );
    for squad in [
        SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap(),
        SquadRepo::list(pool, &ws("ws-a")).await.unwrap().remove(0),
    ] {
        let roled = squad.members.iter().find(|m| m.actor == agent("a-1")).unwrap();
        assert_eq!(roled.role, "owns the migrations", "the role is stored");
        let other = squad.members.iter().find(|m| m.actor == human("u-1")).unwrap();
        assert_eq!(other.role, "", "only the named membership is touched");
    }

    // Replace, with surrounding whitespace trimmed.
    SquadRepo::set_member_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "  owns the CLI  ")
        .await
        .unwrap();
    let s = SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap();
    assert_eq!(
        s.members.iter().find(|m| m.actor == agent("a-1")).unwrap().role,
        "owns the CLI"
    );

    // Clear.
    SquadRepo::set_member_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "").await.unwrap();
    let s = SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap();
    assert_eq!(
        s.members.iter().find(|m| m.actor == agent("a-1")).unwrap().role,
        "",
        "an empty role clears the label"
    );
}

/// Both levers are tenant-guarded: a squad id from another workspace is a
/// `NotFound` and the real tenant's row is provably untouched.
#[tokio::test]
async fn role_and_instructions_are_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir).await;
    let pool = store.pool();
    seed_ws(pool, "ws-a").await;
    seed_ws(pool, "ws-b").await;
    SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1).await.unwrap();
    SquadRepo::add_member_with_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "keeper")
        .await
        .unwrap();
    SquadRepo::set_instructions(pool, &ws("ws-a"), "s1", "original").await.unwrap();

    let err = SquadRepo::set_member_role(pool, &ws("ws-b"), "s1", &agent("a-1"), "hijacked")
        .await
        .unwrap_err();
    assert!(matches!(err, SquadRepoError::NotFound), "got {err:?}");
    let err = SquadRepo::set_instructions(pool, &ws("ws-b"), "s1", "hijacked").await.unwrap_err();
    assert!(matches!(err, SquadRepoError::NotFound), "got {err:?}");

    let s = SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().expect("present");
    assert_eq!(s.instructions, "original", "the other tenant wrote nothing");
    assert_eq!(s.members[0].role, "keeper");
}

/// A plain `add_member` re-add PRESERVES an existing role (the `DO NOTHING`
/// conflict path is load-bearing); an `add_member_with_role` re-add overwrites
/// it, because supplying a role is explicit intent.
#[tokio::test]
async fn re_add_preserves_a_role_unless_a_role_is_supplied() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir).await;
    let pool = store.pool();
    seed_ws(pool, "ws-a").await;
    SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1).await.unwrap();
    SquadRepo::add_member_with_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "owns the migrations")
        .await
        .unwrap();

    // Plain re-add: role survives.
    SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
    let s = SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap();
    assert_eq!(s.members.len(), 1, "still one membership");
    assert_eq!(
        s.members[0].role, "owns the migrations",
        "a plain re-add must never clear a role"
    );

    // Re-add WITH a role: overwritten.
    SquadRepo::add_member_with_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "owns the CLI")
        .await
        .unwrap();
    let s = SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap();
    assert_eq!(s.members.len(), 1);
    assert_eq!(s.members[0].role, "owns the CLI");

    // An explicit empty role clears it.
    SquadRepo::add_member_with_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "").await.unwrap();
    let s = SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap();
    assert_eq!(s.members[0].role, "");
}

/// `remove_member` then `add_member` yields a ROLELESS membership — the role
/// lives on the row, so dropping the row drops the role.
#[tokio::test]
async fn removing_a_member_drops_its_role() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir).await;
    let pool = store.pool();
    seed_ws(pool, "ws-a").await;
    SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1).await.unwrap();
    SquadRepo::add_member_with_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "keeper").await.unwrap();
    SquadRepo::remove_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
    SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();

    let s = SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap();
    assert_eq!(s.members[0].role, "", "a re-created membership starts roleless");
}

/// `set_instructions` round-trips VERBATIM through BOTH read paths, including an
/// embedded newline (the text reaches an agent's `CLAUDE.md` unescaped), and
/// `""` clears it. Surrounding whitespace is trimmed; interior text is not.
#[tokio::test]
async fn instructions_round_trip_verbatim_and_clear() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir).await;
    let pool = store.pool();
    seed_ws(pool, "ws-a").await;
    SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1).await.unwrap();

    let text = "Route schema work to the DB owner.\n\n- Escalate a red CI to the reporter.";
    SquadRepo::set_instructions(pool, &ws("ws-a"), "s1", &format!("  {text}  ")).await.unwrap();

    let via_get = SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap();
    assert_eq!(via_get.instructions, text, "verbatim through `get`");
    let via_list = SquadRepo::list(pool, &ws("ws-a")).await.unwrap();
    assert_eq!(via_list[0].instructions, text, "verbatim through `list`");

    SquadRepo::set_instructions(pool, &ws("ws-a"), "s1", "").await.unwrap();
    assert_eq!(
        SquadRepo::get(pool, &ws("ws-a"), "s1").await.unwrap().unwrap().instructions,
        "",
        "an empty value clears the field"
    );
}

/// D3: `member_agent_ids` — the FAN-OUT query — is role-blind. Roles never
/// filter dispatch; every agent member is still dispatched to.
#[tokio::test]
async fn the_fanout_query_is_role_blind() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir).await;
    let pool = store.pool();
    seed_ws(pool, "ws-a").await;
    SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1).await.unwrap();
    SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
    SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-2")).await.unwrap();
    let roleless = SquadRepo::member_agent_ids(pool, &ws("ws-a"), "s1").await.unwrap();

    SquadRepo::set_member_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "owns the migrations")
        .await
        .unwrap();
    let roled = SquadRepo::member_agent_ids(pool, &ws("ws-a"), "s1").await.unwrap();

    assert_eq!(
        roled, roleless,
        "roles must not change who the fan-out dispatches to"
    );
    assert_eq!(roled, vec!["a-1".to_string(), "a-2".to_string()]);
}

/// Deleting a workspace still tears down its `squad_member` rows — the new
/// column does not disturb `WORKSPACE_TEARDOWN`'s child-before-parent order.
#[tokio::test]
async fn workspace_delete_still_cascades_memberships() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir).await;
    let pool = store.pool();
    seed_ws(pool, "ws-a").await;
    // A second workspace so the delete is not blocked by the last-workspace guard.
    seed_ws(pool, "ws-keep").await;
    SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1).await.unwrap();
    SquadRepo::add_member_with_role(pool, &ws("ws-a"), "s1", &agent("a-1"), "keeper").await.unwrap();
    SquadRepo::add_member_with_role(pool, &ws("ws-a"), "s1", &human("u-1"), "reviewer")
        .await
        .unwrap();
    assert_eq!(member_count(pool).await, 2);

    ainb_hangar_store::repo::workspace::WorkspaceRepo::delete(pool, &ws("ws-a"))
        .await
        .expect("workspace teardown");
    assert_eq!(
        member_count(pool).await,
        0,
        "squad_member must cascade with the workspace"
    );
}
