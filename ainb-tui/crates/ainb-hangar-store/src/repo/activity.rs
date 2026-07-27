//! Typed repository over the `activity_log` table (multica parity #13,
//! migration 0059).
//!
//! One row per **narrative fact** about an issue: it was created, its state
//! moved, it was re-assigned, its priority/title/due-date was edited, a task on
//! it completed or failed. Attributed to a polymorphic
//! [`ActivityActor`] (`member` / `agent` / `system`).
//!
//! # Best-effort by contract
//!
//! An audit write must NEVER fail the mutation it describes (multica's
//! `slog.Error`-and-return listener contract). Every call site wraps
//! [`ActivityRepo::record`] in `if let Err(e) = … { tracing::warn!(…) }`; the
//! repo itself still returns the error so tests can assert on it.
//!
//! # Not trimmed
//!
//! Unlike `dispatch_attempt` (0058, bounded to 20 rows per issue), this log is
//! the complete story and is only reaped by
//! [`crate::repo::issue::IssueRepo::delete_cascade`].
//!
//! # Lenient reads
//!
//! [`Activity::action`] and [`Activity::actor_type`] are kept as raw strings and
//! decoded on demand ([`Activity::code`] / [`Activity::actor`]): a token written
//! by a newer daemon decodes to `None` and is rendered raw rather than failing
//! the read. [`Activity::details`] is likewise raw JSON text, decoded by
//! [`Activity::details_json`].

use ainb_hangar_core::activity::{ActivityAction, ActivityActor};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

/// An activity row to record.
#[derive(Debug, Clone)]
pub struct NewActivity<'a> {
    /// Owning tenant. The one FK on the table.
    pub workspace_id: &'a str,
    /// The card the fact is about; `None` for a future issue-less activity.
    pub issue_id: Option<&'a str>,
    /// Who did it.
    pub actor: &'a ActivityActor,
    /// What happened.
    pub action: ActivityAction,
    /// The free-form details object. Must be a JSON **object**; `{}` when there
    /// is nothing to say.
    pub details: serde_json::Value,
    /// Epoch millis.
    pub created_at: i64,
}

/// A read-back `activity_log` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// ULID primary key.
    pub id: String,
    /// Owning tenant.
    pub workspace_id: String,
    /// The card the fact is about.
    pub issue_id: Option<String>,
    /// The raw stored actor kind (`member` / `agent` / `system`).
    pub actor_type: Option<String>,
    /// The raw stored actor id; `NULL` for a system row.
    pub actor_id: Option<String>,
    /// The raw stored action token; decode with [`Self::code`].
    pub action: String,
    /// The raw stored details JSON text; decode with [`Self::details_json`].
    pub details: String,
    /// Epoch millis.
    pub created_at: i64,
}

impl Activity {
    /// Decode [`Self::action`] into the typed vocabulary. `None` for a token
    /// this binary does not know (render the raw string in that case).
    #[must_use]
    pub fn code(&self) -> Option<ActivityAction> {
        ActivityAction::parse(&self.action)
    }

    /// Decode the `(actor_type, actor_id)` pair. `None` for a malformed or
    /// unknown pair.
    #[must_use]
    pub fn actor(&self) -> Option<ActivityActor> {
        ActivityActor::parse(self.actor_type.as_deref()?, self.actor_id.as_deref())
    }

    /// Decode [`Self::details`]. Unparseable text yields an empty object rather
    /// than an error — a details blob is decoration, never load-bearing.
    #[must_use]
    pub fn details_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.details)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
    }

    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        Self {
            id: row.get("id"),
            workspace_id: row.get("workspace_id"),
            issue_id: row.get("issue_id"),
            actor_type: row.get("actor_type"),
            actor_id: row.get("actor_id"),
            action: row.get("action"),
            details: row.get("details"),
            created_at: row.get("created_at"),
        }
    }
}

const SELECT_COLS: &str =
    "id, workspace_id, issue_id, actor_type, actor_id, action, details, created_at";

/// Stateless repo over `activity_log`.
pub struct ActivityRepo;

impl ActivityRepo {
    /// Insert one activity row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault (for example a `workspace_id`
    /// FK violation). Callers log and continue — see the module docs.
    pub async fn record(
        pool: &SqlitePool,
        id: &str,
        new: &NewActivity<'_>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(INSERT_SQL)
            .bind(id)
            .bind(new.workspace_id)
            .bind(new.issue_id)
            .bind(new.actor.type_str())
            .bind(new.actor.id())
            .bind(new.action.as_db_str())
            .bind(details_text(&new.details))
            .bind(new.created_at)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Insert one activity row inside an open transaction (so a mutation and
    /// its audit row commit together when the caller wants that).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn record_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        id: &str,
        new: &NewActivity<'_>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(INSERT_SQL)
            .bind(id)
            .bind(new.workspace_id)
            .bind(new.issue_id)
            .bind(new.actor.type_str())
            .bind(new.actor.id())
            .bind(new.action.as_db_str())
            .bind(details_text(&new.details))
            .bind(new.created_at)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// A card's activity history, **newest first**.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn list_for_issue(
        pool: &SqlitePool,
        issue_id: &str,
        limit: i64,
    ) -> Result<Vec<Activity>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {SELECT_COLS} FROM activity_log \
             WHERE issue_id = ? ORDER BY created_at DESC, id DESC LIMIT ?"
        ))
        .bind(issue_id)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        Ok(rows.iter().map(Activity::from_row).collect())
    }

    /// The workspace-scoped feed, newest first. Never returns a sibling
    /// tenant's rows.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn list_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
        limit: i64,
    ) -> Result<Vec<Activity>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {SELECT_COLS} FROM activity_log \
             WHERE workspace_id = ? ORDER BY created_at DESC, id DESC LIMIT ?"
        ))
        .bind(workspace_id)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        Ok(rows.iter().map(Activity::from_row).collect())
    }

    /// Reap a deleted card's activity rows. Called from
    /// [`crate::repo::issue::IssueRepo::delete_cascade`]'s explicit cascade (the
    /// table carries no FK on `issue_id` on purpose — see the migration).
    /// Returns how many rows fell.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn delete_for_issue(
        tx: &mut Transaction<'_, Sqlite>,
        issue_id: &str,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM activity_log WHERE issue_id = ?")
            .bind(issue_id)
            .execute(&mut **tx)
            .await?;
        Ok(res.rows_affected())
    }
}

const INSERT_SQL: &str = "INSERT INTO activity_log \
     (id, workspace_id, issue_id, actor_type, actor_id, action, details, created_at) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?)";

/// Serialise a details value, falling back to `{}` for anything that is not a
/// JSON object (the column's documented contract).
fn details_text(v: &serde_json::Value) -> String {
    if v.is_object() {
        v.to_string()
    } else {
        "{}".to_string()
    }
}
