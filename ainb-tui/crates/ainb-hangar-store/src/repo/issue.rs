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
    /// Urgency: `0..3` mapping `P3..P0` (HIGHER = MORE URGENT). The schema
    /// defaults this to `0` (P3, routine); mirrors `agent_task_queue.priority`.
    pub priority: i64,
    /// Optional deadline as epoch milliseconds; `None` when unset.
    pub due_date: Option<i64>,
    /// Free-form labels (e.g. `["bug", "p0"]`). Stored as a single JSON-array
    /// column — the minimal persistence the create flow needs (the full labels
    /// table + attach/detach is a separate concern).
    pub labels: Vec<String>,
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
    /// Urgency: `0..3` mapping `P3..P0` (HIGHER = MORE URGENT; default `0`).
    pub priority: i64,
    /// Optional deadline as epoch milliseconds; `None` when unset.
    pub due_date: Option<i64>,
    /// Free-form labels, re-assembled from the JSON-array `labels` column.
    pub labels: Vec<String>,
}

/// A partial-edit instruction for one issue's mutable fields (e38.8).
///
/// Each field is an `Option` of "leave unchanged" vs "set to this value". The
/// two nullable columns (`assignee`, `due_date`) nest a second `Option` so the
/// caller can distinguish *clear to NULL* (`Some(None)`) from *leave unchanged*
/// (`None`) — a single layer would conflate the two. `priority` and `state` are
/// non-nullable, so a single `Option` suffices.
///
/// `Default` is all-`None` (a no-op edit); callers fill in only the fields they
/// touch (`IssueFieldUpdate { priority: Some(2), ..Default::default() }`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueFieldUpdate {
    /// New lifecycle state, or `None` to leave it unchanged.
    pub state: Option<String>,
    /// New assignee: `None` leaves it, `Some(None)` clears it (unassign),
    /// `Some(Some(actor))` assigns it.
    pub assignee: Option<Option<ActorRef>>,
    /// New urgency `0..3`, or `None` to leave it unchanged.
    pub priority: Option<i64>,
    /// New due date (epoch ms): `None` leaves it, `Some(None)` clears the
    /// deadline, `Some(Some(ts))` sets it.
    pub due_date: Option<Option<i64>>,
}

