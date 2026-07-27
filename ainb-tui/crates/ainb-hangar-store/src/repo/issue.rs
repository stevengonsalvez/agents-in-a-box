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

use ainb_hangar_core::acceptance::{
    AcceptanceCriterion, criteria_from_json, criteria_to_json, normalise_ids,
    resolve_criterion_index,
};
use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_core::origin::{IssueOrigin, OriginKind};
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
    /// Ordered acceptance criteria, one [`AcceptanceCriterion`] per element:
    /// stable id + text + checked bit. Stored as a single JSON-array column
    /// (migration 0048 created it, 0054 promoted the element shape from bare
    /// strings to objects — multica parity `init.up.sql:66`).
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    /// Ordered context-reference strings (URL / `owner/repo#123` / free text, one
    /// per element). Stored as a single JSON-array column, identical persistence to
    /// `labels` (migration 0048, multica parity `init.up.sql:67` `context_refs`).
    pub context_refs: Vec<String>,
    /// Optional parent issue: a self-link making this a **sub-issue** of another
    /// issue in the same workspace. `None` = a top-level issue (migration 0046).
    pub parent_issue_id: Option<String>,
    /// Optional 1-based **barrier stage** grouping this sub-issue among its
    /// siblings; `None` = unstaged (one implicit stage) (migration 0046).
    pub stage: Option<i64>,
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
    /// Ordered acceptance criteria, re-assembled from the JSON-array
    /// `acceptance_criteria` column (migrations 0048 + 0054). Legacy bare-string
    /// elements decode as unchecked criteria with placeholder ids.
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    /// Ordered context-reference strings, re-assembled from the JSON-array
    /// `context_refs` column (migration 0048).
    pub context_refs: Vec<String>,
    /// Optional upstream-issue reference (URL or `owner/repo#123`), or `None`
    /// (migration 0043).
    pub external_ref: Option<String>,
    /// Optional parent issue: a self-link making this a **sub-issue**. `None` =
    /// a top-level issue (migration 0046).
    pub parent_issue_id: Option<String>,
    /// Optional 1-based **barrier stage** among siblings; `None` = unstaged
    /// (migration 0046).
    pub stage: Option<i64>,
    /// Who/what created this issue (migration 0056, multica parity #21):
    /// `('autopilot', <autopilot.id>)`, `('comment_mention', <comment.id>)` or
    /// `('manual', NULL)`. `None` for every pre-0056 row — "provenance
    /// unknown", deliberately distinct from an explicit `manual`.
    pub origin: Option<IssueOrigin>,
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

/// Counts of the dependent rows an issue delete removes — the delete summary and
/// the CLI dry-run preview both carry this (63d).
///
/// The three surfaced counts are the ones a human recognises ("this issue has 2
/// comments, 1 run, and sits on 1 board"); the internal link rows (label links,
/// dependency edges, usage rows) cascade silently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueDeleteSummary {
    /// Comment rows on the issue.
    pub comments: i64,
    /// Task rows (any lifecycle state) enqueued against the issue.
    pub tasks: i64,
    /// Board placements (cards) referencing the issue.
    pub placements: i64,
    /// Activity-log rows recording the issue's narrative (migration 0059). The
    /// table carries no FK on `issue_id`, so the cascade reaps these explicitly.
    pub activities: i64,
}

/// A dry-run preview of an issue delete: the human title, the dependent counts,
/// and how many tasks are ACTIVE (which would block the delete) (63d).
///
/// The CLI prints this without `--yes`; a non-zero [`Self::active_tasks`] means a
/// real delete would be refused with [`IssueDeleteError::ActiveTasks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueDeletePreview {
    /// The issue title (for the "would delete <title>" line).
    pub title: String,
    /// The dependent-row counts a delete would remove.
    pub summary: IssueDeleteSummary,
    /// How many of the issue's tasks are ACTIVE (queued / dispatched / running);
    /// non-zero means a delete is refused until the run is cancelled.
    pub active_tasks: i64,
}

/// Failure modes of [`IssueRepo::delete_cascade`] (63d).
#[derive(Debug)]
pub enum IssueDeleteError {
    /// No issue matched `(id, workspace_id)` — an unknown id or a foreign tenant.
    NotFound,
    /// One or more tasks on the issue are ACTIVE (queued / dispatched / running);
    /// the run must be cancelled before the issue can be deleted. Carries the
    /// active count.
    ActiveTasks(i64),
    /// An underlying store fault.
    Db(sqlx::Error),
}

impl std::fmt::Display for IssueDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no issue with that id in this workspace"),
            Self::ActiveTasks(n) => write!(
                f,
                "{n} active task(s) on this issue — cancel the run first, then delete"
            ),
            Self::Db(e) => write!(f, "store error: {e}"),
        }
    }
}

impl std::error::Error for IssueDeleteError {}

impl From<sqlx::Error> for IssueDeleteError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// Statuses that make a task ACTIVE — an issue with any active task refuses
/// deletion. Mirrors the `agent_task_queue.status` CHECK vocabulary (migration
/// 0004); the terminal states (`done` / `failed` / `cancelled`) do not block.
const ACTIVE_TASK_STATUSES: &str = "SELECT COUNT(*) FROM agent_task_queue \
     WHERE issue_id = ? AND status IN ('queued','dispatched','running')";

/// Count the dependent rows a delete would remove, over any connection (the pool
/// for a preview, or the open transaction for the cascade). Re-borrows `conn`
/// per query so the same connection drives all three counts.
async fn count_dependents(
    conn: &mut sqlx::SqliteConnection,
    issue_id: &str,
) -> Result<IssueDeleteSummary, sqlx::Error> {
    let comments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comment WHERE issue_id = ?")
        .bind(issue_id)
        .fetch_one(&mut *conn)
        .await?;
    let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE issue_id = ?")
        .bind(issue_id)
        .fetch_one(&mut *conn)
        .await?;
    let placements: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM board_card WHERE issue_id = ?")
        .bind(issue_id)
        .fetch_one(&mut *conn)
        .await?;
    let activities: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM activity_log WHERE issue_id = ?")
            .bind(issue_id)
            .fetch_one(&mut *conn)
            .await?;
    Ok(IssueDeleteSummary {
        comments,
        tasks,
        placements,
        activities,
    })
}

