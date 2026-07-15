//! Typed repository wrapper over the `squad` × `squad_member` tables (e38.17).
//!
//! A [`Squad`] is a workspace-scoped group of actors with a designated LEADER
//! ([`migration 0017`](../../migrations/0017_squad.sql)). It is the first
//! construct that lets work be assigned to a *team* rather than a single actor.
//! This repo is the only mutation + read surface over the two tables: create a
//! squad with a leader, add / remove member actors, list squads (with their
//! members), and — crucially — resolve a squad to its LEADER's agent id, which is
//! how leader-routing actually takes effect.
//!
//! # Leader routing
//!
//! A squad does NOT introduce a new [`ActorKind`](ainb_hangar_core::actor::ActorKind).
//! Routing rides the squad's leader actor-ref: the existing `agent_task_queue` is
//! `agent_id`-keyed and claimed per-runtime, so a task enqueued for a squad
//! carries the LEADER's `agent_id` and the existing claim/dispatch path routes it
//! to the leader agent with no claim-path change.
//! [`leader_agent_id`](SquadRepo::leader_agent_id) is the seam: it resolves a
//! squad to its leader's agent id (`Some` when the leader is an `agent`, `None`
//! when the leader is a human `member` — a human leader carries no agent to
//! dispatch to), so the daemon / CLI can build a `NewTask` that the leader claims.
//!
//! # Workspace scoping
//!
//! **Every method takes a [`WorkspaceId`] and enforces it in SQL.** A squad id /
//! name from another tenant resolves to no row (a
//! [`SquadRepoError::NotFound`], never a cross-tenant edit), and listing a foreign
//! / unknown workspace yields an empty vec. The `(workspace_id, name)` UNIQUE
//! index is the resolve-or-reject guard: creating a second squad with a name that
//! already exists in the workspace is rejected with [`SquadRepoError::DuplicateName`].
//!
//! The `(workspace_id, name)` UNIQUE index, the `(squad_id, member_type,
//! member_id)` composite PK, the two `leader_type`/`member_type` CHECKs, and the
//! `squad_id` FK are the engine-enforced invariants (foreign keys ARE on — sqlx
//! enables `PRAGMA foreign_keys` by default). The actor-ref `_id` halves carry no
//! FK by design (they are polymorphic), and an FK could not express tenant scope
//! anyway, so the workspace-scope guard and the leader-resolution are enforced
//! here in application code.

use ainb_hangar_core::actor::ActorRef;
use ainb_hangar_core::ids::WorkspaceId;
use sqlx::SqlitePool;

/// One squad with its leader and members (the list row).
///
/// `id` + `name` identify the squad within its workspace; `leader` is the
/// polymorphic leader actor-ref (`member:<id>` / `agent:<id>`); `members` are the
/// squad's member actor-refs (ordered for a stable render). A pure read model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Squad {
    /// The squad's id (`squad.id`).
    pub id: String,
    /// The squad's name (unique within its workspace).
    pub name: String,
    /// The squad's leader as a polymorphic actor-ref.
    pub leader: ActorRef,
    /// The squad's member actor-refs (may be empty), ordered by `(type, id)`.
    pub members: Vec<ActorRef>,
}

/// Stateless typed wrapper over the `squad` + `squad_member` tables.
pub struct SquadRepo;

