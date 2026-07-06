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
//! express. This is FK-less by design (per the reference architecture review §7).
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
    /// New issue title (F6 card edit), or `None` to leave it unchanged. The
    /// caller validates non-blankness before building this (a blank title is a
    /// client error, not a stored empty title).
    pub title: Option<String>,
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
        self.title.is_none()
            && self.state.is_none()
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
        if update.title.is_some() {
            sets.push("title = ?");
        }
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
        if let Some(title) = &update.title {
            query = query.bind(title);
        }
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

    /// The 1-based per-workspace creation ordinal of issue `id` — the `<n>` in
    /// its `HGR-<n>` display id (63l.3).
    ///
    /// Counts the issues in the same workspace that were created at-or-before
    /// this one, tie-broken by `id` so the ordering is total and stable: the
    /// oldest issue is `1`, the next `2`, and so on. Workspace-scoped, so two
    /// workspaces each number their issues from `1` independently. Returns
    /// `Ok(None)` when no issue with `id` exists (a stale id), so the caller can
    /// distinguish "issue absent" from "issue is number N".
    ///
    /// This is a read-time derivation, not a stored counter: an issue's display
    /// number is its position in the workspace's creation order, which the
    /// `(created_at, id)` total order pins deterministically without a sequence
    /// column (and without a per-insert race).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn workspace_seq(
        pool: &SqlitePool,
        workspace_id: &str,
        id: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        // COUNT the rows in the same workspace ordered at-or-before this issue by
        // (created_at, id). The correlated subquery reads the target issue's
        // (created_at, id); a non-existent id makes the inner SELECT empty, so
        // the comparison is NULL and COUNT is 0 — distinguished from a real
        // ordinal by the separate existence check.
        let seq: Option<i64> = sqlx::query_scalar(
            "SELECT COUNT(*) FROM issue AS sibling \
             WHERE sibling.workspace_id = ?1 \
               AND (sibling.created_at, sibling.id) <= ( \
                   SELECT target.created_at, target.id FROM issue AS target \
                   WHERE target.id = ?2 AND target.workspace_id = ?1 \
               )",
        )
        .bind(workspace_id)
        .bind(id)
        .fetch_optional(pool)
        .await?;
        // COUNT always returns one row; a 0 means the id is absent in this
        // workspace (the subquery matched nothing), so report None for an
        // unknown id and the 1-based ordinal otherwise.
        Ok(seq.filter(|n| *n > 0))
    }

    /// Full-text-ish search: every issue in `workspace_id` whose title,
    /// description, OR any of its comment bodies contains `query`
    /// (case-insensitive substring), ranked title > description > comment
    /// (e38.12).
    ///
    /// A row matches if `query` appears in any of the three text surfaces; each
    /// row's rank is its **strongest** hit (a title match outranks a
    /// description-only match outranks a comment-only match), so an issue that
    /// only mentions the term in a comment sorts below one that has it in the
    /// title. The result is ordered `rank DESC, created_at, id` — strongest hits
    /// first, then deterministic by age then id.
    ///
    /// The match is a true substring (the LIKE wildcards `%` / `_` and the
    /// escape `\` in `query` are escaped via [`like_escape`] + `ESCAPE '\'`), so
    /// a query containing `%` matches a literal `%`, mirroring the plugin's
    /// client-side `contains` filter rather than turning into a wildcard. A blank
    /// (all-whitespace) query matches nothing — searching for "" must not dump
    /// the whole board.
    ///
    /// The search joins `comment` so a comment-body hit promotes its parent
    /// issue; `GROUP BY i.id` collapses the per-comment fan-out back to one row
    /// per issue carrying its best rank. Workspace-scoped through
    /// `i.workspace_id = ?`, so a sibling tenant's matching issue is never
    /// returned.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a row's actor / labels
    /// columns are malformed.
    pub async fn search_ranked(
        pool: &SqlitePool,
        workspace_id: &str,
        query: &str,
    ) -> Result<Vec<Issue>, sqlx::Error> {
        // A blank query matches nothing — never dump the whole board.
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        // Lower-case both sides for ASCII-and-Unicode-safe case-insensitivity
        // (SQLite's bare LIKE is only case-insensitive over ASCII), and escape
        // the LIKE metacharacters so the term is a literal substring.
        let pattern = format!("%{}%", like_escape(&trimmed.to_lowercase()));
        // The CASE assigns a per-surface weight; MAX over the comment fan-out
        // keeps the strongest surface per issue. HAVING rank > 0 drops the
        // non-matching rows the LEFT JOIN would otherwise keep.
        let rows = sqlx::query(
            "SELECT i.id, i.workspace_id, i.title, i.description, i.state, \
             i.assignee_type, i.assignee_id, i.creator_type, i.creator_id, i.created_at, \
             i.priority, i.due_date, i.labels, \
             MAX(CASE \
                 WHEN LOWER(i.title) LIKE ?2 ESCAPE '\\' THEN 3 \
                 WHEN LOWER(i.description) LIKE ?2 ESCAPE '\\' THEN 2 \
                 WHEN LOWER(c.body) LIKE ?2 ESCAPE '\\' THEN 1 \
                 ELSE 0 END) AS rank \
             FROM issue i \
             LEFT JOIN comment c ON c.issue_id = i.id \
             WHERE i.workspace_id = ?1 \
             GROUP BY i.id \
             HAVING rank > 0 \
             ORDER BY rank DESC, i.created_at, i.id",
        )
        .bind(workspace_id)
        .bind(&pattern)
        .fetch_all(pool)
        .await?;
        rows.iter().map(issue_from_row).collect()
    }
}