/// Wall-clock millis for a subscription row minted as a side effect.
///
/// The subscription writers below are best-effort side effects, not part of the
/// caller's logical write, so they do not thread the caller's clock through
/// every signature. `created_at` on `issue_subscriber` is ordering metadata
/// only — nothing branches on it.
fn now_ms_for_subscription() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Subscribe a new issue's creator (and assignee, when set) — multica parity
/// #22. Best effort: a failure is logged, never propagated, because the issue
/// itself has already committed.
async fn auto_subscribe_on_create(pool: &SqlitePool, issue: &NewIssue) {
    use crate::repo::issue_subscriber::{IssueSubscriberRepo, SubscribeReason};
    let now = now_ms_for_subscription();
    // Order matters: `creator` first, so an actor who is BOTH creator and
    // assignee keeps `creator` (first reason wins, as the reference's backfill).
    let pairs = [
        (Some(&issue.creator), SubscribeReason::Creator),
        (issue.assignee.as_ref(), SubscribeReason::Assignee),
    ];
    for (actor, reason) in pairs {
        let Some(actor) = actor else { continue };
        if let Err(e) =
            IssueSubscriberRepo::add(pool, &issue.workspace_id, &issue.id, actor, reason, now).await
        {
            tracing::warn!(error = %e, issue_id = %issue.id, "create auto-subscribe failed");
        }
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
        let acceptance_json = criteria_to_json(&issue.acceptance_criteria);
        let context_refs_json = labels_to_json(&issue.context_refs);
        sqlx::query(
            "INSERT INTO issue \
             (id, workspace_id, title, description, state, \
              assignee_type, assignee_id, creator_type, creator_id, created_at, \
              priority, due_date, labels, acceptance_criteria, context_refs, \
              parent_issue_id, stage) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(acceptance_json)
        .bind(context_refs_json)
        .bind(&issue.parent_issue_id)
        .bind(issue.stage)
        .execute(pool)
        .await?;
        // multica parity #22: the creator (and the assignee, when set) become
        // SUBSCRIBERS of the new issue -- the reference's `016_backfill` seeds
        // exactly this pair, and its live writer does the same at create time.
        // Done HERE rather than in each daemon handler because `insert` is the
        // one create seam every path shares (issue create RPC, board-card
        // create, autopilot fire, the CLI). Best effort: a subscription is a
        // side effect and must never fail an otherwise committed create.
        auto_subscribe_on_create(pool, issue).await;
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
             priority, due_date, labels, acceptance_criteria, context_refs, external_ref, parent_issue_id, stage, origin_type, origin_id \
             FROM issue WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        row.map(|r| issue_from_row(&r)).transpose()
    }

    /// Stamp an issue's ORIGIN PROVENANCE post-insert (migration 0056), exactly
    /// like [`CardParityRepo::set_issue_external_ref`] stamps `external_ref`.
    ///
    /// A post-insert setter is deliberate: it keeps [`NewIssue`] — and its ~30
    /// construction sites — untouched, and mirrors the pattern the crate already
    /// uses for every "the create flow learned one more fact" column.
    ///
    /// Workspace-scoped: an issue id from another tenant matches zero rows and
    /// changes nothing. Returns `true` when exactly one row was stamped.
    ///
    /// [`CardParityRepo::set_issue_external_ref`]: crate::repo::card_parity::CardParityRepo::set_issue_external_ref
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_origin(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
        origin: &IssueOrigin,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE issue SET origin_type = ?, origin_id = ? WHERE id = ? AND workspace_id = ?",
        )
        .bind(origin.kind_db_str())
        .bind(origin.id())
        .bind(issue_id)
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// The DETERMINISTIC by-origin lookup — multica's `GetIssueByOrigin`
    /// (`service/task.go:1836`), the whole reason the pair exists: resolve "the
    /// issue this run produced" by its stamped provenance instead of "the
    /// agent's most recent issue", which races against concurrent creates by the
    /// same agent.
    ///
    /// Served by `idx_issue_origin`. A `manual` origin carries no id and is
    /// therefore never a useful lookup key — it matches the (potentially many)
    /// manual rows, so callers should only pass an id-bearing origin. Returns
    /// the OLDEST match when several exist, so the answer is stable.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a row is malformed.
    pub async fn by_origin(
        pool: &SqlitePool,
        workspace_id: &str,
        origin: &IssueOrigin,
    ) -> Result<Option<Issue>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, workspace_id, title, description, state, \
             assignee_type, assignee_id, creator_type, creator_id, created_at, \
             priority, due_date, labels, acceptance_criteria, context_refs, external_ref, parent_issue_id, stage, origin_type, origin_id \
             FROM issue \
             WHERE workspace_id = ? AND origin_type = ? AND origin_id IS ? \
             ORDER BY created_at, id LIMIT 1",
        )
        .bind(workspace_id)
        .bind(origin.kind_db_str())
        .bind(origin.id())
        .fetch_optional(pool)
        .await?;
        row.map(|r| issue_from_row(&r)).transpose()
    }

    /// Every issue in a workspace stamped with `kind` — the "which issues did
    /// the platform create" list filter multica's `origin_type` was added for
    /// (`042_autopilot.up.sql:73`, "so they can be filtered in lists").
    ///
    /// Oldest first. Served by `idx_issue_origin`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a row is malformed.
    pub async fn list_by_origin_kind(
        pool: &SqlitePool,
        workspace_id: &str,
        kind: OriginKind,
    ) -> Result<Vec<Issue>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, workspace_id, title, description, state, \
             assignee_type, assignee_id, creator_type, creator_id, created_at, \
             priority, due_date, labels, acceptance_criteria, context_refs, external_ref, parent_issue_id, stage, origin_type, origin_id \
             FROM issue \
             WHERE workspace_id = ? AND origin_type = ? \
             ORDER BY created_at, id",
        )
        .bind(workspace_id)
        .bind(kind.as_db_str())
        .fetch_all(pool)
        .await?;
        rows.iter().map(issue_from_row).collect()
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
        let touched = res.rows_affected() == 1;
        // multica parity #22: a NEW assignee starts watching the issue. Clearing
        // the assignee subscribes nobody and un-subscribes nobody (the reference
        // has no un-subscribe-on-reassign either -- membership only grows until
        // someone explicitly opts out). Best effort, as at create.
        if let (true, Some(Some(assignee))) = (touched, update.assignee.as_ref()) {
            crate::repo::issue_subscriber::IssueSubscriberRepo::add(
                pool,
                workspace_id,
                id,
                assignee,
                crate::repo::issue_subscriber::SubscribeReason::Assignee,
                now_ms_for_subscription(),
            )
            .await
            .map_err(|e| tracing::warn!(error = %e, id, "assignee auto-subscribe failed"))
            .ok();
        }
        Ok(touched)
    }

    /// Apply ONE lifecycle `state` to many issues in a SINGLE transaction,
    /// returning `(id, prev_state)` for every id that actually changed
    /// (multica parity #3-rest).
    ///
    /// The transaction is the point: the child-done cascade that runs after this
    /// must observe the batch's FINAL state, never a half-applied prefix. Ids are
    /// processed in caller order; an unknown or foreign-tenant id is skipped (it
    /// resolves no row), and an id already in `state` is skipped too so a no-op
    /// re-save cannot manufacture a transition.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if any statement fails; the whole batch then
    /// rolls back.
    pub async fn set_state_batch(
        pool: &SqlitePool,
        workspace_id: &str,
        ids: &[String],
        state: &str,
    ) -> Result<Vec<(String, String)>, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let mut changed = Vec::new();
        for id in ids {
            let prev: Option<String> = sqlx::query_scalar(
                "SELECT state FROM issue WHERE id = ? AND workspace_id = ?",
            )
            .bind(id)
            .bind(workspace_id)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(prev) = prev else { continue };
            if prev == state {
                continue;
            }
            let res = sqlx::query("UPDATE issue SET state = ? WHERE id = ? AND workspace_id = ?")
                .bind(state)
                .bind(id)
                .bind(workspace_id)
                .execute(&mut *tx)
                .await?;
            if res.rows_affected() == 1 {
                changed.push((id.clone(), prev));
            }
        }
        tx.commit().await?;
        Ok(changed)
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
             priority, due_date, labels, acceptance_criteria, context_refs, external_ref, parent_issue_id, stage, origin_type, origin_id \
             FROM issue WHERE workspace_id = ? AND state = ? ORDER BY created_at",
        )
        .bind(workspace_id)
        .bind(state)
        .fetch_all(pool)
        .await?;
        rows.iter().map(issue_from_row).collect()
    }

    /// List the sub-issues of `parent_id`, ordered by barrier `stage` (unstaged
    /// last) then creation order (migration 0046).
    ///
    /// Backed by `idx_issue_parent`. `NULLS LAST` keeps unstaged children after
    /// the staged frontier so the caller can reason about the lowest unfinished
    /// stage without a second pass.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a row's actor / labels
    /// columns are malformed.
    pub async fn list_children(
        pool: &SqlitePool,
        parent_id: &str,
    ) -> Result<Vec<Issue>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, workspace_id, title, description, state, \
             assignee_type, assignee_id, creator_type, creator_id, created_at, \
             priority, due_date, labels, acceptance_criteria, context_refs, external_ref, parent_issue_id, stage, origin_type, origin_id \
             FROM issue WHERE parent_issue_id = ? \
             ORDER BY stage IS NULL, stage, created_at, id",
        )
        .bind(parent_id)
        .fetch_all(pool)
        .await?;
        rows.iter().map(issue_from_row).collect()
    }

    /// Roll-up counts for a parent's sub-issues: `(done, total)` where `done`
    /// counts children whose `state` is terminal (`done` or `cancelled`)
    /// (migration 0046).
    ///
    /// A parent with no children reads `(0, 0)`. Backed by `idx_issue_parent`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn child_progress(
        pool: &SqlitePool,
        parent_id: &str,
    ) -> Result<(i64, i64), sqlx::Error> {
        let row = sqlx::query(
            "SELECT \
               COALESCE(SUM(CASE WHEN state IN ('done','cancelled') THEN 1 ELSE 0 END), 0) AS done, \
               COUNT(*) AS total \
             FROM issue WHERE parent_issue_id = ?",
        )
        .bind(parent_id)
        .fetch_one(pool)
        .await?;
        Ok((row.try_get("done")?, row.try_get("total")?))
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
             i.priority, i.due_date, i.labels, i.acceptance_criteria, i.context_refs, i.external_ref, i.parent_issue_id, i.stage, i.origin_type, i.origin_id, \
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

    /// Dry-run preview of an issue delete: the title, the dependent-row counts,
    /// and the active-task count that would block the delete (63d).
    ///
    /// Workspace-scoped: a `(id, workspace_id)` pair that matches no issue (an
    /// unknown id, or one owned by another tenant) yields `Ok(None)`, so a caller
    /// can distinguish "absent" from "present with these counts". A read only —
    /// nothing is mutated.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn delete_preview(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
    ) -> Result<Option<IssueDeletePreview>, sqlx::Error> {
        let title: Option<String> =
            sqlx::query_scalar("SELECT title FROM issue WHERE id = ? AND workspace_id = ?")
                .bind(issue_id)
                .bind(workspace_id)
                .fetch_optional(pool)
                .await?;
        let Some(title) = title else {
            return Ok(None);
        };
        let mut conn = pool.acquire().await?;
        let summary = count_dependents(&mut conn, issue_id).await?;
        let active_tasks: i64 = sqlx::query_scalar(ACTIVE_TASK_STATUSES)
            .bind(issue_id)
            .fetch_one(&mut *conn)
            .await?;
        Ok(Some(IssueDeletePreview {
            title,
            summary,
            active_tasks,
        }))
    }

    /// Delete an issue and every dependent row in one transaction, in FK order
    /// (63d).
    ///
    /// Refuses (leaving everything intact) when any task on the issue is ACTIVE —
    /// a queued / dispatched / running run must be cancelled first, so a delete
    /// never orphans a live task. Workspace-scoped: a `(id, workspace_id)` pair
    /// that matches no issue is [`IssueDeleteError::NotFound`] and touches nothing.
    ///
    /// The cascade, in dependency order under a deferred-FK transaction (so a
    /// retry task's `parent_task_id` self-reference to a sibling task in the same
    /// delete set never trips mid-statement):
    ///
    /// 1. the `inbox_entry` notifications whose `subject_id` is the issue, one of
    ///    its comments, or one of its tasks go (resolved by subquery WHILE the
    ///    comment/task rows still exist) — a deleted issue's notifications must
    ///    not deep-link to nothing.
    /// 2. `run_history.task_id` is NULLED, not deleted — cost accounting never
    ///    shrinks; the run row survives, detached from the vanishing task.
    /// 3. the tasks' `task_usage` rows and `task`-kind `beads_mapping` rows go.
    /// 4. the `agent_task_queue` task rows go.
    /// 5. the issue's `issue_label` links, `comment`s, `board_card` placements,
    ///    `card_dependency` edges (either endpoint), and `issue`-kind
    ///    `beads_mapping` row go.
    /// 6. the `issue` row itself goes.
    ///
    /// `event_log.entity` refs are DELIBERATELY left untouched: the event log is
    /// an append-only audit trail (same rationale as keeping `run_history`).
    ///
    /// Returns the [`IssueDeleteSummary`] of what fell (the counts captured before
    /// the deletes).
    ///
    /// # Errors
    ///
    /// [`IssueDeleteError::NotFound`] (no such issue in the workspace),
    /// [`IssueDeleteError::ActiveTasks`] (a live run blocks the delete), or
    /// [`IssueDeleteError::Db`] on a store fault.
    pub async fn delete_cascade(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
    ) -> Result<IssueDeleteSummary, IssueDeleteError> {
        let mut tx = pool.begin().await?;

        // Resolve the issue within its workspace; a foreign / unknown id is
        // NotFound and rolls the (empty) transaction back on drop.
        let exists: Option<String> =
            sqlx::query_scalar("SELECT title FROM issue WHERE id = ? AND workspace_id = ?")
                .bind(issue_id)
                .bind(workspace_id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(IssueDeleteError::NotFound);
        }

        // Refuse while any task is live — deleting the issue would orphan the run.
        let active: i64 = sqlx::query_scalar(ACTIVE_TASK_STATUSES)
            .bind(issue_id)
            .fetch_one(&mut *tx)
            .await?;
        if active > 0 {
            return Err(IssueDeleteError::ActiveTasks(active));
        }

        // Capture the summary BEFORE any delete so the counts reflect what fell.
        let summary = count_dependents(&mut tx, issue_id).await?;

        // Defer FK enforcement to commit: a retry task chains to its parent via
        // `parent_task_id`, so a single `DELETE ... WHERE issue_id = ?` over both
        // would trip an immediate self-FK check depending on row order. The whole
        // graph is consistent at commit, which is all that matters. `PRAGMA
        // defer_foreign_keys` is transaction-scoped and resets on commit.
        sqlx::query("PRAGMA defer_foreign_keys = ON").execute(&mut *tx).await?;

        // 1. Drop the issue's inbox notifications — the entries whose by-value
        //    `subject_id` (migration 0021) is the issue itself, one of its
        //    comments, or one of its tasks. MUST run while the comment/task rows
        //    still exist (the subqueries resolve their ids), and is workspace-
        //    scoped for hygiene. `event_log.entity` is deliberately NOT cleaned:
        //    the event log is an append-only audit trail (same rationale as
        //    keeping `run_history` below).
        sqlx::query(
            "DELETE FROM inbox_entry \
             WHERE workspace_id = ?2 \
               AND (subject_id = ?1 \
                    OR subject_id IN (SELECT id FROM comment WHERE issue_id = ?1) \
                    OR subject_id IN (SELECT id FROM agent_task_queue WHERE issue_id = ?1))",
        )
        .bind(issue_id)
        .bind(workspace_id)
        .execute(&mut *tx)
        .await?;

        // 2. Preserve cost history: null the link from surviving run rows to the
        //    tasks about to vanish (run_history.task_id is a nullable FK).
        sqlx::query(
            "UPDATE run_history SET task_id = NULL \
             WHERE task_id IN (SELECT id FROM agent_task_queue WHERE issue_id = ?)",
        )
        .bind(issue_id)
        .execute(&mut *tx)
        .await?;

        // 3. The tasks' children: usage rows (FK) + task-kind beads correlations.
        sqlx::query(
            "DELETE FROM beads_mapping \
             WHERE hangar_kind = 'task' \
               AND hangar_id IN (SELECT id FROM agent_task_queue WHERE issue_id = ?)",
        )
        .bind(issue_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM task_usage \
             WHERE task_id IN (SELECT id FROM agent_task_queue WHERE issue_id = ?)",
        )
        .bind(issue_id)
        .execute(&mut *tx)
        .await?;

        // 4. The task rows themselves.
        sqlx::query("DELETE FROM agent_task_queue WHERE issue_id = ?")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;

        // 4b. The card's dispatch-attempt audit rows (migration 0058). The table
        //     carries no FK on `issue_id` on purpose (an audit row must survive a
        //     race with a concurrent delete), so the reap is explicit here — the
        //     same shape as the `agent_task_queue` delete above.
        crate::repo::dispatch_attempt::DispatchAttemptRepo::delete_for_issue(&mut tx, issue_id)
            .await?;

        // 4c. The card's activity-log narrative (migration 0059). Same shape and
        //     same rationale as 4b: no FK on `issue_id` by design, so the reap is
        //     explicit. The narrative is scoped to the card, so it dies with it.
        crate::repo::activity::ActivityRepo::delete_for_issue(&mut tx, issue_id).await?;

        // 5. The issue's directly-linked rows. First orphan any sub-issues: the
        //    `parent_issue_id` self-FK is `ON DELETE SET NULL` (migration 0046),
        //    so children are NULLed automatically at the final DELETE, but FK
        //    enforcement can be off on a given connection — this explicit UPDATE
        //    makes the orphaning deterministic (belt-and-braces), so deleting a
        //    parent never blocks and never leaves a dangling child link.
        sqlx::query("UPDATE issue SET parent_issue_id = NULL WHERE parent_issue_id = ?")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM issue_label WHERE issue_id = ?")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM comment WHERE issue_id = ?")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM board_card WHERE issue_id = ?")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "DELETE FROM card_dependency \
             WHERE dependent_issue_id = ? OR blocker_issue_id = ?",
        )
        .bind(issue_id)
        .bind(issue_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM beads_mapping WHERE hangar_kind = 'issue' AND hangar_id = ?")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;
        // 5b. The card's watchers and reactions (migration 0062). Neither table
        //     carries an FK on `issue_id` (0058's rule), so the reap is explicit
        //     here, exactly like `issue_label` / `comment` above.
        sqlx::query("DELETE FROM issue_subscriber WHERE issue_id = ?")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM issue_reaction WHERE issue_id = ?")
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;

        // 6. The issue row (workspace-scoped again, belt-and-braces).
        sqlx::query("DELETE FROM issue WHERE id = ? AND workspace_id = ?")
            .bind(issue_id)
            .bind(workspace_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(summary)
    }

    /// Set one acceptance criterion's checked state on one issue,
    /// workspace-scoped (multica parity #11-rest).
    ///
    /// Read-modify-write of the JSON column inside ONE transaction so two
    /// concurrent ticks cannot lose each other's write. The UPDATE additionally
    /// carries a compare-and-swap (`AND acceptance_criteria = <the exact text we
    /// read>`) so a lost update surfaces as [`CriterionError::Conflict`] instead
    /// of silently clobbering.
    ///
    /// Legacy bare-string elements are normalised to minted stable ids on the
    /// way through — the write-time half of the 0054 backfill.
    ///
    /// **Idempotent**: ticking an already-ticked criterion succeeds without
    /// bumping `checked_at`, so a retrying agent does not rewrite provenance.
    ///
    /// Returns the refreshed [`Issue`].
    ///
    /// # Errors
    ///
    /// - [`CriterionError::IssueNotFound`] — no issue with that id in that
    ///   workspace (unknown id OR cross-tenant).
    /// - [`CriterionError::CriterionNotFound`] — the issue carries no criterion
    ///   matching `criterion` (neither by id nor by 1-based ordinal).
    /// - [`CriterionError::Conflict`] — a concurrent write changed the column
    ///   between our read and our write.
    /// - [`CriterionError::Db`] — an underlying `sqlx` failure.
    pub async fn set_criterion_checked(
        pool: &SqlitePool,
        id_gen: &dyn IdGen,
        workspace_id: &str,
        issue_id: &str,
        criterion: &str,
        checked: bool,
        at: i64,
        by: Option<&str>,
    ) -> Result<Issue, CriterionError> {
        let mut tx = pool.begin().await?;

        let raw: Option<String> = sqlx::query_scalar(
            "SELECT acceptance_criteria FROM issue WHERE id = ? AND workspace_id = ?",
        )
        .bind(issue_id)
        .bind(workspace_id)
        .fetch_optional(&mut *tx)
        .await?;
        let raw = raw.ok_or(CriterionError::IssueNotFound)?;

        let mut items = criteria_from_json(&raw)
            .map_err(|e| CriterionError::Db(decode_err("acceptance_criteria", &e.to_string())))?;
        let idx =
            resolve_criterion_index(&items, criterion).ok_or(CriterionError::CriterionNotFound)?;

        // Normalise placeholder ids (legacy 0048 rows) so the row converges to
        // the structured shape on its first write.
        normalise_ids(&mut items, id_gen);
        if checked {
            items[idx].tick(at, by);
        } else {
            items[idx].untick();
        }
        let next = criteria_to_json(&items);

        let affected = sqlx::query(
            "UPDATE issue SET acceptance_criteria = ? \
             WHERE id = ? AND workspace_id = ? AND acceptance_criteria = ?",
        )
        .bind(&next)
        .bind(issue_id)
        .bind(workspace_id)
        .bind(&raw)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if affected == 0 && next != raw {
            return Err(CriterionError::Conflict);
        }

        tx.commit().await?;

        Self::get_by_id(pool, issue_id).await?.ok_or(CriterionError::IssueNotFound)
    }
}

