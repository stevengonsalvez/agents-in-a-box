//! Typed repository wrapper over the `issue_subscriber` table (multica parity
//! #22, migration 0062).
//!
//! A subscription is a SET MEMBERSHIP: `(issue, actor)` is the primary key and
//! [`SubscribeReason`] is provenance, not state. Every add is
//! `INSERT OR IGNORE`, so the FIRST reason wins — an actor who created an issue
//! and later comments on it stays `creator`, exactly as the reference's
//! `AddIssueSubscriber` (`pkg/db/queries/subscriber.sql:1-4`).
//!
//! # Workspace scoping without a `workspace_id` column
//!
//! Like [`super::comment`], the table carries no `workspace_id` — a subscription
//! belongs to an issue, and the issue carries the workspace. Both the add and
//! the remove are therefore **scoped through a join to `issue`**: they only land
//! when `(issue_id, workspace_id)` resolves to a real issue in that tenant, and
//! a foreign tenant's write returns `Ok(false)` rather than erroring.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use sqlx::{Row, SqlitePool};

/// Why an actor is subscribed — PROVENANCE, not state (see migration 0062).
///
/// The column has no `CHECK` (SQLite cannot widen one without a table rebuild),
/// so this enum is the vocabulary's only enforcement: [`Self::as_db_str`] is the
/// single writer and [`Self::parse`] is a tolerant reader that yields `None` for
/// a token written by a newer build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubscribeReason {
    /// The actor created the issue.
    Creator,
    /// The actor is (or was) the issue's assignee.
    Assignee,
    /// The actor commented on the issue.
    Commenter,
    /// The actor was `@`-mentioned in a comment.
    Mentioned,
    /// The actor explicitly asked to watch the issue.
    Manual,
    /// The actor is a standing SUBSCRIBER of the autopilot that SPAWNED this
    /// issue (multica parity #27, migration 0064) — they never touched this
    /// occurrence, they follow the recurring rule behind it.
    ///
    /// Appending here needs no migration precisely because 0062 decision 3 put
    /// the vocabulary in Rust with a tolerant parser and NO SQL `CHECK`.
    Autopilot,
}

impl SubscribeReason {
    /// The token stored in `issue_subscriber.reason`. The ONLY writer of that
    /// column.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Creator => "creator",
            Self::Assignee => "assignee",
            Self::Commenter => "commenter",
            Self::Mentioned => "mentioned",
            Self::Manual => "manual",
            Self::Autopilot => "autopilot",
        }
    }

    /// Tolerant reader: an unknown token (a newer build's vocabulary) yields
    /// `None` rather than failing the whole read.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "creator" => Some(Self::Creator),
            "assignee" => Some(Self::Assignee),
            "commenter" => Some(Self::Commenter),
            "mentioned" => Some(Self::Mentioned),
            "manual" => Some(Self::Manual),
            "autopilot" => Some(Self::Autopilot),
            _ => None,
        }
    }
}

/// One materialised `issue_subscriber` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSubscriber {
    /// The issue watched.
    pub issue_id: String,
    /// The watching actor, re-assembled from its two TEXT columns.
    pub actor: ActorRef,
    /// The parsed provenance, `None` for a token this build does not know.
    pub reason: Option<SubscribeReason>,
    /// The raw stored token, always available for rendering.
    pub reason_raw: String,
    /// When the subscription was created (epoch millis).
    pub created_at: i64,
}

/// Stateless typed wrapper over the `issue_subscriber` table.
pub struct IssueSubscriberRepo;

impl IssueSubscriberRepo {
    /// Idempotently subscribe `actor` to `issue_id`, workspace-scoped through
    /// the join to `issue` ([`super::comment::CommentRepo::insert`]'s pattern).
    ///
    /// Returns `Ok(true)` when a row landed, `Ok(false)` when it did not —
    /// either because the `(issue, workspace)` pair resolves to nothing (a
    /// foreign tenant writes nothing rather than erroring) or because the actor
    /// was ALREADY subscribed, in which case the existing row keeps its
    /// ORIGINAL reason (first-reason-wins).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement fails.
    pub async fn add(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
        actor: &ActorRef,
        reason: SubscribeReason,
        now_ms: i64,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "INSERT OR IGNORE INTO issue_subscriber \
                 (issue_id, actor_type, actor_id, reason, created_at) \
             SELECT ?, ?, ?, ?, ? \
             WHERE EXISTS (SELECT 1 FROM issue WHERE id = ? AND workspace_id = ?)",
        )
        .bind(issue_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(reason.as_db_str())
        .bind(now_ms)
        .bind(issue_id)
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Unsubscribe `actor` from `issue_id`, workspace-scoped the same way.
    ///
    /// Removing an absent subscription is an idempotent `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the statement fails.
    pub async fn remove(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
        actor: &ActorRef,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM issue_subscriber \
             WHERE issue_id = ? AND actor_type = ? AND actor_id = ? \
               AND EXISTS (SELECT 1 FROM issue WHERE id = ? AND workspace_id = ?)",
        )
        .bind(issue_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(issue_id)
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Every subscriber of one issue, oldest first.
    ///
    /// Ordered by `created_at`, then `actor_type`, `actor_id` — the reference's
    /// bare `ORDER BY created_at` is non-deterministic within a millisecond.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list(
        pool: &SqlitePool,
        issue_id: &str,
    ) -> Result<Vec<IssueSubscriber>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT issue_id, actor_type, actor_id, reason, created_at \
             FROM issue_subscriber WHERE issue_id = ? \
             ORDER BY created_at, actor_type, actor_id",
        )
        .bind(issue_id)
        .fetch_all(pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                let kind: String = r.get("actor_type");
                let id: String = r.get("actor_id");
                let actor = ActorRef::new(kind.parse::<ActorKind>().ok()?, id).ok()?;
                let reason_raw: String = r.get("reason");
                Some(IssueSubscriber {
                    issue_id: r.get("issue_id"),
                    actor,
                    reason: SubscribeReason::parse(&reason_raw),
                    reason_raw,
                    created_at: r.get("created_at"),
                })
            })
            .collect())
    }

    /// Whether `actor` currently watches `issue_id`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn is_subscribed(
        pool: &SqlitePool,
        issue_id: &str,
        actor: &ActorRef,
    ) -> Result<bool, sqlx::Error> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM issue_subscriber \
             WHERE issue_id = ? AND actor_type = ? AND actor_id = ?",
        )
        .bind(issue_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .fetch_one(pool)
        .await?;
        Ok(n > 0)
    }

    /// How many actors watch `issue_id`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn count(pool: &SqlitePool, issue_id: &str) -> Result<u32, sqlx::Error> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM issue_subscriber WHERE issue_id = ?")
            .bind(issue_id)
            .fetch_one(pool)
            .await?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// The inbox aggregator's read: just the watching actors, no provenance.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn actors(pool: &SqlitePool, issue_id: &str) -> Result<Vec<ActorRef>, sqlx::Error> {
        Ok(Self::list(pool, issue_id).await?.into_iter().map(|s| s.actor).collect())
    }
}