impl SquadRepo {
    /// Create a squad named `name` in `workspace` with `leader`, returning its id.
    ///
    /// Workspace-scoped + resolve-or-reject: a name that already exists in the
    /// workspace is rejected with [`SquadRepoError::DuplicateName`] (the
    /// `(workspace_id, name)` UNIQUE index), never a silent overwrite. The `leader`
    /// actor-ref's discriminant is kept honest by the table's `leader_type` CHECK.
    ///
    /// # Errors
    ///
    /// Returns [`SquadRepoError::DuplicateName`] when the name is taken in the
    /// workspace, or [`SquadRepoError::Db`] on a store failure.
    pub async fn create(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &str,
        name: &str,
        leader: &ActorRef,
        created_at: i64,
    ) -> Result<String, SquadRepoError> {
        let res = sqlx::query(
            "INSERT INTO squad (id, workspace_id, name, leader_type, leader_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(workspace.as_str())
        .bind(name)
        .bind(leader.kind().as_str())
        .bind(leader.id())
        .bind(created_at)
        .execute(pool)
        .await;
        match res {
            Ok(_) => Ok(id.to_string()),
            Err(e) if is_unique_violation(&e) => Err(SquadRepoError::DuplicateName),
            Err(e) => Err(SquadRepoError::Db(e)),
        }
    }

    /// Add `member` to `squad` within `workspace`, idempotently.
    ///
    /// Workspace-scoped: a squad id that does not belong to `workspace` resolves
    /// to no squad and is rejected with [`SquadRepoError::NotFound`] (never a
    /// cross-tenant edit). Adding the same member twice is a no-op (the
    /// `(squad_id, member_type, member_id)` composite PK dedupes via
    /// `ON CONFLICT DO NOTHING`).
    ///
    /// # Errors
    ///
    /// Returns [`SquadRepoError::NotFound`] when the squad is not in `workspace`,
    /// or [`SquadRepoError::Db`] on a store failure.
    pub async fn add_member(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        squad_id: &str,
        member: &ActorRef,
    ) -> Result<(), SquadRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_squad_in_ws(&mut tx, workspace, squad_id).await?;
        sqlx::query(
            "INSERT INTO squad_member (squad_id, member_type, member_id) VALUES (?, ?, ?) \
             ON CONFLICT (squad_id, member_type, member_id) DO NOTHING",
        )
        .bind(squad_id)
        .bind(member.kind().as_str())
        .bind(member.id())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove `member` from `squad` within `workspace`, idempotently.
    ///
    /// Workspace-scoped like [`add_member`](Self::add_member): a foreign squad id
    /// is rejected with [`SquadRepoError::NotFound`]. Removing a member that is not
    /// in the squad is a no-op (idempotent).
    ///
    /// # Errors
    ///
    /// Returns [`SquadRepoError::NotFound`] when the squad is not in `workspace`,
    /// or [`SquadRepoError::Db`] on a store failure.
    pub async fn remove_member(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        squad_id: &str,
        member: &ActorRef,
    ) -> Result<(), SquadRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_squad_in_ws(&mut tx, workspace, squad_id).await?;
        sqlx::query(
            "DELETE FROM squad_member WHERE squad_id = ? AND member_type = ? AND member_id = ?",
        )
        .bind(squad_id)
        .bind(member.kind().as_str())
        .bind(member.id())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// List every squad of `workspace` with its members, ordered by name.
    ///
    /// Workspace-scoped: a foreign / unknown workspace yields an empty vec, never
    /// another tenant's squads. Each squad's `members` are ordered by
    /// `(member_type, member_id)` for a stable render.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a query or actor-ref decode fails.
    pub async fn list(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Vec<Squad>, sqlx::Error> {
        use sqlx::Row;
        let squad_rows = sqlx::query(
            "SELECT id, name, leader_type, leader_id FROM squad \
             WHERE workspace_id = ? ORDER BY name, id",
        )
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;

        let mut squads = Vec::with_capacity(squad_rows.len());
        for r in &squad_rows {
            let id: String = r.try_get("id")?;
            let leader = decode_actor(
                &r.try_get::<String, _>("leader_type")?,
                &r.try_get::<String, _>("leader_id")?,
            )?;
            let member_rows = sqlx::query(
                "SELECT member_type, member_id FROM squad_member \
                 WHERE squad_id = ? ORDER BY member_type, member_id",
            )
            .bind(&id)
            .fetch_all(pool)
            .await?;
            let mut members = Vec::with_capacity(member_rows.len());
            for m in &member_rows {
                members.push(decode_actor(
                    &m.try_get::<String, _>("member_type")?,
                    &m.try_get::<String, _>("member_id")?,
                )?);
            }
            squads.push(Squad {
                id,
                name: r.try_get("name")?,
                leader,
                members,
            });
        }
        Ok(squads)
    }

    /// Resolve `squad` to its LEADER's agent id, the seam that makes leader-routing
    /// take effect.
    ///
    /// Returns `Some(agent_id)` when the squad's leader is an `agent` (a task
    /// enqueued with that `agent_id` is claimed by / dispatched to the leader),
    /// `None` when the leader is a human `member` (a human carries no agent to
    /// dispatch to — the caller decides what to do, e.g. reject a squad
    /// assignment whose leader cannot run work). Workspace-scoped: a squad id from
    /// another tenant resolves to `None`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query or actor-ref decode fails.
    pub async fn leader_agent_id(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        squad_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        use ainb_hangar_core::actor::ActorKind;
        let row = sqlx::query_as::<_, (String, String)>(
            "SELECT leader_type, leader_id FROM squad WHERE id = ? AND workspace_id = ?",
        )
        .bind(squad_id)
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
        Ok(row.and_then(|(kind, id)| {
            (kind.parse::<ActorKind>() == Ok(ActorKind::Agent)).then_some(id)
        }))
    }

    /// List the `agent`-typed members of `squad` (their `agent.id`s), the fan-out
    /// seam that makes member dispatch real (P7).
    ///
    /// Returns only the members whose actor-ref is an `agent` (a human `member`
    /// carries no runtime to dispatch to and is skipped), ordered by id for a
    /// stable fan-out. Workspace-scoped: a squad id from another tenant resolves to
    /// an empty vec (the `JOIN squad ... workspace_id` guard), never another
    /// tenant's members. The LEADER is *not* excluded here — a squad whose leader
    /// is also listed as a member yields that agent once in this list; the fan-out
    /// caller dedupes it against the already-dispatched leader.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn member_agent_ids(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        squad_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query_as::<_, (String,)>(
            "SELECT sm.member_id FROM squad_member sm \
             JOIN squad s ON s.id = sm.squad_id \
             WHERE sm.squad_id = ? AND s.workspace_id = ? AND sm.member_type = 'agent' \
             ORDER BY sm.member_id",
        )
        .bind(squad_id)
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Confirm `squad_id` belongs to `workspace` inside `tx`, erroring with
    /// [`SquadRepoError::NotFound`] otherwise (the tenant guard the member
    /// mutations share).
    async fn ensure_squad_in_ws(
        tx: &mut sqlx::SqliteConnection,
        workspace: &WorkspaceId,
        squad_id: &str,
    ) -> Result<(), SquadRepoError> {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM squad WHERE id = ? AND workspace_id = ?")
                .bind(squad_id)
                .bind(workspace.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(SquadRepoError::NotFound);
        }
        Ok(())
    }
}

/// Decode a `(type, id)` pair into a typed [`ActorRef`], mapping a malformed pair
/// (a junk discriminant or empty id the CHECK should have prevented) onto a
/// decode error so a corrupt row surfaces rather than panics.
fn decode_actor(kind: &str, id: &str) -> Result<ActorRef, sqlx::Error> {
    format!("{kind}:{id}")
        .parse::<ActorRef>()
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))
}