/// Resolve a caller-supplied criterion selector against an issue's list: either
/// the exact `id`, or a 1-BASED ORDINAL (`"2"`).
///
/// Ordinals exist because an agent reading the detail card sees positions, not
/// ids. Re-exported from [`ainb_hangar_core::acceptance`] so the CLI and the
/// daemon reach the same addressing through the store seam they already use.
#[must_use]
pub fn resolve_criterion<'a>(
    items: &'a [AcceptanceCriterion],
    sel: &str,
) -> Option<&'a AcceptanceCriterion> {
    ainb_hangar_core::acceptance::resolve_criterion(items, sel)
}

/// Error surface for [`IssueRepo::set_criterion_checked`].
///
/// Splits tenant isolation, addressing, and lost-update detection from a raw
/// database failure, mirroring
/// [`LabelRepoError`](crate::repo::label::LabelRepoError).
#[derive(Debug, thiserror::Error)]
pub enum CriterionError {
    /// No issue with that id IN THAT WORKSPACE (unknown id or cross-tenant).
    /// Nothing is written.
    #[error("issue not found in this workspace")]
    IssueNotFound,
    /// The issue exists but carries no criterion with that id or ordinal.
    #[error("no acceptance criterion matches that id or ordinal")]
    CriterionNotFound,
    /// A concurrent write changed the column between our read and our write.
    #[error("criterion changed concurrently; re-read and retry")]
    Conflict,
    /// An underlying `sqlx` failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
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
    let acceptance_criteria = criteria_from_json(&row.try_get::<String, _>("acceptance_criteria")?)
        .map_err(|e| decode_err("acceptance_criteria", &e.to_string()))?;
    let context_refs = labels_from_json(&row.try_get::<String, _>("context_refs")?)?;
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
        acceptance_criteria,
        context_refs,
        external_ref: row.try_get("external_ref")?,
        parent_issue_id: row.try_get("parent_issue_id")?,
        stage: row.try_get("stage")?,
        // LENIENT by design (migration 0056): a stored kind outside the
        // allow-list degrades to `manual` instead of failing the whole row, so
        // a newer daemon's future kind cannot brick an older reader.
        origin: IssueOrigin::from_db(row.try_get("origin_type")?, row.try_get("origin_id")?),
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

