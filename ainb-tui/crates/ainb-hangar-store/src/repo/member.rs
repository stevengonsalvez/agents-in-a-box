//! Typed repository wrapper over the `member` × `user` tables (e38.11).
//!
//! A [`Member`] is a `(workspace_id, user_id)` row carrying a `role` of `owner`,
//! `admin`, or `member` (migration 0001's `CHECK` constraint). The `member` table
//! has been data-only since v1 — seeded once as the workspace owner — so this repo
//! is the first *mutation* surface over it: list, set-role, and remove, each
//! workspace-scoped and each guarding the workspace's last owner.
//!
//! # Workspace scoping
//!
//! **Every method takes a [`WorkspaceId`] and enforces it in SQL.** The `member`
//! PK is `(workspace_id, user_id)`, so a `(workspace, user)` pair from another
//! tenant resolves to no row and a mutation aimed at it touches nothing (a
//! [`MemberRepoError::NotFound`], never a cross-tenant edit). Listing a foreign /
//! unknown workspace yields an empty vec, never another tenant's members.
//!
//! # The last-owner invariant
//!
//! A workspace must always keep at least one `owner` (otherwise nobody can
//! administer it). Both [`set_role`](MemberRepo::set_role) and
//! [`remove`](MemberRepo::remove) refuse the mutation when it would drop the
//! workspace's owner count to zero — demoting the sole owner, or removing it,
//! is rejected with [`MemberRepoError::LastOwner`]. The check counts owners
//! INSIDE the mutation's transaction (so a concurrent demotion cannot race past
//! it) and only blocks when the *target* is currently an owner and is the *only*
//! one; demoting one of two owners is allowed.
//!
//! The `(workspace_id, user_id)` composite PK and the `role` `CHECK` are the
//! engine-enforced invariants (as are the declared FKs — sqlx enables `PRAGMA
//! foreign_keys` by default). The role-token validation and the last-owner guard
//! are semantic rules no constraint can express, so they are enforced here in
//! application code.

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::WorkspaceId;
use sqlx::SqlitePool;

/// The three roles a member can hold within a workspace (`member.role`).
///
/// The set is closed by migration 0001's `CHECK (role IN ('owner','admin','member'))`;
/// this enum mirrors it so a bad token is rejected at the repo boundary (an
/// [`MemberRepoError::InvalidRole`]) before it ever reaches SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    /// Full administrative control; a workspace must always keep at least one.
    Owner,
    /// Elevated management, short of ownership.
    Admin,
    /// A regular member.
    Member,
}

impl MemberRole {
    /// The wire / column token for this role.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    /// Parse a role token, returning `None` for anything outside the closed set.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "owner" => Some(Self::Owner),
            "admin" => Some(Self::Admin),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

/// One workspace member joined with its user record (the list row).
///
/// `user_id` + `email` come from the `user` join; `role` is the `member.role`
/// token. Display-only — the email is the human label the Members pane renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The member's user id (`user.id`).
    pub user_id: String,
    /// The member's email (`user.email`) — the display label.
    pub email: String,
    /// The member's role token (`owner` / `admin` / `member`).
    pub role: String,
}

/// Stateless typed wrapper over the `member` + `user` tables.
pub struct MemberRepo;

impl MemberRepo {
    /// Add a human member: find-or-create the `user` by email, then insert the
    /// `(workspace_id, user_id, role)` membership. Workspace-scoped.
    ///
    /// Mirrors multica's membership flow (`workspace.go` `CreateMember`,
    /// auto-stubbing a `user` by email when none exists). In one transaction it
    /// looks up `user.id` for `email`; if absent it mints a ULID user id (the
    /// store's [`IdGen`] convention) and inserts the `user` row, then inserts the
    /// membership. An existing user (a member of another workspace, say) is
    /// reused, so the same email in a sibling tenant is a *separate* membership
    /// over one shared user row.
    ///
    /// Idempotent-safe: re-adding an existing `(workspace, user)` pair trips the
    /// member composite-PK uniqueness and is reported as
    /// [`MemberRepoError::AlreadyMember`] (nothing written), never a duplicate.
    ///
    /// Returns the created membership as a [`Member`] (the same shape
    /// [`list`](MemberRepo::list) returns).
    ///
    /// # Errors
    ///
    /// Returns [`MemberRepoError::EmptyEmail`] when `email` is blank,
    /// [`MemberRepoError::AlreadyMember`] when the pair already exists, or
    /// [`MemberRepoError::Db`] on a store failure.
    pub async fn add(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        email: &str,
        role: MemberRole,
    ) -> Result<Member, MemberRepoError> {
        let mut tx = pool.begin().await?;
        let member = Self::add_in_tx(&mut tx, workspace, email, role).await?;
        tx.commit().await?;
        Ok(member)
    }

