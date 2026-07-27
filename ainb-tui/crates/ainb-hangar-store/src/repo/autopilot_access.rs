//! Typed repositories over the `autopilot_subscriber` + `autopilot_collaborator`
//! tables, and the ONE definition of "may this actor write this rule"
//! (multica parity #27, migration 0064).
//!
//! Two per-RULE actor sets, both shaped exactly like [`super::issue_subscriber`]:
//!
//! * **subscribers** — the standing NOTIFY list. Every issue the autopilot
//!   SPAWNS auto-subscribes this set (see [`super::autopilot_run`]), so a human
//!   tracking a recurring automation is notified per occurrence without having
//!   to watch each spawned issue by hand.
//! * **collaborators** — explicit WRITE-GRANTS on the rule itself, beyond the
//!   implicit owner / workspace-owner / workspace-admin.
//!
//! # Set membership, first-grant-wins
//!
//! `(autopilot, actor)` is the primary key on both tables and every add is
//! `INSERT OR IGNORE`, so re-adding an existing collaborator keeps the ORIGINAL
//! row (and its `created_at`). A ROLE CHANGE is therefore an explicit
//! [`AutopilotCollaboratorRepo::set_role`], never an accidental side effect of a
//! re-add.
//!
//! # Workspace scoping without trusting the caller
//!
//! Neither table is read by id alone on the write path: every mutation is
//! **scoped through `WHERE EXISTS (SELECT 1 FROM autopilot WHERE id = ? AND
//! workspace_id = ?)`**, so a foreign tenant's write returns `Ok(false)` rather
//! than erroring or landing.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
use sqlx::{Row, SqlitePool};

use super::autopilot::AccessMode;
use super::member::{MemberRepo, MemberRole};

/// What an explicit grant lets its holder do.
///
/// The column carries NO `CHECK` (SQLite cannot widen one without a full table
/// rebuild), so this enum is the vocabulary's only enforcement: [`Self::as_db_str`]
/// is the single writer and [`Self::parse`] is a tolerant reader that yields
/// `None` for a token written by a newer build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CollaboratorRole {
    /// May WRITE the rule (edit / enable / disable / re-grant). The default when
    /// a caller omits a role.
    #[default]
    Editor,
    /// May be listed against the rule but grants NO write. Deliberately weaker
    /// than "no row at all" only in that it is visible.
    Viewer,
}

impl CollaboratorRole {
    /// The token stored in `autopilot_collaborator.role`. The ONLY writer of
    /// that column.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::Viewer => "viewer",
        }
    }

    /// Tolerant reader: an unknown token (a newer build's vocabulary) yields
    /// `None` rather than failing the whole read — and, because only `Editor`
    /// grants write, an unknown role can never widen access.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "editor" => Some(Self::Editor),
            "viewer" => Some(Self::Viewer),
            _ => None,
        }
    }
}

/// One materialised `autopilot_collaborator` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutopilotCollaborator {
    /// The rule the grant is on.
    pub autopilot_id: String,
    /// The granted actor, re-assembled from its two TEXT columns.
    pub actor: ActorRef,
    /// The parsed role, `None` for a token this build does not know.
    pub role: Option<CollaboratorRole>,
    /// The raw stored token, always available for rendering.
    pub role_raw: String,
    /// Who granted it; `None` = unattributed.
    pub created_by: Option<ActorRef>,
    /// When the grant was created (epoch millis).
    pub created_at: i64,
}

/// One materialised `autopilot_subscriber` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutopilotSubscriberRow {
    /// The rule subscribed to.
    pub autopilot_id: String,
    /// The subscribing actor.
    pub actor: ActorRef,
    /// Who added them; `None` = unattributed.
    pub created_by: Option<ActorRef>,
    /// When the subscription was created (epoch millis).
    pub created_at: i64,
}

fn actor_of(row: &sqlx::sqlite::SqliteRow) -> Option<ActorRef> {
    let kind: String = row.get("actor_type");
    let id: String = row.get("actor_id");
    ActorRef::new(kind.parse::<ActorKind>().ok()?, id).ok()
}

fn created_by_of(row: &sqlx::sqlite::SqliteRow) -> Option<ActorRef> {
    let raw: Option<String> = row.get("created_by");
    raw.and_then(|s| s.parse::<ActorRef>().ok())
}

