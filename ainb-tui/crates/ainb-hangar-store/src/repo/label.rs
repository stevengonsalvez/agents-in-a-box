//! Typed repository wrapper over the issue-label tables (e38.10).
//!
//! Covers `label` (the workspace-scoped definition) and `issue_label` (the
//! attach join). A [`Label`] is a `(workspace_id, name)` pair plus an optional
//! `color`; it attaches to an issue through the `issue_label` join. Like
//! [`SkillRepo`](crate::repo::skill::SkillRepo) this is a thin, stateless sqlx
//! wrapper sharing the single [`crate::Store`] pool.
//!
//! # The `label` table is the source of truth; `issue.labels` is a cache
//!
//! Migration 0014 added a denormalized `issue.labels` JSON-array column — the
//! minimal persistence the create flow needed. Migration 0016 promoted labels to
//! this first-class entity. The `label` + `issue_label` tables are now the SOURCE
//! OF TRUTH; the `issue.labels` JSON column is kept as a denormalized read-cache
//! that [`attach`](LabelRepo::attach) / [`detach`](LabelRepo::detach) keep in
//! sync inside the same transaction. The `hangar/issues_list` snapshot reads the
//! JSON cache straight into `IssueRow.labels` (which the plugin chips render), so
//! a label mutation that did not also rewrite the cache would make the list go
//! stale — hence every mutation re-derives the cache from the join before it
//! commits.
//!
//! # Workspace scoping
//!
//! **Every mutation takes a [`WorkspaceId`] and enforces it in SQL.** The
//! `issue_label` join carries no `workspace_id` of its own (mirroring `comment`,
//! migration 0003); tenant isolation is enforced by resolving both the issue and
//! the label through their owning workspace before touching the join:
//!
//! - the issue must satisfy `WHERE id = ? AND workspace_id = ?` — a
//!   foreign-tenant issue id resolves to nothing and the mutation is rejected
//!   with [`LabelRepoError::IssueNotFound`] (never a cross-tenant attach);
//! - the label is resolved (or, on attach, created) by `(workspace_id, name)`,
//!   so a label name always binds to the caller's tenant.
//!
//! `PRAGMA foreign_keys` is off in this crate, so the `(workspace_id, name)`
//! UNIQUE index (`idx_label_workspace_name`, migration 0016) and the
//! `(issue_id, label_id)` composite PK are the engine-enforced invariants;
//! referential integrity of the join (and its cascade on issue/label deletion)
//! is enforced here in application code.

use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::WorkspaceId;
use sqlx::SqlitePool;

/// A workspace-scoped label definition (the `label` table).
///
/// `color` is an optional presentation hint (a hex string such as `#ff0000`);
/// `None` when the label carries no colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Label name (unique within the workspace).
    pub name: String,
    /// Optional presentation colour (hex), `None` when unset.
    pub color: Option<String>,
}

/// Stateless typed wrapper over the `label` + `issue_label` tables.
pub struct LabelRepo;

impl LabelRepo {
    /// Attach the label `name` to `issue_id` within `workspace` (idempotent).
    ///
    /// Resolve-or-create: if a label `(workspace, name)` already exists it is
    /// reused (its `color` left as-is — a bare attach never overwrites an
    /// existing label's colour); otherwise a fresh label row is minted with the
    /// supplied `color`. The attach itself is `INSERT ... ON CONFLICT
    /// (issue_id, label_id) DO NOTHING`, so calling it twice leaves exactly one
    /// join row. After the join is written the issue's `labels` JSON cache is
    /// re-derived from the join so `issues_list` stays correct. The whole flow
    /// runs in a single transaction.
    ///
    /// Workspace-scoped: the issue must belong to `workspace` (else
    /// [`LabelRepoError::IssueNotFound`] and nothing is written) — a caller
    /// holding an issue id minted in another tenant can never forge a join row.
    ///
    /// # Errors
    ///
    /// Returns [`LabelRepoError::IssueNotFound`] when the issue is not in
    /// `workspace`, or [`LabelRepoError::Db`] on a store failure.
    pub async fn attach(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        name: &str,
        color: Option<&str>,
    ) -> Result<(), LabelRepoError> {
        let mut tx = pool.begin().await?;
        ensure_issue_in_workspace(&mut tx, workspace, issue_id).await?;

        // Resolve-or-create the label by (workspace, name). ON CONFLICT keeps the
        // existing row (a bare attach must not clobber an established colour), so
        // RETURNING hands back whichever id won — stable across re-attaches.
        let label_id: String = sqlx::query_scalar(
            "INSERT INTO label (id, workspace_id, name, color) VALUES (?, ?, ?, ?) \
             ON CONFLICT (workspace_id, name) DO UPDATE SET name = excluded.name \
             RETURNING id",
        )
        .bind(SystemIdGen.new_ulid())
        .bind(workspace.as_str())
        .bind(name)
        .bind(color)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO issue_label (issue_id, label_id) VALUES (?, ?) \
             ON CONFLICT (issue_id, label_id) DO NOTHING",
        )
        .bind(issue_id)
        .bind(&label_id)
        .execute(&mut *tx)
        .await?;