impl IssueFieldUpdate {
    /// `true` when no field is set, so [`IssueRepo::update_fields`] would write
    /// nothing — the handler uses this to skip a pointless UPDATE.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.state.is_none()
            && self.assignee.is_none()
            && self.priority.is_none()
            && self.due_date.is_none()
    }
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
        let labels_json = labels_to_json(&issue.labels);
        sqlx::query(
            "INSERT INTO issue \
             (id, workspace_id, title, description, state, \
              assignee_type, assignee_id, creator_type, creator_id, created_at, \
              priority, due_date, labels) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(issue.priority)
        .bind(issue.due_date)
        .bind(labels_json)
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
             assignee_type, assignee_id, creator_type, creator_id, created_at, \
             priority, due_date, labels \
             FROM issue WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        row.map(|r| issue_from_row(&r)).transpose()
    }

    /// Overwrite an issue's lifecycle `state` (e.g. `"open"` → `"done"`).
    ///
    /// Used by the Beads → Hangar inbound sync (P2.4) to land a `bd`-side status
    /// change on the mirrored Hangar issue. Idempotent at the SQL level: writing
    /// the same `state` twice is a no-op UPDATE, and updating an absent id simply
    /// affects zero rows (not an error) — callers that need to distinguish should
    /// pre-check with [`get_by_id`](Self::get_by_id).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn update_state(pool: &SqlitePool, id: &str, state: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE issue SET state = ? WHERE id = ?")
            .bind(state)
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Edit a subset of one issue's mutable fields, scoped to a workspace
    /// (e38.8).
    ///
    /// Only the fields set in `update` are written; absent fields are left as-is.
    /// The two nullable columns can be cleared (`Some(None)`) or set
    /// (`Some(Some(_))`). The write is **workspace-scoped**: the `WHERE` clause
    /// matches `(id, workspace_id)`, so an issue id from another tenant matches
    /// zero rows and changes nothing (a no-op, never a cross-tenant edit).
    ///
    /// Returns `true` when exactly one row was updated, `false` when the
    /// `(id, workspace_id)` pair matched no issue (a foreign tenant, an unknown
    /// id, or — defensively — an empty `update`, which is a deliberate no-op).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails (e.g. a `state`/`priority`
    /// value the schema's constraints reject).
    pub async fn update_fields(
        pool: &SqlitePool,
        workspace_id: &str,
        id: &str,
        update: &IssueFieldUpdate,
    ) -> Result<bool, sqlx::Error> {
        // An empty edit is a deliberate no-op: building an UPDATE with no SET
        // clause would be invalid SQL, so short-circuit before constructing it.
        if update.is_empty() {
            return Ok(false);
        }

        // Build the SET list dynamically from only the present fields, binding
        // positionally in the same order so the `query.bind(...)` chain below
        // matches the placeholders. The nullable actor splits into its two
        // columns (`assignee_type`, `assignee_id`) when present.
        let mut sets: Vec<&str> = Vec::new();
        if update.state.is_some() {
            sets.push("state = ?");
        }
        if update.assignee.is_some() {
            sets.push("assignee_type = ?");
            sets.push("assignee_id = ?");
        }
        if update.priority.is_some() {
            sets.push("priority = ?");
        }
        if update.due_date.is_some() {
            sets.push("due_date = ?");
        }
        let sql = format!(
            "UPDATE issue SET {} WHERE id = ? AND workspace_id = ?",
            sets.join(", ")
        );

        let mut query = sqlx::query(&sql);
        if let Some(state) = &update.state {
            query = query.bind(state);
        }
        if let Some(assignee) = &update.assignee {
            let (assignee_type, assignee_id) = split_actor(assignee.as_ref());
            query = query.bind(assignee_type).bind(assignee_id);
        }
        if let Some(priority) = &update.priority {
            query = query.bind(priority);
        }
        if let Some(due_date) = &update.due_date {
            query = query.bind(due_date);
        }
        let res = query.bind(id).bind(workspace_id).execute(pool).await?;
        Ok(res.rows_affected() == 1)
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
             assignee_type, assignee_id, creator_type, creator_id, created_at, \
             priority, due_date, labels \
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
            let kind = k.parse::<ActorKind>().map_err(|e| decode_err(column, &e.to_string()))?;
            let actor = ActorRef::new(kind, i).map_err(|e| decode_err(column, &e.to_string()))?;
            Ok(Some(actor))
        }
        _ => Err(decode_err(
            column,
            "actor type/id columns disagree on nullness",
        )),
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
    let creator = actor_from_columns(
        row.try_get("creator_type")?,
        row.try_get("creator_id")?,
        "creator",
    )?
    .ok_or_else(|| decode_err("creator", "creator columns must be non-null"))?;
    let labels = labels_from_json(&row.try_get::<String, _>("labels")?)?;
    Ok(Issue {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        state: row.try_get("state")?,
        assignee,
        creator,
        created_at: row.try_get("created_at")?,
        priority: row.try_get("priority")?,
        due_date: row.try_get("due_date")?,
        labels,
    })
}

/// Serialize a label list into the JSON-array text the `labels` column stores.
///
/// An empty list yields `"[]"` (the column default), so a label-less issue is
/// byte-identical to a row written by the schema default.
fn labels_to_json(labels: &[String]) -> String {
    // serde_json::to_string of a Vec<String> is infallible (no map keys, no
    // non-finite floats), so the fallback can never fire — it just keeps the
    // signature panic-free.
    serde_json::to_string(labels).unwrap_or_else(|_| "[]".to_string())
}

/// Re-assemble a label list from the `labels` column's JSON-array text.
///
/// # Errors
///
/// Returns a [`sqlx::Error::ColumnDecode`] if the stored text is not a JSON
/// array of strings (which only the schema default `'[]'` and
/// [`labels_to_json`] ever write, so this surfaces external corruption).
fn labels_from_json(raw: &str) -> Result<Vec<String>, sqlx::Error> {
    serde_json::from_str(raw).map_err(|e| decode_err("labels", &e.to_string()))
}
