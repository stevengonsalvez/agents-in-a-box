//! Typed repository wrapper over `workspace_invitation` (multica parity #18).
//!
//! Reference: multica `server/migrations/041_workspace_invitation.up.sql`,
//! `server/pkg/db/queries/invitation.sql`, `server/internal/handler/invitation.go`.
//!
//! Before migration 0063 the only way to add a human was
//! [`MemberRepo::add`](crate::repo::member::MemberRepo::add) — the *instant join*
//! multica's `invitation.go` says `CreateInvitation` replaces. This repo adds the
//! pending state between "someone was invited" and "someone is a member":
//!
//! ```text
//!            create                accept
//!   (none) ──────────▶ pending ──────────────▶ accepted  (+ member row)
//!                        │  │
//!               decline  │  │  expires_at <= now
//!                        ▼  ▼
//!                   declined  expired
//! ```
//!
//! # One live pending invite per (workspace, email)
//!
//! `idx_invitation_unique_pending` is a PARTIAL unique index over
//! `(workspace_id, invitee_email) WHERE status = 'pending'`, so an
//! accepted / declined / expired row never blocks a re-invite. A partial index
//! cannot reference `now()`, though, so a *past-due* `pending` row would block
//! the re-invite forever (multica issue #2055). [`create`](InvitationRepo::create)
//! therefore sweeps stale pending rows to `expired` FIRST, inside the same
//! transaction — dropping that sweep makes `re_invite_after_expiry_succeeds` fail
//! with a unique violation.
//!
//! # Epoch-ms instead of `INTERVAL '7 days'`
//!
//! `SQLite` has no temporal type, so every Hangar timestamp is an epoch-ms
//! `INTEGER` and the reference's `DEFAULT now() + INTERVAL '7 days'` is computed
//! in Rust as <code>now + [INVITE_TTL_MS]</code>. Every method takes a
//! [`HangarClock`] so the expiry paths are deterministic under `FixedClock`.
//!
//! # Accept is ONE transaction
//!
//! [`accept`](InvitationRepo::accept) flips the invitation and inserts the
//! membership in a single transaction (multica uses a `qtx` for exactly this):
//! a crash between the two would otherwise leave an `accepted` invitation with
//! no member, and nothing could ever re-accept it. The status flip is a guarded
//! `UPDATE ... WHERE id = ? AND status = 'pending'` so two concurrent accepts
//! cannot both create a member.
//!
//! # Workspace scoping
//!
//! Listing and [`revoke`](InvitationRepo::revoke) are workspace-scoped in SQL, so
//! a foreign tenant's invitation resolves to no row — a
//! [`InvitationRepoError::NotFound`], never a cross-tenant delete.

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::WorkspaceId;
use sqlx::{Row, SqlitePool};

use crate::repo::member::{Member, MemberRepo, MemberRole, member_role_in_tx};

/// How long a pending invitation stays live: 7 days, in milliseconds.
///
/// The reference expresses this as the column DEFAULT `now() + INTERVAL '7
/// days'`; `SQLite` cannot, so it is applied in Rust at insert time.
pub const INVITE_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// One `workspace_invitation` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invitation {
    /// ULID primary key.
    pub id: String,
    /// The workspace the invitee is being invited to.
    pub workspace_id: String,
    /// `user.id` of whoever issued the invite.
    pub inviter_id: String,
    /// The invitee's email, always stored normalised (trimmed + lowercased).
    pub invitee_email: String,
    /// `user.id` of the invitee once one is known — set at create time when a
    /// user with that email already exists, otherwise backfilled on accept.
    pub invitee_user_id: Option<String>,
    /// The role the invitee will hold: `admin` or `member` (never `owner`).
    pub role: String,
    /// `pending` | `accepted` | `declined` | `expired`.
    pub status: String,
    /// Epoch-ms the invite was issued.
    pub created_at: i64,
    /// Epoch-ms of the last status change.
    pub updated_at: i64,
    /// Epoch-ms after which the invite can no longer be accepted.
    pub expires_at: i64,
}

