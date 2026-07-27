//! Typed repository wrapper over the `issue_reaction` table (multica parity
//! #22, migration 0062).
//!
//! An emoji reaction is unique per `(issue, actor, emoji)`: reacting twice is a
//! no-op, not an error (the reference answers `201` either way,
//! `internal/handler/issue_reaction.go`). A blank emoji is rejected at the REPO
//! boundary so every caller — RPC, CLI, plugin — inherits the reference's
//! "emoji is required" guard without restating it.
//!
//! Like [`super::issue_subscriber`], writes are workspace-scoped through the
//! join to `issue`: a foreign tenant lands nothing and gets `Ok(false)`.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use sqlx::{Row, SqlitePool};

/// Failure modes of the reaction repo.
#[derive(Debug, thiserror::Error)]
pub enum IssueReactionError {
    /// The caller passed an empty (or whitespace-only) emoji — the reference's
    /// `400 "emoji is required"`.
    #[error("emoji is required")]
    EmptyEmoji,
    /// A store fault.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// One materialised `issue_reaction` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueReaction {
    /// Primary key (ULID minted by the caller's `IdGen`).
    pub id: String,
    /// The issue reacted to.
    pub issue_id: String,
    /// The reacting actor.
    pub actor: ActorRef,
    /// The emoji itself (free text by nature).
    pub emoji: String,
    /// When the reaction landed (epoch millis).
    pub created_at: i64,
}

/// One aggregated emoji bucket, for a wire row or a render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionTally {
    /// The emoji this bucket counts.
    pub emoji: String,
    /// How many distinct actors used it.
    pub count: u32,
    /// Those actors, so a viewer can tell whether the bucket is theirs.
    pub actors: Vec<ActorRef>,
}

/// Stateless typed wrapper over the `issue_reaction` table.
pub struct IssueReactionRepo;

impl IssueReactionRepo {
    /// Idempotently add one reaction. `INSERT OR IGNORE` on the UNIQUE triple,
    /// so reacting twice lands nothing and answers `Ok(false)`.
    ///
    /// # Errors
    ///
    /// [`IssueReactionError::EmptyEmoji`] for a blank emoji;
    /// [`IssueReactionError::Db`] on a store fault.
    #[allow(clippy::too_many_arguments)]
    pub async fn add(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
        actor: &ActorRef,
        emoji: &str,
        id: &str,
        now_ms: i64,
    ) -> Result<bool, IssueReactionError> {
        if emoji.trim().is_empty() {
            return Err(IssueReactionError::EmptyEmoji);
        }
        let res = sqlx::query(
            "INSERT OR IGNORE INTO issue_reaction \
                 (id, issue_id, workspace_id, actor_type, actor_id, emoji, created_at) \
             SELECT ?, ?, ?, ?, ?, ?, ? \
             WHERE EXISTS (SELECT 1 FROM issue WHERE id = ? AND workspace_id = ?)",
        )
        .bind(id)
        .bind(issue_id)
        .bind(workspace_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(emoji)
        .bind(now_ms)
        .bind(issue_id)
        .bind(workspace_id)
        .execute(pool)
        .await
        .map_err(IssueReactionError::Db)?;
        Ok(res.rows_affected() == 1)
    }

    /// Remove one reaction. Removing an absent one is an idempotent
    /// `Ok(false)`.
    ///
    /// # Errors
    ///
    /// [`IssueReactionError::EmptyEmoji`] for a blank emoji;
    /// [`IssueReactionError::Db`] on a store fault.
    pub async fn remove(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
        actor: &ActorRef,
        emoji: &str,
    ) -> Result<bool, IssueReactionError> {
        if emoji.trim().is_empty() {
            return Err(IssueReactionError::EmptyEmoji);
        }
        let res = sqlx::query(
            "DELETE FROM issue_reaction \
             WHERE issue_id = ? AND workspace_id = ? AND actor_type = ? \
               AND actor_id = ? AND emoji = ?",
        )
        .bind(issue_id)
        .bind(workspace_id)
        .bind(actor.kind().as_str())
        .bind(actor.id())
        .bind(emoji)
        .execute(pool)
        .await
        .map_err(IssueReactionError::Db)?;
        Ok(res.rows_affected() == 1)
    }

    /// Every reaction on one issue, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list(
        pool: &SqlitePool,
        issue_id: &str,
    ) -> Result<Vec<IssueReaction>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, issue_id, actor_type, actor_id, emoji, created_at \
             FROM issue_reaction WHERE issue_id = ? \
             ORDER BY created_at, emoji, actor_type, actor_id",
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
                Some(IssueReaction {
                    id: r.get("id"),
                    issue_id: r.get("issue_id"),
                    actor,
                    emoji: r.get("emoji"),
                    created_at: r.get("created_at"),
                })
            })
            .collect())
    }

    /// Aggregated buckets, most-used first then emoji ascending — a stable
    /// render order independent of insertion timing.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn tallies(
        pool: &SqlitePool,
        issue_id: &str,
    ) -> Result<Vec<ReactionTally>, sqlx::Error> {
        let mut buckets: Vec<ReactionTally> = Vec::new();
        for row in Self::list(pool, issue_id).await? {
            if let Some(b) = buckets.iter_mut().find(|b| b.emoji == row.emoji) {
                b.count += 1;
                b.actors.push(row.actor);
            } else {
                buckets.push(ReactionTally {
                    emoji: row.emoji,
                    count: 1,
                    actors: vec![row.actor],
                });
            }
        }
        buckets.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.emoji.cmp(&b.emoji)));
        Ok(buckets)
    }
}
