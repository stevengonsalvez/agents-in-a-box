//! Typed repository wrapper over the `issue` table.
//!
//! [`IssueRepo`] is a thin, stateless sqlx wrapper. The notable feature is the
//! **polymorphic actor** mapping: an issue's assignee and creator are
//! [`ActorRef`]s in the Rust API, but at the SQL boundary each is split into the
//! two TEXT columns the schema actually stores (`assignee_type`/`assignee_id`,
//! `creator_type`/`creator_id`).
//!
//! # `(actor_type, actor_id)` invariant
//!
//! There is deliberately **no foreign key** on the `_id` columns — an actor may
//! live in either the `member` or `agent` table, which a single `SQLite` FK cannot
//! express. This is FK-less by design (per the Multica architecture review §7).
//! The `_type` columns' `CHECK` constraints keep the discriminant honest; the
//! [`ActorRef`] type keeps the `_id` half non-empty; referential integrity of
//! the `_id` value is a service-layer concern.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use sqlx::{Row, SqlitePool};

/// Parameters for inserting a new `issue` row.
///
/// `assignee` is optional (an unassigned issue is valid); `creator` is
/// mandatory. Both are stored as polymorphic `(type, id)` pairs.
#[derive(Debug, Clone)]
pub struct NewIssue {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Issue title.
    pub title: String,
    /// Free-form description; `None` when unset.
    pub description: Option<String>,
    /// Lifecycle state (e.g. `"open"`); the schema defaults this to `"open"`.
    pub state: String,
    /// Assigned actor, or `None` for an unassigned issue.
    pub assignee: Option<ActorRef>,
    /// The actor that created the issue (mandatory).
    pub creator: ActorRef,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}

/// A fully-materialised `issue` row read back from the database.
///
/// The polymorphic columns are re-assembled into [`ActorRef`]s on read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Primary key.
    pub id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Issue title.
    pub title: String,
    /// Free-form description.
    pub description: Option<String>,
    /// Lifecycle state.
    pub state: String,
    /// Assigned actor, or `None`.
    pub assignee: Option<ActorRef>,
    /// Creating actor.
    pub creator: ActorRef,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}

/// Stateless typed wrapper over the `issue` table.
pub struct IssueRepo;

impl IssueRepo {
    /// Insert one issue, splitting each [`ActorRef`] into its two TEXT columns
    /// at the SQL boundary.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — for example a
    /// `workspace_id` FK violation or an `assignee_type`/`creator_type` `CHECK`
    /// violation (the latter cannot happen through [`ActorRef`], only via raw
    /// SQL).
    pub async fn insert(pool: &SqlitePool, issue: &NewIssue) -> Result<(), sqlx::Error> {
        let (assignee_type, assignee_id) = split_actor(issue.assignee.as_ref());
        let (creator_type, creator_id) = split_actor(Some(&issue.creator));
        sqlx::query(
            "INSERT INTO issue \
             (id, workspace_id, title, description, state, \
              assignee_type, assignee_id, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&issue.id)
        .bind(&issue.workspace_id)
        .bind(&issue.title)
        .bind(&issue.description)
        .bind(&issue.state)
        .bind(assignee_type)
        .bind(assignee_id)
        .bind(creator_type)
        .bind(creator_id)
        .bind(issue.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Fetch one issue by primary key, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a stored actor pair is
    /// malformed (which should be impossible given the `CHECK` constraint plus
    /// the non-null `creator_*` columns).
    pub async fn get_by_id(pool: &SqlitePool, id: &str) -> Result<Option<Issue>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, workspace_id, title, description, state, \
             assignee_type, assignee_id, creator_type, creator_id, created_at \
             FROM issue WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        row.map(|r| issue_from_row(&r)).transpose()
    }

    /// List issues in a workspace filtered by `state`, ordered by `created_at`.
    ///
    /// Backed by the `idx_issue_workspace_state` index.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a row's actor columns are
    /// malformed.
    pub async fn list_by_workspace_state(
        pool: &SqlitePool,
        workspace_id: &str,
        state: &str,
    ) -> Result<Vec<Issue>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, workspace_id, title, description, state, \
             assignee_type, assignee_id, creator_type, creator_id, created_at \
             FROM issue WHERE workspace_id = ? AND state = ? ORDER BY created_at",
        )
        .bind(workspace_id)
        .bind(state)
        .fetch_all(pool)
        .await?;
        rows.iter().map(issue_from_row).collect()
    }
}

/// Split an optional [`ActorRef`] into the `(type, id)` column pair the schema
/// stores. `None` yields `(None, None)` (the unassigned case).
fn split_actor(actor: Option<&ActorRef>) -> (Option<&'static str>, Option<String>) {
    actor.map_or((None, None), |a| {
        (Some(a.kind().as_str()), Some(a.id().to_string()))
    })
}

/// Re-assemble an [`ActorRef`] from a `(type, id)` column pair.
///
/// `None`/`None` (an unassigned assignee) yields `Ok(None)`. A non-null type
/// with a non-null id yields the actor; any other combination, or an
/// unrecognised type token, is a row-level corruption surfaced as a decode
/// error.
fn actor_from_columns(
    kind: Option<String>,
    id: Option<String>,
    column: &str,
) -> Result<Option<ActorRef>, sqlx::Error> {
    match (kind, id) {
        (None, None) => Ok(None),
        (Some(k), Some(i)) => {
            let kind = k
                .parse::<ActorKind>()
                .map_err(|e| decode_err(column, &e.to_string()))?;
            let actor = ActorRef::new(kind, i).map_err(|e| decode_err(column, &e.to_string()))?;
            Ok(Some(actor))
        }
        _ => Err(decode_err(column, "actor type/id columns disagree on nullness")),
    }
}

/// Build a `sqlx` decode error for a malformed actor column pair.
fn decode_err(column: &str, detail: &str) -> sqlx::Error {
    sqlx::Error::ColumnDecode {
        index: column.to_string(),
        source: format!("malformed actor for '{column}': {detail}").into(),
    }
}

/// Map one raw `issue` row into an [`Issue`], re-assembling the polymorphic
/// actor columns. A missing `creator` is treated as corruption (the columns are
/// `NOT NULL`).
fn issue_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Issue, sqlx::Error> {
    let assignee = actor_from_columns(
        row.try_get("assignee_type")?,
        row.try_get("assignee_id")?,
        "assignee",
    )?;
    let creator =
        actor_from_columns(row.try_get("creator_type")?, row.try_get("creator_id")?, "creator")?
            .ok_or_else(|| decode_err("creator", "creator columns must be non-null"))?;
    Ok(Issue {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        state: row.try_get("state")?,
        assignee,
        creator,
        created_at: row.try_get("created_at")?,
    })
}