    /// A fresh unchecked criterion with a caller-chosen (deterministic) id.
    fn criterion(id: &str, text: &str) -> AcceptanceCriterion {
        AcceptanceCriterion::with_id(id, text).unwrap()
    }

    /// A deterministic id generator for the normalise-on-write path.
    fn idgen(ids: &[&str]) -> ainb_hangar_core::idgen::FixedIdGen {
        ainb_hangar_core::idgen::FixedIdGen::new(
            ids.iter().map(std::string::ToString::to_string).collect(),
        )
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
                acceptance_criteria: Vec::new(),
                context_refs: Vec::new(),
                parent_issue_id: None,
                stage: None,
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

    /// `acceptance_criteria` + `context_refs` round-trip verbatim and
    /// order-preserving through insert → `get_by_id`, and a create without them
    /// reads back as empty (migration 0048 default `'[]'`).
    #[tokio::test]
    async fn acceptance_criteria_and_context_refs_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        IssueRepo::insert(
            pool,
            &NewIssue {
                id: "issue-ac".into(),
                workspace_id: "ws-a".into(),
                title: "Ship gap 11".into(),
                description: None,
                state: "open".into(),
                assignee: None,
                creator: member(),
                created_at: 1,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                acceptance_criteria: vec![
                    criterion("ac-1", "cargo build is green"),
                    criterion("ac-2", "detail card shows criteria"),
                ],
                context_refs: vec!["acme/api#42".into()],
                parent_issue_id: None,
                stage: None,
            },
        )
        .await
        .unwrap();

        let issue = IssueRepo::get_by_id(pool, "issue-ac").await.unwrap().unwrap();
        assert_eq!(
            issue
                .acceptance_criteria
                .iter()
                .map(|c| (c.id.as_str(), c.text.as_str(), c.checked))
                .collect::<Vec<_>>(),
            vec![
                ("ac-1", "cargo build is green", false),
                ("ac-2", "detail card shows criteria", false),
            ],
            "criteria survive verbatim, order-preserving, ids stable, unchecked"
        );
        assert_eq!(
            issue.context_refs,
            vec!["acme/api#42".to_string()],
            "context refs survive verbatim"
        );

        // A create without the two lists reads back as the empty default.
        seed_issue(pool, "ws-a", "issue-plain", "No lists", None, 2).await;
        let plain = IssueRepo::get_by_id(pool, "issue-plain").await.unwrap().unwrap();
        assert!(
            plain.acceptance_criteria.is_empty(),
            "default empty criteria"
        );
        assert!(plain.context_refs.is_empty(), "default empty context refs");
    }