/// Normalise an email the way multica does (`strings.ToLower(strings.TrimSpace)`).
///
/// Applied on create AND on every ownership comparison, so `"Dana@Example.com "`
/// invited and `"dana@example.com"` accepting are the same person.
#[must_use]
pub fn normalize_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// Stateless typed wrapper over the `workspace_invitation` table.
pub struct InvitationRepo;

impl InvitationRepo {
    /// Invite `email` into `workspace` as `role`, issued by `inviter_user_id`.
    ///
    /// Mirrors multica's `CreateInvitation`: the inviter must already be a member
    /// of the workspace; `owner` may never be invited; an email that already
    /// belongs to a member is rejected; stale pending rows are swept to `expired`
    /// before the live-pending check so a re-invite after expiry works.
    ///
    /// `invitee_user_id` is set only when a `user` with that email already
    /// exists — no stub user is minted at invite time (that is
    /// [`accept`](InvitationRepo::accept)'s job).
    ///
    /// # Errors
    ///
    /// [`InvitationRepoError::EmptyEmail`], [`InvitationRepoError::CannotInviteOwner`],
    /// [`InvitationRepoError::InviterNotMember`], [`InvitationRepoError::AlreadyMember`],
    /// [`InvitationRepoError::AlreadyPending`], or [`InvitationRepoError::Db`].
    pub async fn create(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
        inviter_user_id: &str,
        email: &str,
        role: MemberRole,
    ) -> Result<Invitation, InvitationRepoError> {
        let email = normalize_email(email);
        if email.is_empty() {
            return Err(InvitationRepoError::EmptyEmail);
        }
        // multica: "cannot invite as owner" — ownership is transferred, never handed out.
        if role == MemberRole::Owner {
            return Err(InvitationRepoError::CannotInviteOwner);
        }
        let now = clock.now_ms();

        let mut tx = pool.begin().await?;

        // (1) The inviter must be a member of this workspace.
        if member_role_in_tx(&mut tx, workspace, inviter_user_id).await?.is_none() {
            return Err(InvitationRepoError::InviterNotMember);
        }

        // (2) If that email already has an account, it must not already be a member.
        let invitee_user_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM user WHERE email = ?")
                .bind(&email)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(uid) = invitee_user_id.as_deref() {
            if member_role_in_tx(&mut tx, workspace, uid).await?.is_some() {
                return Err(InvitationRepoError::AlreadyMember);
            }
        }

        // (3) Sweep past-due pending rows for this (workspace, email) — the partial
        //     unique index cannot reference `now`, so a stale row would block us.
        sqlx::query(
            "UPDATE workspace_invitation SET status = 'expired', updated_at = ? \
             WHERE workspace_id = ? AND invitee_email = ? AND status = 'pending' \
               AND expires_at <= ?",
        )
        .bind(now)
        .bind(workspace.as_str())
        .bind(&email)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        // (4) A LIVE pending invite blocks a second one.
        let live: Option<String> = sqlx::query_scalar(
            "SELECT id FROM workspace_invitation \
             WHERE workspace_id = ? AND invitee_email = ? AND status = 'pending' \
               AND expires_at > ?",
        )
        .bind(workspace.as_str())
        .bind(&email)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        if live.is_some() {
            return Err(InvitationRepoError::AlreadyPending);
        }

        let invitation = Invitation {
            id: SystemIdGen.new_ulid(),
            workspace_id: workspace.as_str().to_string(),
            inviter_id: inviter_user_id.to_string(),
            invitee_email: email,
            invitee_user_id,
            role: role.as_str().to_string(),
            status: "pending".to_string(),
            created_at: now,
            updated_at: now,
            expires_at: now + INVITE_TTL_MS,
        };
        let inserted = sqlx::query(
            "INSERT INTO workspace_invitation \
             (id, workspace_id, inviter_id, invitee_email, invitee_user_id, role, status, \
              created_at, updated_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?)",
        )
        .bind(&invitation.id)
        .bind(&invitation.workspace_id)
        .bind(&invitation.inviter_id)
        .bind(&invitation.invitee_email)
        .bind(invitation.invitee_user_id.as_deref())
        .bind(&invitation.role)
        .bind(invitation.created_at)
        .bind(invitation.updated_at)
        .bind(invitation.expires_at)
        .execute(&mut *tx)
        .await;
        if let Err(e) = inserted {
            // The partial unique index is the race-safe backstop for step (4).
            if e.as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
            {
                return Err(InvitationRepoError::AlreadyPending);
            }
            return Err(InvitationRepoError::Db(e));
        }
        tx.commit().await?;
        Ok(invitation)
    }