    /// The body of [`add`](MemberRepo::add), running inside a caller-owned
    /// transaction so a join can be made ATOMIC with another write.
    ///
    /// [`InvitationRepo::accept`](crate::repo::invitation::InvitationRepo::accept)
    /// (parity #18) must flip the invitation to `accepted` *and* create the
    /// membership in ONE transaction — multica does exactly this with a `qtx`.
    /// `add` is now a thin `begin → add_in_tx → commit`, so this extraction is a
    /// pure refactor: the public verb's behaviour is unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`MemberRepoError::EmptyEmail`] when `email` is blank,
    /// [`MemberRepoError::AlreadyMember`] when the pair already exists, or
    /// [`MemberRepoError::Db`] on a store failure. On any error the caller's
    /// transaction is left un-committed (i.e. rolled back on drop).
    pub(crate) async fn add_in_tx(
        tx: &mut sqlx::SqliteConnection,
        workspace: &WorkspaceId,
        email: &str,
        role: MemberRole,
    ) -> Result<Member, MemberRepoError> {
        let email = email.trim();
        if email.is_empty() {
            return Err(MemberRepoError::EmptyEmail);
        }

        // Find-or-create the user by email (`user.email` is NOT NULL UNIQUE).
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM user WHERE email = ?")
            .bind(email)
            .fetch_optional(&mut *tx)
            .await?;
        let user_id = if let Some(id) = existing {
            id
        } else {
            let id = SystemIdGen.new_ulid();
            let now = HangarClock::now_ms(&SystemClock);
            sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
                .bind(&id)
                .bind(email)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            id
        };

        // Insert the membership. A conflict on the composite PK means this user is
        // already a member of this workspace → AlreadyMember (not a raw Db error).
        let inserted =
            sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, ?)")
                .bind(workspace.as_str())
                .bind(&user_id)
                .bind(role.as_str())
                .execute(&mut *tx)
                .await;
        if let Err(e) = inserted {
            let already_member = e
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation);
            if already_member {
                return Err(MemberRepoError::AlreadyMember);
            }
            return Err(MemberRepoError::Db(e));
        }
        Ok(Member {
            user_id,
            email: email.to_string(),
            role: role.as_str().to_string(),
        })
    }

    /// Read `user_id`'s role within `workspace`, or `None` when the pair matches no
    /// member (an unknown user, or a member owned by another tenant).
    ///
    /// The public, non-transactional counterpart of [`member_role_in_tx`], used as
    /// the workspace-membership predicate by
    /// [`crate::repo::agent::AgentRepo::can_invoke`] (gap #8): `Some(_)` means "is a
    /// member of this workspace". An out-of-set stored token (which the `CHECK`
    /// constraint should prevent) reads back `None`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn role(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        user_id: &str,
    ) -> Result<Option<MemberRole>, sqlx::Error> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT role FROM member WHERE workspace_id = ? AND user_id = ?")
                .bind(workspace.as_str())
                .bind(user_id)
                .fetch_optional(pool)
                .await?;
        Ok(raw.as_deref().and_then(MemberRole::parse))
    }

    /// List every member of `workspace`, joined with their user record, ordered
    /// by email (the stable Members-pane render order).
    ///
    /// Workspace-scoped: a foreign / unknown workspace yields an empty vec, never
    /// another tenant's members.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Vec<Member>, sqlx::Error> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT m.user_id AS user_id, u.email AS email, m.role AS role \
             FROM member m JOIN user u ON u.id = m.user_id \
             WHERE m.workspace_id = ? ORDER BY u.email",
        )
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Member {
                    user_id: r.try_get("user_id")?,
                    email: r.try_get("email")?,
                    role: r.try_get("role")?,
                })
            })
            .collect()
    }

    /// Resolve a human `@handle` from a comment body to a workspace member.
    ///
    /// hangar has **no `handle` column on `user`** — the identity it stores is
    /// the email — so the email's LOCAL PART is the human handle:
    /// `alice@example.com` is `@alice`. Matching is attempted in this order,
    /// first hit wins, all workspace-scoped and all case-insensitive except the
    /// exact id:
    ///
    /// 1. exact `user.id` (so a roster that pastes the raw id works);
    /// 2. the email local part (`substr(email, 1, instr(email,'@') - 1)`);
    /// 3. the full email.
    ///
    /// A handle that matches more than one member (two tenants' users sharing a
    /// local part, e.g. `alice@a.com` and `alice@b.com` both in this workspace)
    /// is genuinely ambiguous; the lowest `user.id` wins deterministically
    /// rather than the query returning an arbitrary row. The unambiguous address
    /// is the link form `[@Alice](mention://member/<user_id>)`, which never goes
    /// through this resolver at all — every generated roster prefers it.
    ///
    /// Returns `None` for an unknown handle: an unresolvable mention is an
    /// `ignored` outcome, never an error.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn resolve_handle(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        handle: &str,
    ) -> Result<Option<Member>, sqlx::Error> {
        use sqlx::Row;
        let handle = handle.trim();
        if handle.is_empty() {
            return Ok(None);
        }
        // One query, three ranked predicates: SQLite has no NULLS-ordered CASE
        // shortcut, so the rank is computed and ordered on. `instr` returns 0
        // for an email with no `@`, and `substr(x, 1, -1)` is empty, so such a
        // row can only ever match rule 1 or 3 — never a phantom local part.
        let rows = sqlx::query(
            "SELECT m.user_id AS user_id, u.email AS email, m.role AS role, \
                    CASE WHEN m.user_id = ?1 THEN 0 \
                         WHEN instr(u.email, '@') > 1 \
                              AND lower(substr(u.email, 1, instr(u.email, '@') - 1)) \
                                  = lower(?1) THEN 1 \
                         ELSE 2 END AS rank \
             FROM member m JOIN user u ON u.id = m.user_id \
             WHERE m.workspace_id = ?2 \
               AND ( m.user_id = ?1 \
                     OR lower(u.email) = lower(?1) \
                     OR ( instr(u.email, '@') > 1 \
                          AND lower(substr(u.email, 1, instr(u.email, '@') - 1)) \
                              = lower(?1) ) ) \
             ORDER BY rank, m.user_id LIMIT 1",
        )
        .bind(handle)
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
        rows.map(|r| {
            Ok(Member {
                user_id: r.try_get("user_id")?,
                email: r.try_get("email")?,
                role: r.try_get("role")?,
            })
        })
        .transpose()
    }

    /// Set `user_id`'s role within `workspace` to `role`, guarding the last owner.
    ///
    /// Workspace-scoped: a `(workspace, user)` pair that matches no member row is
    /// rejected with [`MemberRepoError::NotFound`] (covers an unknown user, or a
    /// member owned by another tenant) — never a cross-tenant edit. Demoting the
    /// workspace's *only* owner to a non-owner role is rejected with
    /// [`MemberRepoError::LastOwner`]; demoting one of several owners, or any other
    /// role change, is allowed. The owner-count check and the write run in one
    /// transaction so a concurrent demotion cannot slip the count to zero.
    ///
    /// # Errors
    ///
    /// Returns [`MemberRepoError::NotFound`] when the member is not in `workspace`,
    /// [`MemberRepoError::LastOwner`] when the change would orphan the workspace,
    /// or [`MemberRepoError::Db`] on a store failure.
    pub async fn set_role(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        user_id: &str,
        role: MemberRole,
    ) -> Result<(), MemberRepoError> {
        let mut tx = pool.begin().await?;
        let current = member_role_in_tx(&mut tx, workspace, user_id).await?;
        let Some(current) = current else {
            return Err(MemberRepoError::NotFound);
        };
        // Demoting the sole owner away from `owner` would orphan the workspace.
        if current == "owner"
            && role != MemberRole::Owner
            && owner_count_in_tx(&mut tx, workspace).await? <= 1
        {
            return Err(MemberRepoError::LastOwner);
        }
        sqlx::query("UPDATE member SET role = ? WHERE workspace_id = ? AND user_id = ?")
            .bind(role.as_str())
            .bind(workspace.as_str())
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove `user_id` from `workspace`, guarding the last owner.
    ///
    /// Workspace-scoped: a `(workspace, user)` pair that matches no member row is
    /// rejected with [`MemberRepoError::NotFound`] (covers an unknown user, or a
    /// member owned by another tenant). Removing the workspace's *only* owner is
    /// rejected with [`MemberRepoError::LastOwner`]; removing one of several
    /// owners, or any non-owner, is allowed. The owner-count check and the delete
    /// run in one transaction. The `user` row itself is left intact (a user may
    /// belong to other workspaces); only the membership join is removed.
    ///
    /// # Errors
    ///
    /// Returns [`MemberRepoError::NotFound`] when the member is not in `workspace`,
    /// [`MemberRepoError::LastOwner`] when the removal would orphan the workspace,
    /// or [`MemberRepoError::Db`] on a store failure.
    pub async fn remove(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        user_id: &str,
    ) -> Result<(), MemberRepoError> {
        let mut tx = pool.begin().await?;
        let current = member_role_in_tx(&mut tx, workspace, user_id).await?;
        let Some(current) = current else {
            return Err(MemberRepoError::NotFound);
        };
        // Removing the sole owner would orphan the workspace.
        if current == "owner" && owner_count_in_tx(&mut tx, workspace).await? <= 1 {
            return Err(MemberRepoError::LastOwner);
        }
        sqlx::query("DELETE FROM member WHERE workspace_id = ? AND user_id = ?")
            .bind(workspace.as_str())
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Read one member's current role within `workspace`, scoped by the composite PK,
/// inside the mutation's transaction. `None` when no such member exists (an
/// unknown user, or a foreign-tenant pair).
pub(crate) async fn member_role_in_tx(
    tx: &mut sqlx::SqliteConnection,
    workspace: &WorkspaceId,
    user_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT role FROM member WHERE workspace_id = ? AND user_id = ?")
        .bind(workspace.as_str())
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
}

/// Count the `owner`-role members of `workspace` inside the mutation's
/// transaction (drives the last-owner guard).
async fn owner_count_in_tx(
    tx: &mut sqlx::SqliteConnection,
    workspace: &WorkspaceId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM member WHERE workspace_id = ? AND role = 'owner'")
        .bind(workspace.as_str())
        .fetch_one(&mut *tx)
        .await
}

/// Error surface for [`MemberRepo`].
///
/// Splits a tenant-isolation / not-found rejection from the last-owner invariant
/// and a raw store fault, mirroring
/// [`LabelRepoError`](crate::repo::label::LabelRepoError).
#[derive(Debug, thiserror::Error)]
pub enum MemberRepoError {
    /// The target member does not belong to the supplied workspace (covers an
    /// unknown user id too). The mutation is rejected, nothing written.
    #[error("member not found in this workspace")]
    NotFound,
    /// The mutation would leave the workspace with no owner (demoting / removing
    /// the sole owner). Rejected so a workspace always keeps an administrator.
    #[error("a workspace must always keep at least one owner")]
    LastOwner,
    /// The supplied role token is outside the closed `owner`/`admin`/`member`
    /// set. Surfaced by the caller after [`MemberRole::parse`] returns `None`.
    #[error("invalid role: must be one of owner/admin/member")]
    InvalidRole,
    /// [`add`](MemberRepo::add) was called with a blank email. Rejected before
    /// any write (a member's email is its human label; it must be non-empty).
    #[error("email must not be empty")]
    EmptyEmail,
    /// [`add`](MemberRepo::add) targeted a `(workspace, user)` pair that already
    /// exists — the user is already a member of this workspace. Nothing written.
    #[error("that user is already a member of this workspace")]
    AlreadyMember,
    /// An underlying `sqlx` failure (uniqueness conflict, IO, …).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    async fn seed_ws(pool: &SqlitePool, ws: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(ws)
            .bind(ws)
            .bind(ws)
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn seed_member(pool: &SqlitePool, ws: &str, user: &str, email: &str, role: &str) {
        sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, 1000)")
            .bind(user)
            .bind(email)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, ?)")
            .bind(ws)
            .bind(user)
            .bind(role)
            .execute(pool)
            .await
            .unwrap();
    }

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id.to_string()).unwrap()
    }

    /// `list` returns the workspace's members ordered by email, and is
    /// workspace-scoped (a sibling tenant's member never leaks).
    #[tokio::test]
    async fn list_returns_workspace_members_ordered_by_email() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_member(pool, "ws-a", "u-bob", "bob@x.io", "admin").await;
        seed_member(pool, "ws-a", "u-amy", "amy@x.io", "owner").await;
        // A member of another tenant must not leak into ws-a's list.
        seed_member(pool, "ws-b", "u-zed", "zed@x.io", "owner").await;

        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert_eq!(members.len(), 2, "only ws-a's members");
        assert_eq!(members[0].email, "amy@x.io", "ordered by email");
        assert_eq!(members[0].role, "owner");
        assert_eq!(members[1].email, "bob@x.io");
        assert_eq!(members[1].role, "admin");
    }

    /// An unknown workspace lists as empty, never an error.
    #[tokio::test]
    async fn list_unknown_workspace_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let members = MemberRepo::list(store.pool(), &ws("nope")).await.unwrap();
        assert!(members.is_empty());
    }

    /// A role change on a non-sole-owner member persists.
    #[tokio::test]
    async fn set_role_changes_an_admin_to_member() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_member(pool, "ws-a", "u-owner", "o@x.io", "owner").await;
        seed_member(pool, "ws-a", "u-bob", "bob@x.io", "admin").await;

        MemberRepo::set_role(pool, &ws("ws-a"), "u-bob", MemberRole::Member)
            .await
            .unwrap();
        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        let bob = members.iter().find(|m| m.user_id == "u-bob").unwrap();
        assert_eq!(bob.role, "member", "role persisted");
    }

    /// Demoting the SOLE owner is rejected (last-owner guard); the role is intact.
    #[tokio::test]
    async fn set_role_rejects_demoting_the_only_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_member(pool, "ws-a", "u-owner", "o@x.io", "owner").await;
        seed_member(pool, "ws-a", "u-bob", "bob@x.io", "member").await;

        let err = MemberRepo::set_role(pool, &ws("ws-a"), "u-owner", MemberRole::Admin)
            .await
            .unwrap_err();
        assert!(matches!(err, MemberRepoError::LastOwner), "got {err:?}");
        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        let owner = members.iter().find(|m| m.user_id == "u-owner").unwrap();
        assert_eq!(owner.role, "owner", "rejected demotion left the role");
    }

    /// Demoting one of TWO owners is allowed (the workspace keeps an owner).
    #[tokio::test]
    async fn set_role_allows_demoting_one_of_two_owners() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_member(pool, "ws-a", "u-amy", "amy@x.io", "owner").await;
        seed_member(pool, "ws-a", "u-bob", "bob@x.io", "owner").await;

        MemberRepo::set_role(pool, &ws("ws-a"), "u-bob", MemberRole::Admin)
            .await
            .unwrap();
        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        let bob = members.iter().find(|m| m.user_id == "u-bob").unwrap();
        assert_eq!(bob.role, "admin");
    }

    /// A foreign-tenant member id cannot be edited through another workspace.
    #[tokio::test]
    async fn set_role_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_member(pool, "ws-b", "u-zed", "zed@x.io", "admin").await;

        let err = MemberRepo::set_role(pool, &ws("ws-a"), "u-zed", MemberRole::Member)
            .await
            .unwrap_err();
        assert!(matches!(err, MemberRepoError::NotFound), "got {err:?}");
    }

    /// Removing a non-owner member drops the join; the user row survives.
    #[tokio::test]
    async fn remove_drops_a_member() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_member(pool, "ws-a", "u-owner", "o@x.io", "owner").await;
        seed_member(pool, "ws-a", "u-bob", "bob@x.io", "member").await;

        MemberRepo::remove(pool, &ws("ws-a"), "u-bob").await.unwrap();
        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].user_id, "u-owner");
    }

    /// Removing the SOLE owner is rejected (last-owner guard); the member stays.
    #[tokio::test]
    async fn remove_rejects_removing_the_only_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_member(pool, "ws-a", "u-owner", "o@x.io", "owner").await;
        seed_member(pool, "ws-a", "u-bob", "bob@x.io", "member").await;

        let err = MemberRepo::remove(pool, &ws("ws-a"), "u-owner").await.unwrap_err();
        assert!(matches!(err, MemberRepoError::LastOwner), "got {err:?}");
        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert_eq!(members.len(), 2, "rejected removal left both members");
    }

    /// `add` mints a fresh user (by email) AND the membership when neither
    /// exists — the workspace's first *added* member.
    #[tokio::test]
    async fn add_mints_user_and_membership() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        let m = MemberRepo::add(pool, &ws("ws-a"), "dana@example.com", MemberRole::Member)
            .await
            .unwrap();
        assert_eq!(m.email, "dana@example.com");
        assert_eq!(m.role, "member");
        assert!(!m.user_id.is_empty(), "a user id was minted");

        // The user row exists and the membership is listed.
        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].email, "dana@example.com");
        assert_eq!(members[0].user_id, m.user_id);
    }

    /// Re-adding the SAME `(workspace, user)` pair is rejected as `AlreadyMember`
    /// — idempotent-safe, never a duplicate row.
    #[tokio::test]
    async fn add_existing_pair_is_already_member() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        MemberRepo::add(pool, &ws("ws-a"), "dana@example.com", MemberRole::Member)
            .await
            .unwrap();
        let err = MemberRepo::add(pool, &ws("ws-a"), "dana@example.com", MemberRole::Admin)
            .await
            .unwrap_err();
        assert!(matches!(err, MemberRepoError::AlreadyMember), "got {err:?}");

        // Still exactly one membership, and the original role is untouched.
        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(
            members[0].role, "member",
            "re-add did not overwrite the role"
        );
    }

    /// `add` reuses an EXISTING user (found by email) rather than minting a
    /// second user row — the same person joining a second workspace.
    #[tokio::test]
    async fn add_reuses_existing_user_by_email() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        // Dana already exists as a member of ws-a.
        let first = MemberRepo::add(pool, &ws("ws-a"), "dana@example.com", MemberRole::Member)
            .await
            .unwrap();

        let second = MemberRepo::add(pool, &ws("ws-b"), "dana@example.com", MemberRole::Admin)
            .await
            .unwrap();
        assert_eq!(
            second.user_id, first.user_id,
            "the same email reuses the one user row"
        );
        // Exactly one `user` row for that email.
        let user_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user WHERE email = 'dana@example.com'")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(user_count, 1, "no duplicate user minted");
    }

    /// `add` is workspace-scoped: the same email added to two sibling tenants is
    /// two separate memberships (one shared user), each with its own role.
    #[tokio::test]
    async fn add_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;

        MemberRepo::add(pool, &ws("ws-a"), "dana@example.com", MemberRole::Member)
            .await
            .unwrap();
        MemberRepo::add(pool, &ws("ws-b"), "dana@example.com", MemberRole::Admin)
            .await
            .unwrap();

        let a = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        let b = MemberRepo::list(pool, &ws("ws-b")).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].role, "member", "ws-a keeps its own role");
        assert_eq!(b[0].role, "admin", "ws-b keeps its own role");
        assert_eq!(a[0].user_id, b[0].user_id, "one shared user across tenants");
    }

    /// A blank email is rejected before any write.
    #[tokio::test]
    async fn add_rejects_empty_email() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        let err = MemberRepo::add(pool, &ws("ws-a"), "   ", MemberRole::Member).await.unwrap_err();
        assert!(matches!(err, MemberRepoError::EmptyEmail), "got {err:?}");
        let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert!(members.is_empty(), "nothing written on a blank email");
    }

    /// `MemberRole::parse` round-trips the closed set and rejects junk.
    #[test]
    fn role_parse_round_trips_and_rejects_junk() {
        for role in [MemberRole::Owner, MemberRole::Admin, MemberRole::Member] {
            assert_eq!(MemberRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(MemberRole::parse("superuser"), None);
        assert_eq!(MemberRole::parse(""), None);
    }
}
