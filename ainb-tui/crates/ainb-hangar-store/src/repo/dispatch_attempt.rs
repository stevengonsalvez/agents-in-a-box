//! Typed repository over the `dispatch_attempt` audit table (multica parity #12,
//! migration 0058).
//!
//! One row per **admission decision**: a trigger surface tried to turn a card
//! into a run, and this is the stable code
//! ([`ainb_hangar_core::dispatch_reason::DispatchReason`]) plus the free-text
//! detail for what happened. It is the persistent answer to "why is my card not
//! running", which before 0058 existed only as an ephemeral RPC error string or
//! a debug log line.
//!
//! # Bounded by construction
//!
//! [`DispatchAttemptRepo::record`] trims the issue's history to the newest
//! [`DISPATCH_ATTEMPT_KEEP`] rows in the same call as the insert, so a hot
//! auto-run cascade cannot grow the table without bound and no sweeper job is
//! needed (see the migration's comment block).
//!
//! # Workspace scoping
//!
//! [`DispatchAttemptRepo::list_by_workspace`] is `workspace_id`-scoped like every
//! other repo here, so a sibling tenant's attempts are never returned.
//! [`DispatchAttemptRepo::latest_for_issue`] / [`DispatchAttemptRepo::list_for_issue`]
//! key on the issue id, which is already tenant-resolved by every caller.
//!
//! # Lenient reads
//!
//! [`DispatchAttempt::reason`] is kept as the raw `String` and decoded on demand
//! via [`DispatchAttempt::code`]: a token written by a newer daemon that this
//! binary does not know decodes to `None` and is rendered as raw text rather
//! than failing the read.

use ainb_hangar_core::dispatch_reason::{DispatchReason, DispatchSource};
use sqlx::{Row, SqlitePool, Sqlite, Transaction};

/// How many attempts are retained per issue. The recorder trims to this on every
/// insert (migration 0058, decision 3).
pub const DISPATCH_ATTEMPT_KEEP: i64 = 20;

/// An attempt to record.
#[derive(Debug, Clone)]
pub struct NewDispatchAttempt<'a> {
    /// Owning tenant. The one FK on the table.
    pub workspace_id: &'a str,
    /// The card the attempt was for; `None` for a future issue-less trigger.
    pub issue_id: Option<&'a str>,
    /// The resolved target agent, when one resolved.
    pub agent_id: Option<&'a str>,
    /// The target agent's runtime, when one resolved.
    pub runtime_id: Option<&'a str>,
    /// Set iff a task row was actually written.
    pub task_id: Option<&'a str>,
    /// The stable admission code.
    pub reason: DispatchReason,
    /// Free text carrying the specifics the code deliberately omits.
    pub detail: Option<&'a str>,
    /// Which trigger surface made the attempt.
    pub source: DispatchSource,
    /// Epoch millis.
    pub created_at: i64,
}

/// A read-back `dispatch_attempt` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchAttempt {
    /// ULID primary key.
    pub id: String,
    /// Owning tenant.
    pub workspace_id: String,
    /// The card the attempt was for.
    pub issue_id: Option<String>,
    /// The resolved target agent.
    pub agent_id: Option<String>,
    /// The target agent's runtime.
    pub runtime_id: Option<String>,
    /// The task row written, when one was.
    pub task_id: Option<String>,
    /// The raw stored code. Kept as text so an unknown future token survives a
    /// read; decode with [`Self::code`].
    pub reason: String,
    /// Free-text specifics.
    pub detail: Option<String>,
    /// The raw stored trigger surface.
    pub source: String,
    /// Epoch millis.
    pub created_at: i64,
}

impl DispatchAttempt {
    /// Decode [`Self::reason`] into the typed vocabulary. `None` for a token
    /// this binary does not know (render the raw string in that case).
    #[must_use]
    pub fn code(&self) -> Option<DispatchReason> {
        DispatchReason::parse(&self.reason)
    }

    /// Decode [`Self::source`]; `None` for an unknown token.
    #[must_use]
    pub fn source_code(&self) -> Option<DispatchSource> {
        DispatchSource::parse(&self.source)
    }