/// Stateless typed wrapper over the `autopilot_collaborator` table.
pub struct AutopilotCollaboratorRepo;

impl AutopilotCollaboratorRepo {
    /// Idempotently grant `actor` a role on `autopilot_id`, workspace-scoped
    /// through the join to `autopilot`.
    ///
    /// Returns `Ok(true)` when a row landed, `Ok(false)` when it did not —
    /// either because `(autopilot, workspace)` resolves to nothing (a foreign
    /// tenant writes nothing rather than erroring) or because the actor ALREADY
    /// held a grant, in which case the existing row keeps its ORIGINAL role and
    /// `created_at` (first-grant-wins; use [`Self::set_role`] to change one).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement fails.
    pub async fn add(
        pool: &SqlitePool,
        workspace_id: &str,
        autopilot_id: &str,
        actor: &ActorRef,
        role: CollaboratorRole,
        created_by: Option<&ActorRef>,
        now_ms: i64,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "INSERT OR IGNORE INTO autopilot_collaborator \
                 (autopilot_id, workspace_id, actor_type, actor_id, role, created_by, created_at) \
             SELECT ?, ?, ?, ?, ?, ?, ? \
             WHERE EXISTS (SELECT 1 FROM autopilot WHERE id = ? AND workspace_id = ?)",
        )
        .bind(autopilot_id)
        .bind(workspace_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(role.as_db_str())
        .bind(created_by.map(ToString::to_string))
        .bind(now_ms)
        .bind(autopilot_id)
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Change an existing grant's role. The ONLY role mutator — a re-`add` is
    /// deliberately inert.
    ///
    /// Returns `Ok(false)` when no such grant exists in this workspace.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement fails.
    pub async fn set_role(
        pool: &SqlitePool,
        workspace_id: &str,
        autopilot_id: &str,
        actor: &ActorRef,
        role: CollaboratorRole,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE autopilot_collaborator SET role = ? \
             WHERE autopilot_id = ? AND actor_type = ? AND actor_id = ? AND workspace_id = ?",
        )
        .bind(role.as_db_str())
        .bind(autopilot_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Revoke `actor`'s grant. Removing an absent grant is an idempotent
    /// `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement fails.
    pub async fn remove(
        pool: &SqlitePool,
        workspace_id: &str,
        autopilot_id: &str,
        actor: &ActorRef,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM autopilot_collaborator \
             WHERE autopilot_id = ? AND actor_type = ? AND actor_id = ? AND workspace_id = ?",
        )
        .bind(autopilot_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Every grant on one rule, oldest first.
    ///
    /// Ordered by `created_at`, then `actor_type`, `actor_id` — a bare
    /// `ORDER BY created_at` is non-deterministic within a millisecond.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list(
        pool: &SqlitePool,
        autopilot_id: &str,
    ) -> Result<Vec<AutopilotCollaborator>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT autopilot_id, actor_type, actor_id, role, created_by, created_at \
             FROM autopilot_collaborator WHERE autopilot_id = ? \
             ORDER BY created_at, actor_type, actor_id",
        )
        .bind(autopilot_id)
        .fetch_all(pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let actor = actor_of(&r)?;
                let role_raw: String = r.get("role");
                Some(AutopilotCollaborator {
                    autopilot_id: r.get("autopilot_id"),
                    actor,
                    role: CollaboratorRole::parse(&role_raw),
                    role_raw,
                    created_by: created_by_of(&r),
                    created_at: r.get("created_at"),
                })
            })
            .collect())
    }

    /// One grant by `(autopilot, actor)`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn get(
        pool: &SqlitePool,
        autopilot_id: &str,
        actor: &ActorRef,
    ) -> Result<Option<AutopilotCollaborator>, sqlx::Error> {
        Ok(Self::list(pool, autopilot_id).await?.into_iter().find(|c| &c.actor == actor))
    }

    /// How many grants exist on one rule.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn count(pool: &SqlitePool, autopilot_id: &str) -> Result<u32, sqlx::Error> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM autopilot_collaborator WHERE autopilot_id = ?",
        )
        .bind(autopilot_id)
        .fetch_one(pool)
        .await?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Grant counts for EVERY rule in a workspace, as `(autopilot_id, count)`.
    ///
    /// One `GROUP BY` for the whole list screen — deliberately not N+1 per row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn counts_by_autopilot(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<(String, u32)>, sqlx::Error> {
        counts_by_autopilot(pool, workspace_id, "autopilot_collaborator").await
    }
}