/// Escape the SQL `LIKE` metacharacters (`\`, `%`, `_`) in `term` so the value
/// is matched as a literal substring under an `ESCAPE '\'` clause.
///
/// Escaping `\` first is essential: were it escaped after `%`/`_`, the escape
/// characters this function itself inserts would be double-escaped. The result
/// is wrapped in `%...%` by the caller to form the substring pattern.
fn like_escape(term: &str) -> String {
    term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use ainb_hangar_core::actor::{ActorKind, ActorRef};

    fn member() -> ActorRef {
        ActorRef::new(ActorKind::Member, "user-1").unwrap()
    }

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

    #[allow(clippy::too_many_arguments)]
    async fn seed_issue(
        pool: &SqlitePool,
        ws: &str,
        id: &str,
        title: &str,
        description: Option<&str>,
        created_at: i64,
    ) {
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: id.into(),
                workspace_id: ws.into(),
                title: title.into(),
                description: description.map(ToString::to_string),
                state: "open".into(),
                assignee: None,
                creator: member(),
                created_at,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
            },
        )
        .await
        .unwrap();
    }

    async fn seed_comment(pool: &SqlitePool, id: &str, issue_id: &str, body: &str) {
        sqlx::query(
            "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
             VALUES (?, ?, 'member', 'user-1', ?, 1000)",
        )
        .bind(id)
        .bind(issue_id)
        .bind(body)
        .execute(pool)
        .await
        .unwrap();
    }

    /// `update_fields` writes a new title (F6 card edit) scoped to the workspace,
    /// leaving the untouched columns alone; a title-only edit is not a no-op.
    #[tokio::test]
    async fn update_fields_edits_title_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_issue(pool, "ws-a", "issue-1", "Old title", Some("body"), 1).await;

        // A title-only edit is NOT empty (would previously short-circuit).
        let update = IssueFieldUpdate {
            title: Some("New title".into()),
            ..Default::default()
        };
        assert!(!update.is_empty(), "a title edit must write");
        let touched = IssueRepo::update_fields(pool, "ws-a", "issue-1", &update).await.unwrap();
        assert!(touched, "one row updated");

        let issue = IssueRepo::get_by_id(pool, "issue-1").await.unwrap().unwrap();
        assert_eq!(issue.title, "New title", "title rewritten");
        assert_eq!(
            issue.description.as_deref(),
            Some("body"),
            "other columns untouched"
        );

        // A cross-tenant edit (right id, wrong workspace) touches no row.
        let cross = IssueRepo::update_fields(pool, "ws-b", "issue-1", &update).await.unwrap();
        assert!(!cross, "a foreign-workspace title edit must miss");
        let unchanged = IssueRepo::get_by_id(pool, "issue-1").await.unwrap().unwrap();
        assert_eq!(
            unchanged.title, "New title",
            "foreign-tenant edit left the title"
        );
    }

    /// A title hit outranks a description hit outranks a comment-only hit; a
    /// non-matching issue is excluded entirely.
    #[tokio::test]
    async fn search_ranks_title_over_description_over_comment() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        // i-title: term in the title (rank 3).
        seed_issue(pool, "ws-a", "i-title", "Fix the widget", None, 1).await;
        // i-desc: term only in the description (rank 2).
        seed_issue(
            pool,
            "ws-a",
            "i-desc",
            "Unrelated",
            Some("the widget broke"),
            2,
        )
        .await;
        // i-comment: term only in a comment body (rank 1).
        seed_issue(pool, "ws-a", "i-comment", "Other", Some("nothing here"), 3).await;
        seed_comment(pool, "c1", "i-comment", "saw the widget fail").await;
        // i-none: term nowhere — excluded.
        seed_issue(pool, "ws-a", "i-none", "Calm", Some("no match"), 4).await;

        let hits = IssueRepo::search_ranked(pool, "ws-a", "widget").await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            ["i-title", "i-desc", "i-comment"],
            "title > description > comment ranking, non-match excluded"
        );
    }

    /// Search is case-insensitive over both query and stored text.
    #[tokio::test]
    async fn search_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i1", "Refactor the API Layer", None, 1).await;

        let hits = IssueRepo::search_ranked(pool, "ws-a", "api layer").await.unwrap();
        assert_eq!(hits.len(), 1, "case-insensitive title match");
        assert_eq!(hits[0].id, "i1");
    }

    /// A blank query matches nothing — never dump the whole board.
    #[tokio::test]
    async fn search_blank_query_matches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i1", "Anything", None, 1).await;

        assert!(IssueRepo::search_ranked(pool, "ws-a", "   ").await.unwrap().is_empty());
        assert!(IssueRepo::search_ranked(pool, "ws-a", "").await.unwrap().is_empty());
    }

    /// LIKE metacharacters in the query are matched literally (a `%` does not
    /// become a wildcard).
    #[tokio::test]
    async fn search_escapes_like_metacharacters() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i-pct", "Cut load by 50% today", None, 1).await;
        seed_issue(pool, "ws-a", "i-plain", "no percent here", None, 2).await;

        // "50%" must match only the literal-percent issue, not act as a wildcard
        // that would also match "i-plain".
        let hits = IssueRepo::search_ranked(pool, "ws-a", "50%").await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["i-pct"], "% is a literal, not a wildcard");
    }

    /// One issue with the term in BOTH title and a comment ranks by its title
    /// (best surface), not its comment — the MAX-over-fan-out collapse.
    #[tokio::test]
    async fn search_collapses_fanout_to_best_surface() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        // i-both: term in the title (rank 3) AND two comments (rank 1 each).
        seed_issue(pool, "ws-a", "i-both", "deploy pipeline", None, 1).await;
        seed_comment(pool, "c1", "i-both", "the deploy is flaky").await;
        seed_comment(pool, "c2", "i-both", "deploy again please").await;
        // i-cmt: term only in a comment (rank 1).
        seed_issue(pool, "ws-a", "i-cmt", "Other work", None, 2).await;
        seed_comment(pool, "c3", "i-cmt", "deploy notes").await;

        let hits = IssueRepo::search_ranked(pool, "ws-a", "deploy").await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|i| i.id.as_str()).collect();
        // i-both appears exactly once (no fan-out duplication) and ranks above
        // the comment-only i-cmt because its title hit (3) beats a comment (1).
        assert_eq!(
            ids,
            ["i-both", "i-cmt"],
            "best surface per issue, no dup rows"
        );
    }

    /// An issue in a freshly-bootstrapped workspace (NULL prefix — no explicit
    /// one) reads the display id `HGR-<n>`, numbered 1-based in creation order
    /// (63l.3 user proof a). The HGR default lives at the display layer while the
    /// stored title stays verbatim, and a second workspace numbers its own issues
    /// from 1 independently.
    #[tokio::test]
    async fn issue_reads_hgr_display_id_in_a_fresh_workspace() {
        use crate::repo::workspace::issue_display_id;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // A freshly-bootstrapped workspace has a NULL issue_prefix (no explicit
        // prefix) — exactly what `ensure_default_workspace` writes.
        seed_ws(pool, "ws-a").await;

        // Three issues created in order → ordinals 1, 2, 3.
        seed_issue(pool, "ws-a", "i-1", "first", None, 10).await;
        seed_issue(pool, "ws-a", "i-2", "second", None, 20).await;
        seed_issue(pool, "ws-a", "i-3", "third", None, 30).await;

        // The column is NULL (no title-prefix mangling), yet the display id reads
        // HGR-<n> via the display-layer default.
        let prefix: Option<String> =
            sqlx::query_scalar("SELECT issue_prefix FROM workspace WHERE id = ?")
                .bind("ws-a")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(prefix, None, "fresh workspace stores NO explicit prefix");

        for (id, expected_seq, expected_display) in [
            ("i-1", 1, "HGR-1"),
            ("i-2", 2, "HGR-2"),
            ("i-3", 3, "HGR-3"),
        ] {
            let seq = IssueRepo::workspace_seq(pool, "ws-a", id).await.unwrap();
            assert_eq!(seq, Some(expected_seq), "{id} 1-based ordinal");
            assert_eq!(
                issue_display_id(prefix.as_deref(), seq.unwrap()),
                expected_display,
                "{id} reads {expected_display}"
            );
        }

        // A stale id has no ordinal (distinguished from "is number N").
        assert_eq!(
            IssueRepo::workspace_seq(pool, "ws-a", "missing").await.unwrap(),
            None,
            "unknown id has no ordinal"
        );

        // A second workspace numbers its own issues from 1, independent of ws-a.
        seed_ws(pool, "ws-b").await;
        seed_issue(pool, "ws-b", "b-1", "b first", None, 5).await;
        let seq_b = IssueRepo::workspace_seq(pool, "ws-b", "b-1").await.unwrap();
        assert_eq!(seq_b, Some(1), "ws-b numbers from 1 independently");
        assert_eq!(issue_display_id(None, seq_b.unwrap()), "HGR-1");
    }

    /// Search is workspace-scoped: a sibling tenant's matching issue is never
    /// returned.
    #[tokio::test]
    async fn search_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_issue(pool, "ws-a", "i-a", "shared keyword", None, 1).await;
        seed_issue(pool, "ws-b", "i-b", "shared keyword", None, 1).await;

        let hits = IssueRepo::search_ranked(pool, "ws-a", "keyword").await.unwrap();
        let ids: Vec<&str> = hits.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["i-a"], "only the owning tenant's issue is returned");
    }
}