    /// Accept invitation `invitation_id` as `actor_email`, creating the membership.
    ///
    /// The status flip and the member insert share ONE transaction. A past-due
    /// invite self-heals: the row is flipped to `expired` (committed) and
    /// [`InvitationRepoError::Expired`] is returned, so an abandoned invite
    /// converges without waiting for a sweep.
    ///
    /// # Errors
    ///
    /// [`InvitationRepoError::NotFound`], [`InvitationRepoError::NotYours`],
    /// [`InvitationRepoError::NotPending`], [`InvitationRepoError::Expired`],
    /// [`InvitationRepoError::AlreadyMember`], or [`InvitationRepoError::Db`].
    pub async fn accept(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        invitation_id: &str,
        actor_email: &str,
    ) -> Result<Member, InvitationRepoError> {
        let actor_email = normalize_email(actor_email);
        let now = clock.now_ms();

        let mut tx = pool.begin().await?;
        let invitation = fetch_in_tx(&mut tx, invitation_id)
            .await?
            .ok_or(InvitationRepoError::NotFound)?;
        if invitation.invitee_email != actor_email {
            return Err(InvitationRepoError::NotYours);
        }
        if invitation.status != "pending" {
            return Err(InvitationRepoError::NotPending);
        }
        if invitation.expires_at <= now {
            // Self-healing: converge the row, COMMIT it, then report the refusal.
            sqlx::query(
                "UPDATE workspace_invitation SET status = 'expired', updated_at = ? \
                 WHERE id = ? AND status = 'pending'",
            )
            .bind(now)
            .bind(invitation_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Err(InvitationRepoError::Expired);
        }

        // Guarded flip: 0 rows means someone else already acted on it (race-safe).
        let flipped = sqlx::query(
            "UPDATE workspace_invitation SET status = 'accepted', updated_at = ? \
             WHERE id = ? AND status = 'pending'",
        )
        .bind(now)
        .bind(invitation_id)
        .execute(&mut *tx)
        .await?;
        if flipped.rows_affected() == 0 {
            return Err(InvitationRepoError::NotPending);
        }

        let workspace = WorkspaceId::from_str(invitation.workspace_id.clone())
            .map_err(|_| InvitationRepoError::NotFound)?;
        let role = MemberRole::parse(&invitation.role).ok_or(InvitationRepoError::InvalidRole)?;
        let member =
            MemberRepo::add_in_tx(&mut tx, &workspace, &invitation.invitee_email, role).await?;

        // Backfill the invitee's now-known user id.
        sqlx::query("UPDATE workspace_invitation SET invitee_user_id = ? WHERE id = ?")
            .bind(&member.user_id)
            .bind(invitation_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(member)
    }

    /// Decline invitation `invitation_id` as `actor_email`. No member is created.
    ///
    /// # Errors
    ///
    /// [`InvitationRepoError::NotFound`], [`InvitationRepoError::NotYours`],
    /// [`InvitationRepoError::NotPending`], [`InvitationRepoError::Expired`], or
    /// [`InvitationRepoError::Db`].
    pub async fn decline(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        invitation_id: &str,
        actor_email: &str,
    ) -> Result<(), InvitationRepoError> {
        let actor_email = normalize_email(actor_email);
        let now = clock.now_ms();

        let mut tx = pool.begin().await?;
        let invitation = fetch_in_tx(&mut tx, invitation_id)
            .await?
            .ok_or(InvitationRepoError::NotFound)?;
        if invitation.invitee_email != actor_email {
            return Err(InvitationRepoError::NotYours);
        }
        if invitation.status != "pending" {
            return Err(InvitationRepoError::NotPending);
        }
        if invitation.expires_at <= now {
            sqlx::query(
                "UPDATE workspace_invitation SET status = 'expired', updated_at = ? \
                 WHERE id = ? AND status = 'pending'",
            )
            .bind(now)
            .bind(invitation_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Err(InvitationRepoError::Expired);
        }
        let flipped = sqlx::query(
            "UPDATE workspace_invitation SET status = 'declined', updated_at = ? \
             WHERE id = ? AND status = 'pending'",
        )
        .bind(now)
        .bind(invitation_id)
        .execute(&mut *tx)
        .await?;
        if flipped.rows_affected() == 0 {
            return Err(InvitationRepoError::NotPending);
        }
        tx.commit().await?;
        Ok(())
    }

    /// Revoke (delete) a still-pending invitation — the admin-side withdrawal.
    ///
    /// Workspace-scoped in SQL: another tenant's invitation matches no row and is
    /// reported as [`InvitationRepoError::NotFound`], never deleted.
    ///
    /// # Errors
    ///
    /// [`InvitationRepoError::NotFound`] when no pending row matches
    /// `(id, workspace)`, or [`InvitationRepoError::Db`].
    pub async fn revoke(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        invitation_id: &str,
    ) -> Result<(), InvitationRepoError> {
        let deleted = sqlx::query(
            "DELETE FROM workspace_invitation \
             WHERE id = ? AND workspace_id = ? AND status = 'pending'",
        )
        .bind(invitation_id)
        .bind(workspace.as_str())
        .execute(pool)
        .await?;
        if deleted.rows_affected() == 0 {
            return Err(InvitationRepoError::NotFound);
        }
        Ok(())
    }

    /// Every LIVE pending invitation of `workspace`, newest first.
    ///
    /// Filters `expires_at > now` like the reference's
    /// `ListPendingInvitationsByWorkspace`, so a past-due row never renders as
    /// pending even before a sweep runs.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_pending(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
    ) -> Result<Vec<Invitation>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT * FROM workspace_invitation \
             WHERE workspace_id = ? AND status = 'pending' AND expires_at > ? \
             ORDER BY created_at DESC, id DESC",
        )
        .bind(workspace.as_str())
        .bind(clock.now_ms())
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_to_invitation).collect()
    }

    /// Every LIVE pending invitation addressed to `email`, across workspaces
    /// (the reference's `ListPendingInvitationsForUser` — "my invites").
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_pending_for_email(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        email: &str,
    ) -> Result<Vec<Invitation>, sqlx::Error> {
        let email = normalize_email(email);
        let rows = sqlx::query(
            "SELECT i.* FROM workspace_invitation i \
             WHERE i.status = 'pending' AND i.expires_at > ? \
               AND (i.invitee_email = ? \
                    OR i.invitee_user_id IN (SELECT id FROM user WHERE email = ?)) \
             ORDER BY i.created_at DESC, i.id DESC",
        )
        .bind(clock.now_ms())
        .bind(&email)
        .bind(&email)
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_to_invitation).collect()
    }

    /// Fetch one invitation by id regardless of status (CLI printing, tests).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn get(
        pool: &SqlitePool,
        invitation_id: &str,
    ) -> Result<Option<Invitation>, sqlx::Error> {
        let row = sqlx::query("SELECT * FROM workspace_invitation WHERE id = ?")
            .bind(invitation_id)
            .fetch_optional(pool)
            .await?;
        row.as_ref().map(row_to_invitation).transpose()
    }

    /// Flip every past-due `pending` invitation of `workspace` to `expired`,
    /// returning how many rows converged.
    ///
    /// The daemon runs this before every members snapshot so status converges
    /// even when nobody creates a new invite.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn expire_stale(
        pool: &SqlitePool,
        clock: &dyn HangarClock,
        workspace: &WorkspaceId,
    ) -> Result<u64, sqlx::Error> {
        let now = clock.now_ms();
        let res = sqlx::query(
            "UPDATE workspace_invitation SET status = 'expired', updated_at = ? \
             WHERE workspace_id = ? AND status = 'pending' AND expires_at <= ?",
        )
        .bind(now)
        .bind(workspace.as_str())
        .bind(now)
        .execute(pool)
        .await?;
        Ok(res.rows_affected())
    }
}