    /// Seed one issue carrying `criteria` in `ws`.
    async fn seed_issue_with_criteria(
        pool: &SqlitePool,
        ws: &str,
        id: &str,
        criteria: Vec<AcceptanceCriterion>,
    ) {
        IssueRepo::insert(
            pool,
            &NewIssue {
                id: id.into(),
                workspace_id: ws.into(),
                title: "T".into(),
                description: None,
                state: "open".into(),
                assignee: None,
                creator: member(),
                created_at: 1,
                priority: 0,
                due_date: None,
                labels: Vec::new(),
                acceptance_criteria: criteria,
                context_refs: Vec::new(),
                parent_issue_id: None,
                stage: None,
            },
        )
        .await
        .unwrap();
    }

    /// Read the raw `acceptance_criteria` column text (what actually hit disk).
    async fn raw_acceptance(pool: &SqlitePool, id: &str) -> String {
        sqlx::query_scalar::<_, String>("SELECT acceptance_criteria FROM issue WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// **T1** — the sqlite half of the #11-rest acceptance: a tick survives a
    /// pool re-open, ids are stable, and the on-disk shape is an OBJECT array
    /// carrying `"checked":true`.
    #[tokio::test]
    async fn criterion_tick_persists_to_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open_in(dir.path()).await.unwrap();
            let pool = store.pool();
            seed_ws(pool, "ws-a").await;
            seed_issue_with_criteria(
                pool,
                "ws-a",
                "issue-1",
                vec![criterion("ac-a", "builds"), criterion("ac-b", "tests")],
            )
            .await;

            let updated = IssueRepo::set_criterion_checked(
                pool,
                &idgen(&[]),
                "ws-a",
                "issue-1",
                "ac-b",
                true,
                1_700,
                Some("agent:builder"),
            )
            .await
            .expect("tick succeeds");
            assert!(!updated.acceptance_criteria[0].checked);
            assert!(updated.acceptance_criteria[1].checked);
        }

        // Re-open the pool: the state came off disk, not out of memory.
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let issue = IssueRepo::get_by_id(pool, "issue-1").await.unwrap().unwrap();
        assert_eq!(
            issue
                .acceptance_criteria
                .iter()
                .map(|c| (c.id.as_str(), c.text.as_str(), c.checked))
                .collect::<Vec<_>>(),
            vec![("ac-a", "builds", false), ("ac-b", "tests", true)],
            "ids stable, order preserved, only the ticked one is checked"
        );
        assert_eq!(issue.acceptance_criteria[1].checked_at, Some(1_700));
        assert_eq!(
            issue.acceptance_criteria[1].checked_by.as_deref(),
            Some("agent:builder")
        );

        let raw = raw_acceptance(pool, "issue-1").await;
        assert!(
            raw.starts_with("[{"),
            "column holds an OBJECT array, got {raw}"
        );
        assert!(
            raw.contains(r#""id":"ac-b","text":"tests","checked":true"#),
            "structured per-criterion state on disk, got {raw}"
        );
        assert!(
            raw.contains(r#""id":"ac-a","text":"builds","checked":false"#),
            "the sibling stayed unchecked on disk, got {raw}"
        );
    }

    /// **T2** — ticking twice does not rewrite provenance; unticking clears BOTH
    /// `checked_at` and `checked_by`.
    #[tokio::test]
    async fn criterion_tick_is_idempotent_and_untick_clears_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue_with_criteria(pool, "ws-a", "issue-1", vec![criterion("ac-a", "builds")]).await;

        let tick = |at: i64, by: &'static str| async move {
            IssueRepo::set_criterion_checked(
                pool,
                &idgen(&[]),
                "ws-a",
                "issue-1",
                "ac-a",
                true,
                at,
                Some(by),
            )
            .await
            .expect("tick succeeds")
        };

        let first = tick(100, "agent:builder").await;
        assert_eq!(first.acceptance_criteria[0].checked_at, Some(100));

        let second = tick(999, "agent:other").await;
        assert_eq!(
            second.acceptance_criteria[0].checked_at,
            Some(100),
            "a retry must not rewrite when it was ticked"
        );
        assert_eq!(
            second.acceptance_criteria[0].checked_by.as_deref(),
            Some("agent:builder"),
            "a retry must not rewrite who ticked it"
        );

        let cleared = IssueRepo::set_criterion_checked(
            pool,
            &idgen(&[]),
            "ws-a",
            "issue-1",
            "ac-a",
            false,
            1_234,
            Some("agent:third"),
        )
        .await
        .expect("untick succeeds");
        assert!(!cleared.acceptance_criteria[0].checked);
        assert_eq!(cleared.acceptance_criteria[0].checked_at, None);
        assert_eq!(cleared.acceptance_criteria[0].checked_by, None);
        let raw = raw_acceptance(pool, "issue-1").await;
        assert!(
            !raw.contains("checked_at") && !raw.contains("checked_by"),
            "untick leaves no stale provenance on disk, got {raw}"
        );
    }

