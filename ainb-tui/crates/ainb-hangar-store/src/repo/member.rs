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
async fn member_role_in_tx(
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
