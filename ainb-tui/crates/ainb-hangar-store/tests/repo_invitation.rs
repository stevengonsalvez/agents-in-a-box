//! Behavioural coverage for the workspace membership INVITE lifecycle
//! (multica parity #18) against a REAL migrated sqlite store.
//!
//! `invite_then_accept_adds_member` is the item's stated acceptance test: an
//! invite can be created and accepted, and the acceptance is what adds the
//! member row. Everything else pins the gates multica enforces — the 7-day
//! window, one live pending invite per (workspace, email), the ownership check,
//! never inviting as `owner`, and workspace-scoped revocation.

use ainb_hangar_core::clock::{FixedClock, SystemClock};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::invitation::{INVITE_TTL_MS, InvitationRepo, InvitationRepoError};
use ainb_hangar_store::repo::member::{MemberRepo, MemberRepoError, MemberRole};
use sqlx::SqlitePool;

/// Epoch-ms "now" every fixed-clock test starts from.
const T0: i64 = 1_800_000_000_000;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::from_str(id.to_string()).unwrap()
}

async fn open_store(dir: &tempfile::TempDir) -> Store {
    Store::open_in(dir.path()).await.unwrap()
}

/// A workspace with exactly one owner (`u-owner` / `owner@x.io`) — the shape a
/// bootstrapped hangar install has.
async fn seed_workspace_with_owner(pool: &SqlitePool, id: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 1000)")
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    let user = format!("u-owner-{id}");
    let email = format!("owner-{id}@x.io");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, 1000)")
        .bind(&user)
        .bind(&email)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(id)
        .bind(&user)
        .execute(pool)
        .await
        .unwrap();
}

fn owner_of(id: &str) -> String {
    format!("u-owner-{id}")
}

async fn member_count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM member").fetch_one(pool).await.unwrap()
}

async fn status_of(pool: &SqlitePool, id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM workspace_invitation WHERE id = ?")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// THE ACCEPTANCE TEST: an invite can be created and accepted, and accepting is
/// what adds the member. The email is normalised on the way in, so the invitee
/// accepting with the lowercase form is recognised as the same person.
#[tokio::test]
async fn invite_then_accept_adds_member() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;

    let inv = InvitationRepo::create(
        pool,
        &SystemClock,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "  Dana@Example.com ",
        MemberRole::Member,
    )
    .await
    .unwrap();

    // An invitation is NOT a membership: still just the owner.
    assert_eq!(member_count(pool).await, 1, "invite alone adds no member");
    assert_eq!(inv.status, "pending");
    assert_eq!(
        inv.invitee_email, "dana@example.com",
        "email normalised (trim + lowercase) like multica"
    );
    assert!(
        inv.invitee_user_id.is_none(),
        "no stub user is minted at invite time"
    );

    let member = InvitationRepo::accept(pool, &SystemClock, &inv.id, "dana@example.com")
        .await
        .unwrap();
    assert_eq!(member.email, "dana@example.com");
    assert_eq!(member.role, "member");

    assert_eq!(member_count(pool).await, 2, "accept created the membership");
    let members = MemberRepo::list(pool, &ws("ws-a")).await.unwrap();
    let dana = members.iter().find(|m| m.email == "dana@example.com").unwrap();
    assert_eq!(dana.role, "member");

    let stored = InvitationRepo::get(pool, &inv.id).await.unwrap().unwrap();
    assert_eq!(stored.status, "accepted");
    assert_eq!(
        stored.invitee_user_id.as_deref(),
        Some(dana.user_id.as_str()),
        "invitee_user_id backfilled with the resolved user"
    );
}

/// The window is exactly 7 days from issue.
#[tokio::test]
async fn expiry_is_seven_days() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;

    let inv = InvitationRepo::create(
        pool,
        &FixedClock(T0),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();
    assert_eq!(inv.created_at, T0);
    assert_eq!(inv.expires_at - inv.created_at, INVITE_TTL_MS);
}

/// One millisecond past the window the invite is refused AND self-heals to
/// `expired`, so the row converges without waiting for a sweep.
#[tokio::test]
async fn expired_invite_cannot_be_accepted_and_self_expires() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;

    let inv = InvitationRepo::create(
        pool,
        &FixedClock(T0),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();

    let err = InvitationRepo::accept(
        pool,
        &FixedClock(T0 + INVITE_TTL_MS + 1),
        &inv.id,
        "dana@example.com",
    )
    .await
    .unwrap_err();
    assert!(matches!(err, InvitationRepoError::Expired), "got {err:?}");
    assert_eq!(
        member_count(pool).await,
        1,
        "no member from an expired invite"
    );
    assert_eq!(status_of(pool, &inv.id).await, "expired", "row self-healed");
}