    /// **T3** — a pre-0054 row written as bare strings decodes tolerantly, and
    /// the first tick normalises the WHOLE list to minted stable ids while
    /// preserving every text.
    #[tokio::test]
    async fn legacy_string_criteria_decode_and_normalise() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue_with_criteria(pool, "ws-a", "issue-1", Vec::new()).await;

        // Force the column back to the legacy 0048 shape.
        sqlx::query("UPDATE issue SET acceptance_criteria = ? WHERE id = ?")
            .bind(r#"["builds","tests"]"#)
            .bind("issue-1")
            .execute(pool)
            .await
            .unwrap();

        let read = IssueRepo::get_by_id(pool, "issue-1").await.unwrap().unwrap();
        assert_eq!(
            read.acceptance_criteria.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["builds", "tests"],
            "legacy strings decode tolerantly"
        );
        assert!(read.acceptance_criteria.iter().all(|c| !c.checked));

        // Tick by ORDINAL — an agent reading the card sees positions, not ids.
        let updated = IssueRepo::set_criterion_checked(
            pool,
            &idgen(&["MINT1", "MINT2"]),
            "ws-a",
            "issue-1",
            "2",
            true,
            50,
            None,
        )
        .await
        .expect("tick by ordinal succeeds");

        assert_eq!(
            updated
                .acceptance_criteria
                .iter()
                .map(|c| (c.id.as_str(), c.text.as_str(), c.checked))
                .collect::<Vec<_>>(),
            vec![("ac-mint1", "builds", false), ("ac-mint2", "tests", true)],
            "ids minted for BOTH elements; the untouched sibling kept its text"
        );
        let raw = raw_acceptance(pool, "issue-1").await;
        assert!(
            raw.starts_with("[{") && !raw.contains("ac-legacy-"),
            "normalised to objects with minted ids on disk, got {raw}"
        );
    }