/// Whether a `sqlx` error is a `SQLite` UNIQUE-constraint violation (the
/// `(workspace_id, name)` resolve-or-reject guard fired).
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("2067"))
}

/// Error surface for [`SquadRepo`].
///
/// Splits the resolve-or-reject duplicate-name guard and a tenant-isolation /
/// not-found rejection from a raw store fault, mirroring
/// [`MemberRepoError`](crate::repo::member::MemberRepoError).
#[derive(Debug, thiserror::Error)]
pub enum SquadRepoError {
    /// A squad with that name already exists in the workspace (the
    /// `(workspace_id, name)` UNIQUE index fired). The create is rejected.
    #[error("a squad with that name already exists in this workspace")]
    DuplicateName,
    /// The target squad does not belong to the supplied workspace (covers an
    /// unknown squad id too). The mutation is rejected, nothing written.
    #[error("squad not found in this workspace")]
    NotFound,
    /// An underlying `sqlx` failure (IO, decode, …).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use ainb_hangar_core::actor::ActorKind;

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id.to_string()).unwrap()
    }

    fn agent(id: &str) -> ActorRef {
        ActorRef::new(ActorKind::Agent, id).unwrap()
    }

    fn member(id: &str) -> ActorRef {
        ActorRef::new(ActorKind::Member, id).unwrap()
    }

    async fn seed_ws(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(id)
            .bind(id)
            .bind(0_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    /// `create` + `add_member` build a squad with a leader and members; `list`
    /// reads it back with the members ordered.
    #[tokio::test]
    async fn create_add_members_and_list_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1)
            .await
            .unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &member("u-1")).await.unwrap();
        // Idempotent re-add is a no-op.
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();

        let squads = SquadRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert_eq!(squads.len(), 1);
        let s = &squads[0];
        assert_eq!(s.name, "alpha");
        assert_eq!(s.leader, agent("a-lead"));
        assert_eq!(
            s.members,
            vec![agent("a-1"), member("u-1")],
            "ordered, deduped"
        );
    }

    /// A duplicate squad name in the same workspace is rejected (resolve-or-reject).
    #[tokio::test]
    async fn create_rejects_a_duplicate_name() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1)
            .await
            .unwrap();
        let err = SquadRepo::create(pool, &ws("ws-a"), "s2", "alpha", &agent("a-lead"), 2)
            .await
            .unwrap_err();
        assert!(matches!(err, SquadRepoError::DuplicateName), "got {err:?}");
    }

    /// The same name in a DIFFERENT workspace is allowed (the UNIQUE index is
    /// per-tenant), and `list` never leaks a sibling tenant's squad.
    #[tokio::test]
    async fn squads_are_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1)
            .await
            .unwrap();
        SquadRepo::create(pool, &ws("ws-b"), "s2", "alpha", &agent("b-lead"), 1)
            .await
            .unwrap();

        let a = SquadRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert_eq!(a.len(), 1, "only ws-a's squad");
        assert_eq!(a[0].id, "s1");
        let b = SquadRepo::list(pool, &ws("ws-b")).await.unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].id, "s2");
    }

    /// Adding a member through the WRONG workspace touches no row (tenant guard).
    #[tokio::test]
    async fn add_member_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1)
            .await
            .unwrap();
        let err = SquadRepo::add_member(pool, &ws("ws-b"), "s1", &agent("a-1")).await.unwrap_err();
        assert!(matches!(err, SquadRepoError::NotFound), "got {err:?}");
        // s1's membership is untouched.
        let squads = SquadRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert!(squads[0].members.is_empty());
    }

    /// `remove_member` drops the membership; a non-member removal is a no-op.
    #[tokio::test]
    async fn remove_member_drops_membership_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1)
            .await
            .unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
        SquadRepo::remove_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
        // Removing an absent member is a no-op.
        SquadRepo::remove_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
        let squads = SquadRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert!(squads[0].members.is_empty());
    }

    /// `member_agent_ids` returns only the `agent` members (a human `member` is
    /// skipped), ordered by id, and is workspace-scoped (a foreign tenant sees an
    /// empty list) — the fan-out seam.
    #[tokio::test]
    async fn member_agent_ids_lists_only_agent_members_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1)
            .await
            .unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-2")).await.unwrap();
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &agent("a-1")).await.unwrap();
        // A human member is not an agent and must be excluded.
        SquadRepo::add_member(pool, &ws("ws-a"), "s1", &member("u-1")).await.unwrap();

        let ids = SquadRepo::member_agent_ids(pool, &ws("ws-a"), "s1").await.unwrap();
        assert_eq!(
            ids,
            vec!["a-1".to_string(), "a-2".to_string()],
            "agent-only, ordered"
        );

        // A foreign tenant resolves to an empty list (tenant guard).
        seed_ws(pool, "ws-b").await;
        let foreign = SquadRepo::member_agent_ids(pool, &ws("ws-b"), "s1").await.unwrap();
        assert!(foreign.is_empty(), "a foreign tenant sees no members");
    }

    /// `leader_agent_id` resolves an agent leader to its agent id (the routing
    /// seam), returns `None` for a human-member leader, and is workspace-scoped.
    #[tokio::test]
    async fn leader_agent_id_resolves_only_an_agent_leader() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        SquadRepo::create(pool, &ws("ws-a"), "s1", "alpha", &agent("a-lead"), 1)
            .await
            .unwrap();
        SquadRepo::create(pool, &ws("ws-a"), "s2", "beta", &member("u-lead"), 1)
            .await
            .unwrap();

        assert_eq!(
            SquadRepo::leader_agent_id(pool, &ws("ws-a"), "s1").await.unwrap(),
            Some("a-lead".to_string()),
            "an agent leader resolves to its agent id"
        );
        assert_eq!(
            SquadRepo::leader_agent_id(pool, &ws("ws-a"), "s2").await.unwrap(),
            None,
            "a human-member leader carries no agent"
        );
        // A foreign workspace resolves to None (tenant guard).
        seed_ws(pool, "ws-b").await;
        assert_eq!(
            SquadRepo::leader_agent_id(pool, &ws("ws-b"), "s1").await.unwrap(),
            None,
            "a foreign tenant cannot resolve the squad's leader"
        );
    }
}