/// Read one invitation inside a transaction (the ownership / status gate's source).
async fn fetch_in_tx(
    tx: &mut sqlx::SqliteConnection,
    invitation_id: &str,
) -> Result<Option<Invitation>, sqlx::Error> {
    let row = sqlx::query("SELECT * FROM workspace_invitation WHERE id = ?")
        .bind(invitation_id)
        .fetch_optional(&mut *tx)
        .await?;
    row.as_ref().map(row_to_invitation).transpose()
}

fn row_to_invitation(row: &sqlx::sqlite::SqliteRow) -> Result<Invitation, sqlx::Error> {
    Ok(Invitation {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        inviter_id: row.try_get("inviter_id")?,
        invitee_email: row.try_get("invitee_email")?,
        invitee_user_id: row.try_get("invitee_user_id")?,
        role: row.try_get("role")?,
        status: row.try_get("status")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        expires_at: row.try_get("expires_at")?,
    })
}

/// Error surface for [`InvitationRepo`], one variant per multica rejection.
#[derive(Debug, thiserror::Error)]
pub enum InvitationRepoError {
    /// The invitee email was blank after normalisation.
    #[error("email must not be empty")]
    EmptyEmail,
    /// The stored role token is outside `admin`/`member` (a corrupt row).
    #[error("invalid role: must be one of admin/member")]
    InvalidRole,
    /// `owner` was requested — multica: "cannot invite as owner".
    #[error("cannot invite as owner")]
    CannotInviteOwner,
    /// The inviter is not a member of the target workspace.
    #[error("only a workspace member can invite")]
    InviterNotMember,
    /// That email already belongs to a member of this workspace.
    #[error("user is already a member")]
    AlreadyMember,
    /// A live pending invitation for that (workspace, email) already exists.
    #[error("invitation already pending for this email")]
    AlreadyPending,
    /// No invitation matched the supplied id (covers a foreign tenant's id).
    #[error("invitation not found")]
    NotFound,
    /// The acting email is not the invitee.
    #[error("invitation does not belong to you")]
    NotYours,
    /// The invitation is no longer `pending` (already accepted/declined/expired).
    #[error("invitation is not pending")]
    NotPending,
    /// The invitation's 7-day window has closed.
    #[error("invitation has expired")]
    Expired,
    /// An underlying `sqlx` failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl From<crate::repo::member::MemberRepoError> for InvitationRepoError {
    fn from(e: crate::repo::member::MemberRepoError) -> Self {
        use crate::repo::member::MemberRepoError as M;
        match e {
            M::AlreadyMember => Self::AlreadyMember,
            M::EmptyEmail => Self::EmptyEmail,
            M::InvalidRole => Self::InvalidRole,
            M::Db(e) => Self::Db(e),
            M::NotFound | M::LastOwner => Self::NotFound,
        }
    }
}