    /// **T4** — an issue id from workspace B addressed with workspace A is
    /// rejected and writes NOTHING.
    #[tokio::test]
    async fn criterion_set_cross_tenant_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_issue_with_criteria(pool, "ws-b", "issue-b", vec![criterion("ac-a", "builds")]).await;

        let before = raw_acceptance(pool, "issue-b").await;
        let err = IssueRepo::set_criterion_checked(
            pool,
            &idgen(&[]),
            "ws-a",
            "issue-b",
            "ac-a",
            true,
            10,
            None,
        )
        .await
        .expect_err("cross-tenant tick must be rejected");
        assert!(
            matches!(err, CriterionError::IssueNotFound),
            "got {err:?}, want IssueNotFound"
        );
        assert_eq!(
            raw_acceptance(pool, "issue-b").await,
            before,
            "zero rows changed"
        );

        // An unknown criterion selector on a legitimately-owned issue is a
        // distinct error, not a silent no-op.
        let err = IssueRepo::set_criterion_checked(
            pool,
            &idgen(&[]),
            "ws-b",
            "issue-b",
            "ac-nope",
            true,
            10,
            None,
        )
        .await
        .expect_err("unknown criterion must be rejected");
        assert!(
            matches!(err, CriterionError::CriterionNotFound),
            "got {err:?}, want CriterionNotFound"
        );
        // An out-of-range ordinal likewise.
        assert!(matches!(
            IssueRepo::set_criterion_checked(
                pool,
                &idgen(&[]),
                "ws-b",
                "issue-b",
                "7",
                true,
                10,
                None
            )
            .await
            .expect_err("out-of-range ordinal must be rejected"),
            CriterionError::CriterionNotFound
        ));
        assert_eq!(raw_acceptance(pool, "issue-b").await, before);
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

    // ---- ORIGIN PROVENANCE (migration 0056, multica parity #21) -------------

    /// Every column an unstamped (pre-0056-shaped) create leaves behind reads
    /// back as "provenance unknown" — NOT as an implicit `manual`.
    #[tokio::test]
    async fn an_unstamped_issue_reads_back_with_no_origin() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i-1", "t", None, 1).await;