/// The stale sweep is what unblocks the partial unique index after expiry
/// (multica issue #2055). Drop the sweep in `create` and this goes red with a
/// unique violation.
#[tokio::test]
async fn re_invite_after_expiry_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;

    let first = InvitationRepo::create(
        pool,
        &FixedClock(T0),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();

    let later = FixedClock(T0 + INVITE_TTL_MS + 1);
    let second = InvitationRepo::create(
        pool,
        &later,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .expect("the stale pending row was swept, so the re-invite fits the partial index");

    assert_ne!(second.id, first.id);
    assert_eq!(status_of(pool, &first.id).await, "expired");
    assert_eq!(status_of(pool, &second.id).await, "pending");
    // Only the live one lists.
    let pending = InvitationRepo::list_pending(pool, &later, &ws("ws-a")).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, second.id);
}

/// A second LIVE pending invite for the same (workspace, email) is refused.
#[tokio::test]
async fn second_live_pending_invite_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;

    InvitationRepo::create(
        pool,
        &FixedClock(T0),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();
    let err = InvitationRepo::create(
        pool,
        &FixedClock(T0 + 1),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "DANA@example.com",
        MemberRole::Admin,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, InvitationRepoError::AlreadyPending),
        "got {err:?}"
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_invitation")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "the rejected invite wrote nothing");
}

/// The unique index is status-scoped, not email-scoped: a declined invite never
/// blocks a fresh one.
#[tokio::test]
async fn re_invite_after_decline_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;
    let clock = FixedClock(T0);

    let first = InvitationRepo::create(
        pool,
        &clock,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();
    InvitationRepo::decline(pool, &clock, &first.id, "dana@example.com")
        .await
        .unwrap();

    InvitationRepo::create(
        pool,
        &clock,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .expect("a declined invite does not block a re-invite");
    assert_eq!(status_of(pool, &first.id).await, "declined");
}

/// Someone else's invitation cannot be accepted — and nothing is written.
#[tokio::test]
async fn accept_by_a_different_email_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;
    let clock = FixedClock(T0);

    let inv = InvitationRepo::create(
        pool,
        &clock,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();

    let err = InvitationRepo::accept(pool, &clock, &inv.id, "eve@example.com")
        .await
        .unwrap_err();
    assert!(matches!(err, InvitationRepoError::NotYours), "got {err:?}");
    assert_eq!(
        member_count(pool).await,
        1,
        "no member from a foreign accept"
    );
    assert_eq!(status_of(pool, &inv.id).await, "pending", "row untouched");
}

/// Declining closes the invite without creating a member.
#[tokio::test]
async fn decline_does_not_add_member() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;
    let clock = FixedClock(T0);

    let inv = InvitationRepo::create(
        pool,
        &clock,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();
    InvitationRepo::decline(pool, &clock, &inv.id, "Dana@Example.com ")
        .await
        .unwrap();

    assert_eq!(status_of(pool, &inv.id).await, "declined");
    assert_eq!(member_count(pool).await, 1);
    assert!(
        InvitationRepo::list_pending(pool, &clock, &ws("ws-a"))
            .await
            .unwrap()
            .is_empty()
    );
}

/// An accepted invitation is spent: a second accept is refused and no second
/// membership attempt happens.
#[tokio::test]
async fn accepted_invite_cannot_be_accepted_twice() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;
    let clock = FixedClock(T0);

    let inv = InvitationRepo::create(
        pool,
        &clock,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();
    InvitationRepo::accept(pool, &clock, &inv.id, "dana@example.com").await.unwrap();

    let err = InvitationRepo::accept(pool, &clock, &inv.id, "dana@example.com")
        .await
        .unwrap_err();
    assert!(
        matches!(err, InvitationRepoError::NotPending),
        "got {err:?}"
    );
    assert_eq!(member_count(pool).await, 2, "still exactly one new member");
}

/// Ownership is transferred, never invited (multica: "cannot invite as owner").
#[tokio::test]
async fn cannot_invite_as_owner() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;

    let err = InvitationRepo::create(
        pool,
        &FixedClock(T0),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Owner,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, InvitationRepoError::CannotInviteOwner),
        "got {err:?}"
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace_invitation")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

/// Inviting someone who is already a member is refused up-front.
#[tokio::test]
async fn cannot_invite_an_existing_member() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;
    MemberRepo::add(pool, &ws("ws-a"), "dana@example.com", MemberRole::Member)
        .await
        .unwrap();

    let err = InvitationRepo::create(
        pool,
        &FixedClock(T0),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "Dana@Example.com",
        MemberRole::Member,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, InvitationRepoError::AlreadyMember),
        "got {err:?}"
    );
}