/// Stateless typed wrapper over the `autopilot_subscriber` table.
pub struct AutopilotSubscriberRepo;

impl AutopilotSubscriberRepo {
    /// Idempotently subscribe `actor` to `autopilot_id`, workspace-scoped the
    /// same way as [`AutopilotCollaboratorRepo::add`].
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement fails.
    pub async fn add(
        pool: &SqlitePool,
        workspace_id: &str,
        autopilot_id: &str,
        actor: &ActorRef,
        created_by: Option<&ActorRef>,
        now_ms: i64,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "INSERT OR IGNORE INTO autopilot_subscriber \
                 (autopilot_id, workspace_id, actor_type, actor_id, created_by, created_at) \
             SELECT ?, ?, ?, ?, ?, ? \
             WHERE EXISTS (SELECT 1 FROM autopilot WHERE id = ? AND workspace_id = ?)",
        )
        .bind(autopilot_id)
        .bind(workspace_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(created_by.map(ToString::to_string))
        .bind(now_ms)
        .bind(autopilot_id)
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Unsubscribe `actor`. Removing an absent subscription is an idempotent
    /// `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement fails.
    pub async fn remove(
        pool: &SqlitePool,
        workspace_id: &str,
        autopilot_id: &str,
        actor: &ActorRef,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM autopilot_subscriber \
             WHERE autopilot_id = ? AND actor_type = ? AND actor_id = ? AND workspace_id = ?",
        )
        .bind(autopilot_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Every subscriber of one rule, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list(
        pool: &SqlitePool,
        autopilot_id: &str,
    ) -> Result<Vec<AutopilotSubscriberRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT autopilot_id, actor_type, actor_id, created_by, created_at \
             FROM autopilot_subscriber WHERE autopilot_id = ? \
             ORDER BY created_at, actor_type, actor_id",
        )
        .bind(autopilot_id)
        .fetch_all(pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let actor = actor_of(&r)?;
                Some(AutopilotSubscriberRow {
                    autopilot_id: r.get("autopilot_id"),
                    actor,
                    created_by: created_by_of(&r),
                    created_at: r.get("created_at"),
                })
            })
            .collect())
    }

    /// The fan-out read: just the subscribing actors, no provenance.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn actors(
        pool: &SqlitePool,
        autopilot_id: &str,
    ) -> Result<Vec<ActorRef>, sqlx::Error> {
        Ok(Self::list(pool, autopilot_id).await?.into_iter().map(|s| s.actor).collect())
    }

    /// How many actors follow one rule.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn count(pool: &SqlitePool, autopilot_id: &str) -> Result<u32, sqlx::Error> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM autopilot_subscriber WHERE autopilot_id = ?")
                .bind(autopilot_id)
                .fetch_one(pool)
                .await?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Subscriber counts for EVERY rule in a workspace, as
    /// `(autopilot_id, count)`. One `GROUP BY`, not N+1.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn counts_by_autopilot(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<(String, u32)>, sqlx::Error> {
        counts_by_autopilot(pool, workspace_id, "autopilot_subscriber").await
    }
}

/// Shared `GROUP BY` for both actor-set tables. `table` is a compile-time
/// literal supplied by the two call sites above, never caller input.
async fn counts_by_autopilot(
    pool: &SqlitePool,
    workspace_id: &str,
    table: &'static str,
) -> Result<Vec<(String, u32)>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT autopilot_id, COUNT(*) AS n FROM {table} \
         WHERE workspace_id = ? GROUP BY autopilot_id"
    ))
    .bind(workspace_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let n: i64 = r.get("n");
            (
                r.get::<String, _>("autopilot_id"),
                u32::try_from(n).unwrap_or(u32::MAX),
            )
        })
        .collect())
}