    /// Did this attempt actually produce (or fold into) a run?
    ///
    /// An unknown code is treated as **not** dispatched: an older binary reading
    /// a newer daemon's token should surface it rather than hide it.
    #[must_use]
    pub fn is_dispatched(&self) -> bool {
        self.code().is_some_and(DispatchReason::is_dispatched)
    }

    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        Self {
            id: row.get("id"),
            workspace_id: row.get("workspace_id"),
            issue_id: row.get("issue_id"),
            agent_id: row.get("agent_id"),
            runtime_id: row.get("runtime_id"),
            task_id: row.get("task_id"),
            reason: row.get("reason"),
            detail: row.get("detail"),
            source: row.get("source"),
            created_at: row.get("created_at"),
        }
    }
}

const SELECT_COLS: &str = "id, workspace_id, issue_id, agent_id, runtime_id, task_id, \
                           reason, detail, source, created_at";

/// Stateless repo over `dispatch_attempt`.
pub struct DispatchAttemptRepo;

impl DispatchAttemptRepo {
    /// Insert one attempt, then trim the issue's history to the newest
    /// [`DISPATCH_ATTEMPT_KEEP`] rows. Returns the new row's id.
    ///
    /// The trim is a no-op for an issue-less attempt (`issue_id IS NULL`);
    /// those are workspace-scoped facts with no natural per-card bound, and no
    /// producer mints them today.
    pub async fn record(
        pool: &SqlitePool,
        id: &str,
        new: &NewDispatchAttempt<'_>,
    ) -> Result<String, sqlx::Error> {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO dispatch_attempt \
             (id, workspace_id, issue_id, agent_id, runtime_id, task_id, reason, detail, source, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(new.workspace_id)
        .bind(new.issue_id)
        .bind(new.agent_id)
        .bind(new.runtime_id)
        .bind(new.task_id)
        .bind(new.reason.as_db_str())
        .bind(new.detail)
        .bind(new.source.as_db_str())
        .bind(new.created_at)
        .execute(&mut *tx)
        .await?;

        if let Some(issue_id) = new.issue_id {
            sqlx::query(
                "DELETE FROM dispatch_attempt \
                 WHERE issue_id = ? \
                   AND id NOT IN ( \
                       SELECT id FROM dispatch_attempt \
                       WHERE issue_id = ? \
                       ORDER BY created_at DESC, id DESC \
                       LIMIT ? \
                   )",
            )
            .bind(issue_id)
            .bind(issue_id)
            .bind(DISPATCH_ATTEMPT_KEEP)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(id.to_owned())
    }

    /// The newest attempt for a card, if any.
    pub async fn latest_for_issue(
        pool: &SqlitePool,
        issue_id: &str,
    ) -> Result<Option<DispatchAttempt>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {SELECT_COLS} FROM dispatch_attempt \
             WHERE issue_id = ? ORDER BY created_at DESC, id DESC LIMIT 1"
        ))
        .bind(issue_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.as_ref().map(DispatchAttempt::from_row))
    }

    /// A card's attempt history, newest first.
    pub async fn list_for_issue(
        pool: &SqlitePool,
        issue_id: &str,
        limit: i64,
    ) -> Result<Vec<DispatchAttempt>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {SELECT_COLS} FROM dispatch_attempt \
             WHERE issue_id = ? ORDER BY created_at DESC, id DESC LIMIT ?"
        ))
        .bind(issue_id)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        Ok(rows.iter().map(DispatchAttempt::from_row).collect())
    }

    /// The workspace-scoped feed, newest first. Never returns a sibling
    /// tenant's rows.
    pub async fn list_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
        limit: i64,
    ) -> Result<Vec<DispatchAttempt>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {SELECT_COLS} FROM dispatch_attempt \
             WHERE workspace_id = ? ORDER BY created_at DESC, id DESC LIMIT ?"
        ))
        .bind(workspace_id)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        Ok(rows.iter().map(DispatchAttempt::from_row).collect())
    }

    /// Reap a deleted card's attempts. Called from `IssueRepo::delete`'s
    /// explicit cascade (the table carries no FK on `issue_id` on purpose —
    /// see the migration).
    pub async fn delete_for_issue(
        tx: &mut Transaction<'_, Sqlite>,
        issue_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM dispatch_attempt WHERE issue_id = ?")
            .bind(issue_id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}