        sync_labels_cache(&mut tx, issue_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Detach the label `name` from `issue_id` within `workspace` (idempotent).
    ///
    /// Removing an absent link (an unknown label name, or a label that was never
    /// attached) affects zero join rows — not an error, so detach is idempotent.
    /// After the join is touched the issue's `labels` JSON cache is re-derived so
    /// `issues_list` stays correct. The label definition itself is left intact
    /// (a label can be shared across issues); only the join is removed.
    ///
    /// Workspace-scoped like [`attach`](Self::attach): the issue must belong to
    /// `workspace` (else [`LabelRepoError::IssueNotFound`] and nothing is
    /// deleted), and the label is resolved by `(workspace, name)`.
    ///
    /// # Errors
    ///
    /// Returns [`LabelRepoError::IssueNotFound`] when the issue is not in
    /// `workspace`, or [`LabelRepoError::Db`] on a store failure.
    pub async fn detach(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        name: &str,
    ) -> Result<(), LabelRepoError> {
        let mut tx = pool.begin().await?;
        ensure_issue_in_workspace(&mut tx, workspace, issue_id).await?;

        // Resolve the label by (workspace, name). An unknown name leaves the
        // join untouched (idempotent detach) but still re-syncs the cache below.
        let label_id: Option<String> =
            sqlx::query_scalar("SELECT id FROM label WHERE workspace_id = ? AND name = ?")
                .bind(workspace.as_str())
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(label_id) = label_id {
            sqlx::query("DELETE FROM issue_label WHERE issue_id = ? AND label_id = ?")
                .bind(issue_id)
                .bind(&label_id)
                .execute(&mut *tx)
                .await?;
        }

        sync_labels_cache(&mut tx, issue_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// List the label names attached to `issue_id` within `workspace`, ordered
    /// by name (alphabetical, the chip render order).
    ///
    /// Workspace-scoped through a join to `label`: a foreign-tenant issue id (or
    /// one with no labels) yields an empty list, never another tenant's labels.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn labels_for_issue(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT l.name FROM label l \
             JOIN issue_label il ON il.label_id = l.id \
             WHERE il.issue_id = ? AND l.workspace_id = ? \
             ORDER BY l.name",
        )
        .bind(issue_id)
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await
    }

    /// List every label defined in `workspace`, ordered by name.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Vec<Label>, sqlx::Error> {
        sqlx::query_as::<_, Label>(
            "SELECT id, workspace_id, name, color FROM label \
             WHERE workspace_id = ? ORDER BY name",
        )
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await
    }
}

/// Verify `issue_id` belongs to `workspace`, erroring with
/// [`LabelRepoError::IssueNotFound`] otherwise (which also covers a
/// non-existent id). Run inside the mutation's transaction so the check and the
/// write are atomic.
async fn ensure_issue_in_workspace(
    tx: &mut sqlx::SqliteConnection,
    workspace: &WorkspaceId,
    issue_id: &str,
) -> Result<(), LabelRepoError> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT id FROM issue WHERE id = ? AND workspace_id = ?")
            .bind(issue_id)
            .bind(workspace.as_str())
            .fetch_optional(&mut *tx)
            .await?;
    if found.is_none() {
        return Err(LabelRepoError::IssueNotFound);
    }
    Ok(())
}

/// Re-derive `issue_id`'s `labels` JSON cache from the `issue_label` join and
/// write it back, keeping the denormalized column the `issues_list` snapshot
/// reads consistent with the source-of-truth join. The names are ordered
/// alphabetically (the same order [`LabelRepo::labels_for_issue`] returns) so the
/// cache is deterministic. Run inside the mutation's transaction.
async fn sync_labels_cache(
    tx: &mut sqlx::SqliteConnection,
    issue_id: &str,
) -> Result<(), LabelRepoError> {
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT l.name FROM label l \
         JOIN issue_label il ON il.label_id = l.id \
         WHERE il.issue_id = ? \
         ORDER BY l.name",
    )
    .bind(issue_id)
    .fetch_all(&mut *tx)
    .await?;
    // serde_json::to_string of a Vec<String> is infallible (no map keys, no
    // non-finite floats); the fallback keeps the call panic-free and mirrors the
    // empty-array default the schema writes for a label-less issue.
    let json = serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string());
    sqlx::query("UPDATE issue SET labels = ? WHERE id = ?")
        .bind(json)
        .bind(issue_id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Error surface for [`LabelRepo`].
///
/// Splits a tenant-isolation rejection (the issue is not in the caller's
/// workspace) from a database failure, mirroring
/// [`SkillRepoError`](crate::repo::skill::SkillRepoError).
#[derive(Debug, thiserror::Error)]
pub enum LabelRepoError {
    /// The target issue does not belong to the supplied workspace (covers a
    /// non-existent id too). The mutation is rejected, nothing written — the
    /// tenant-isolation guard against a cross-tenant attach/detach.
    #[error("issue not found in this workspace")]
    IssueNotFound,
    /// An underlying `sqlx` failure (FK violation, uniqueness conflict, IO).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for Label {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            name: row.try_get("name")?,
            color: row.try_get("color")?,
        })
    }
}