        let issue = IssueRepo::get_by_id(pool, "i-1").await.unwrap().unwrap();
        assert_eq!(issue.origin, None);
    }

    /// The post-insert setter writes both columns, and the read side
    /// re-assembles the typed pair — through `get_by_id`, `list_by_workspace`
    /// AND `search_ranked`, so no SELECT list is left behind.
    #[tokio::test]
    async fn set_origin_round_trips_through_every_read_surface() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i-1", "keyword title", None, 1).await;

        let origin = IssueOrigin::autopilot("ap-9").unwrap();
        assert!(IssueRepo::set_origin(pool, "ws-a", "i-1", &origin).await.unwrap());

        // Raw SQL — the acceptance shape (`SELECT origin_type, origin_id`).
        let (kind, id): (String, Option<String>) =
            sqlx::query_as("SELECT origin_type, origin_id FROM issue WHERE id = 'i-1'")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!((kind.as_str(), id.as_deref()), ("autopilot", Some("ap-9")));

        assert_eq!(
            IssueRepo::get_by_id(pool, "i-1").await.unwrap().unwrap().origin,
            Some(origin.clone())
        );
        let listed = IssueRepo::list_by_workspace_state(pool, "ws-a", "open").await.unwrap();
        assert_eq!(listed[0].origin, Some(origin.clone()));
        let found = IssueRepo::search_ranked(pool, "ws-a", "keyword").await.unwrap();
        assert_eq!(found[0].origin, Some(origin));
    }

    /// A `manual` stamp writes the kind and leaves `origin_id` NULL — the
    /// column becomes a complete record ("a human authored it"), distinct from
    /// the NULL kind a pre-0056 row keeps.
    #[tokio::test]
    async fn manual_stamps_the_kind_and_a_null_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i-1", "t", None, 1).await;

        IssueRepo::set_origin(pool, "ws-a", "i-1", &IssueOrigin::manual())
            .await
            .unwrap();

        let (kind, id): (String, Option<String>) =
            sqlx::query_as("SELECT origin_type, origin_id FROM issue WHERE id = 'i-1'")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(kind, "manual");
        assert_eq!(id, None);
    }

    /// The setter is workspace-scoped: a foreign tenant's id stamps nothing.
    #[tokio::test]
    async fn set_origin_never_crosses_a_tenant_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_issue(pool, "ws-a", "i-1", "t", None, 1).await;

        let stamped = IssueRepo::set_origin(
            pool,
            "ws-b",
            "i-1",
            &IssueOrigin::comment_mention("c-1").unwrap(),
        )
        .await
        .unwrap();
        assert!(!stamped, "a foreign-tenant stamp matches zero rows");
        assert_eq!(
            IssueRepo::get_by_id(pool, "i-1").await.unwrap().unwrap().origin,
            None
        );
    }

    /// The DETERMINISTIC lookup multica added the pair for
    /// (`GetIssueByOrigin`): the right issue for the right provenance id, and
    /// `None` for a different id — never "the agent's most recent issue".
    #[tokio::test]
    async fn by_origin_resolves_the_stamped_issue_and_only_that_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i-1", "first", None, 1).await;
        seed_issue(pool, "ws-a", "i-2", "second", None, 2).await;

        let mine = IssueOrigin::comment_mention("c-42").unwrap();
        IssueRepo::set_origin(pool, "ws-a", "i-1", &mine).await.unwrap();
        // i-2 is the agent's MORE RECENT issue but a different provenance.
        IssueRepo::set_origin(
            pool,
            "ws-a",
            "i-2",
            &IssueOrigin::comment_mention("c-99").unwrap(),
        )
        .await
        .unwrap();

        let hit = IssueRepo::by_origin(pool, "ws-a", &mine).await.unwrap().unwrap();
        assert_eq!(hit.id, "i-1", "resolved by provenance, not by recency");

        let miss = IssueOrigin::comment_mention("c-nope").unwrap();
        assert!(IssueRepo::by_origin(pool, "ws-a", &miss).await.unwrap().is_none());
        assert!(
            IssueRepo::by_origin(pool, "ws-b", &mine).await.unwrap().is_none(),
            "workspace-scoped"
        );
    }

    /// The "which issues did the platform create" list filter.
    #[tokio::test]
    async fn list_by_origin_kind_filters_to_that_kind_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i-1", "a", None, 1).await;
        seed_issue(pool, "ws-a", "i-2", "b", None, 2).await;
        seed_issue(pool, "ws-a", "i-3", "c", None, 3).await;

        IssueRepo::set_origin(
            pool,
            "ws-a",
            "i-1",
            &IssueOrigin::autopilot("ap-1").unwrap(),
        )
        .await
        .unwrap();
        IssueRepo::set_origin(pool, "ws-a", "i-2", &IssueOrigin::manual())
            .await
            .unwrap();
        IssueRepo::set_origin(
            pool,
            "ws-a",
            "i-3",
            &IssueOrigin::autopilot("ap-2").unwrap(),
        )
        .await
        .unwrap();

        let fired = IssueRepo::list_by_origin_kind(pool, "ws-a", OriginKind::Autopilot)
            .await
            .unwrap();
        let ids: Vec<&str> = fired.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["i-1", "i-3"]);
        // The unstamped row is in neither bucket.
        let manual =
            IssueRepo::list_by_origin_kind(pool, "ws-a", OriginKind::Manual).await.unwrap();
        assert_eq!(manual.len(), 1);
    }

    /// A kind written outside the allow-list (only reachable through raw SQL /
    /// a newer daemon) must not fail the read — it degrades to `manual`.
    #[tokio::test]
    async fn an_unknown_stored_kind_degrades_instead_of_failing_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "i-1", "t", None, 1).await;
        sqlx::query(
            "UPDATE issue SET origin_type = 'from_the_future', origin_id = 'x' WHERE id = 'i-1'",
        )
        .execute(pool)
        .await
        .unwrap();

        let issue = IssueRepo::get_by_id(pool, "i-1").await.unwrap().unwrap();
        assert_eq!(issue.origin.unwrap().kind(), OriginKind::Manual);
    }
}