/// Why [`can_write`] said yes — surfaced so a caller (and a test) can tell an
/// `access_mode = 'open'` pass from a real grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllowReason {
    /// The rule is `access_mode = 'open'` — today's behaviour, unchanged.
    ModeOpen,
    /// The actor published rule version 1, i.e. created the rule.
    Owner,
    /// The actor is a workspace `owner` or `admin`.
    WorkspaceAdmin,
    /// The actor holds an explicit `editor` collaborator grant.
    Collaborator,
}

/// The verdict of the write predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteDecision {
    /// The actor may write, for this reason.
    Allowed(AllowReason),
    /// The actor may not write.
    Denied,
}

impl WriteDecision {
    /// Whether the decision permits the write.
    #[must_use]
    pub const fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed(_))
    }
}

/// multica's `creator OR workspace-owner OR workspace-admin OR explicit
/// collaborator`, plus hangar's `access_mode = 'open'` short-circuit
/// (migration 0064 decision 4). The ONE definition of "may this actor write
/// this rule".
///
/// Resolution order:
///
/// 1. the autopilot does not exist in this workspace → `Denied` (the caller
///    reports not-found; the predicate never leaks a foreign rule's mode);
/// 2. `access_mode = 'open'` → `Allowed(ModeOpen)`;
/// 3. the actor published rule version **1** → `Allowed(Owner)`. Owner is
///    DERIVED, not stored: an unversioned (pre-0061) or unattributed rule
///    simply has no owner — an honest unknown, never a fabricated one;
/// 4. the actor is a `member` with `owner`/`admin` in this workspace →
///    `Allowed(WorkspaceAdmin)`;
/// 5. an `editor` collaborator grant exists → `Allowed(Collaborator)`. A
///    `viewer` grant does NOT grant write, and neither does an unparseable
///    role token;
/// 6. otherwise `Denied`.
///
/// Deliberately NOT baked into [`super::autopilot::AutopilotRepo::update_as`]
/// and friends: authorization belongs at the request seam (which is also where
/// the reference puts it), and burying it in the repo would silently change the
/// meaning of every legacy `actor = None` caller.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if any of the underlying queries fail.
pub async fn can_write(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    autopilot_id: &AutopilotId,
    actor: &ActorRef,
) -> Result<WriteDecision, sqlx::Error> {
    let mode: Option<String> =
        sqlx::query_scalar("SELECT access_mode FROM autopilot WHERE id = ? AND workspace_id = ?")
            .bind(autopilot_id.as_str())
            .bind(workspace.as_str())
            .fetch_optional(pool)
            .await?;
    let Some(mode) = mode else {
        return Ok(WriteDecision::Denied);
    };
    if AccessMode::from_db_str(&mode) == AccessMode::Open {
        return Ok(WriteDecision::Allowed(AllowReason::ModeOpen));
    }

    // The rule's OWNER: whoever published version 1.
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT published_by FROM autopilot_rule_version \
         WHERE autopilot_id = ? AND workspace_id = ? ORDER BY version ASC LIMIT 1",
    )
    .bind(autopilot_id.as_str())
    .bind(workspace.as_str())
    .fetch_optional(pool)
    .await?
    .flatten();
    if owner.as_deref() == Some(actor.to_string().as_str()) {
        return Ok(WriteDecision::Allowed(AllowReason::Owner));
    }

    if actor.kind() == ActorKind::Member {
        let role = MemberRepo::role(pool, workspace, actor.id()).await?;
        if matches!(role, Some(MemberRole::Owner | MemberRole::Admin)) {
            return Ok(WriteDecision::Allowed(AllowReason::WorkspaceAdmin));
        }
    }

    let grant = AutopilotCollaboratorRepo::get(pool, autopilot_id.as_str(), actor).await?;
    if grant.is_some_and(|g| g.role == Some(CollaboratorRole::Editor)) {
        return Ok(WriteDecision::Allowed(AllowReason::Collaborator));
    }

    Ok(WriteDecision::Denied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_tokens_round_trip_and_unknown_reads_none() {
        for r in [CollaboratorRole::Editor, CollaboratorRole::Viewer] {
            assert_eq!(CollaboratorRole::parse(r.as_db_str()), Some(r));
        }
        assert_eq!(CollaboratorRole::parse("archivist"), None);
    }

    #[test]
    fn editor_is_the_default_role() {
        assert_eq!(CollaboratorRole::default(), CollaboratorRole::Editor);
    }
}