/// Only a member of the workspace may invite into it.
#[tokio::test]
async fn inviter_must_be_a_member() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;
    seed_workspace_with_owner(pool, "ws-b").await;

    // ws-b's owner is a stranger to ws-a.
    let err = InvitationRepo::create(
        pool,
        &FixedClock(T0),
        &ws("ws-a"),
        &owner_of("ws-b"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, InvitationRepoError::InviterNotMember),
        "got {err:?}"
    );
}

/// Revoke deletes only a PENDING row and only within its own workspace.
#[tokio::test]
async fn revoke_only_touches_pending_and_is_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;
    seed_workspace_with_owner(pool, "ws-b").await;
    let clock = FixedClock(T0);

    let inv = InvitationRepo::create(
        pool,
        &clock,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();

    // A sibling tenant cannot revoke it.
    let err = InvitationRepo::revoke(pool, &ws("ws-b"), &inv.id).await.unwrap_err();
    assert!(matches!(err, InvitationRepoError::NotFound), "got {err:?}");
    assert_eq!(status_of(pool, &inv.id).await, "pending", "row untouched");

    // Its own workspace can.
    InvitationRepo::revoke(pool, &ws("ws-a"), &inv.id).await.unwrap();
    assert!(InvitationRepo::get(pool, &inv.id).await.unwrap().is_none());

    // A second revoke has nothing left to delete.
    let err = InvitationRepo::revoke(pool, &ws("ws-a"), &inv.id).await.unwrap_err();
    assert!(matches!(err, InvitationRepoError::NotFound), "got {err:?}");
}

/// The workspace-wide sweep converges past-due rows without a create, and the
/// pending list already hides them before it runs.
#[tokio::test]
async fn expire_stale_sweeps_past_due_rows() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;

    let old = InvitationRepo::create(
        pool,
        &FixedClock(T0),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "dana@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();
    let fresh = InvitationRepo::create(
        pool,
        &FixedClock(T0 + INVITE_TTL_MS),
        &ws("ws-a"),
        &owner_of("ws-a"),
        "eve@example.com",
        MemberRole::Admin,
    )
    .await
    .unwrap();

    let later = FixedClock(T0 + INVITE_TTL_MS + 1);
    // Already hidden from the list before any sweep.
    let pending = InvitationRepo::list_pending(pool, &later, &ws("ws-a")).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, fresh.id);

    let swept = InvitationRepo::expire_stale(pool, &later, &ws("ws-a")).await.unwrap();
    assert_eq!(swept, 1, "only the past-due row converged");
    assert_eq!(status_of(pool, &old.id).await, "expired");
    assert_eq!(status_of(pool, &fresh.id).await, "pending");
}

/// "My invites" spans workspaces and matches on the normalised email.
#[tokio::test]
async fn list_pending_for_email_is_cross_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;
    seed_workspace_with_owner(pool, "ws-b").await;
    let clock = FixedClock(T0);

    for w in ["ws-a", "ws-b"] {
        InvitationRepo::create(
            pool,
            &clock,
            &ws(w),
            &owner_of(w),
            "dana@example.com",
            MemberRole::Member,
        )
        .await
        .unwrap();
    }
    InvitationRepo::create(
        pool,
        &clock,
        &ws("ws-a"),
        &owner_of("ws-a"),
        "eve@example.com",
        MemberRole::Member,
    )
    .await
    .unwrap();

    let mine = InvitationRepo::list_pending_for_email(pool, &clock, " Dana@Example.com ")
        .await
        .unwrap();
    assert_eq!(mine.len(), 2, "both workspaces' invites, and not eve's");
    let mut workspaces: Vec<&str> = mine.iter().map(|i| i.workspace_id.as_str()).collect();
    workspaces.sort_unstable();
    assert_eq!(workspaces, vec!["ws-a", "ws-b"]);
}

/// `MemberRepo::add` still behaves exactly as before the `add_in_tx` extraction
/// (the refactor is behaviour-preserving).
#[tokio::test]
async fn member_add_still_commits_and_reports_already_member() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(&dir).await;
    let pool = store.pool();
    seed_workspace_with_owner(pool, "ws-a").await;

    MemberRepo::add(pool, &ws("ws-a"), "dana@example.com", MemberRole::Member)
        .await
        .unwrap();
    assert_eq!(member_count(pool).await, 2, "add committed");
    let err = MemberRepo::add(pool, &ws("ws-a"), "dana@example.com", MemberRole::Member)
        .await
        .unwrap_err();
    assert!(matches!(err, MemberRepoError::AlreadyMember), "got {err:?}");
}
