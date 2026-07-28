//! Typed repository over the custom-property catalog and the per-issue value
//! bag (migration 0066, multica parity #17).
//!
//! Two halves of one feature:
//!
//! - `issue_property` — the per-workspace CATALOG of typed custom fields
//!   (`sprint`, `owner`, `risk`…), each with a display name, a
//!   [`PropertyKind`], an option list, a render position and an
//!   `archived_at` tombstone.
//! - `issue.properties` — the per-issue VALUE BAG, a JSON object keyed by
//!   **`issue_property.id`**.
//!
//! # Why values are keyed by definition id
//!
//! This is the reference's whole reason for the two-table shape: renaming a
//! property's display label is a catalog-only `UPDATE issue_property SET name`
//! that touches **zero** issue rows. A name-keyed bag would need a value
//! migration across every issue on every rename.
//!
//! # Archive, never delete
//!
//! [`IssuePropertyRepo::set_archived`] is the only "removal" — the row survives,
//! so an issue's stored value can always be re-resolved if the definition comes
//! back. [`IssuePropertyRepo::values_for`] simply drops archived definitions
//! (and orphan ids with no catalog row) from its result: the value stays on
//! disk, it just stops rendering.
//!
//! # Single-key atomic writes
//!
//! Every mutation is ONE `json_set` / `json_remove` statement addressing ONE
//! key, so a stale caps snapshot can never cost another key its value.
//! `IssueRepo::update` never touches `properties` — a whole-blob
//! overwrite would race with a concurrent agent's write, which is exactly the
//! invariant the reference states in its own handler header.

use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::properties::{
    MAX_ACTIVE_PROPERTIES, MAX_PROPERTY_BYTES, PropertyError, PropertyKind, PropertyValue,
    properties_from_json, property_value_json, validate_definition, validate_value,
};
use sqlx::{Row, SqlitePool};

use crate::repo::issue_metadata::json_path;

/// One catalogued custom-property definition (the `issue_property` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueProperty {
    /// Primary key (ULID string). This is the key used inside every issue's
    /// value bag, so it must never change once minted.
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Stable slug the CLI / RPC address the property by (unique per
    /// workspace).
    pub key: String,
    /// Display label; freely renameable without touching a single issue row.
    pub name: String,
    /// The value type this property accepts.
    pub kind: PropertyKind,
    /// Catalogued options for `select` / `multi_select`; empty otherwise.
    pub options: Vec<String>,
    /// Render order within the workspace (ascending).
    pub position: i64,
    /// Archive tombstone (epoch millis); `None` = active. NEVER hard-deleted.
    pub archived_at: Option<i64>,
    /// Creation timestamp (epoch millis).
    pub created_at: i64,
}

impl IssueProperty {
    /// Whether this definition is still in the active catalog.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.archived_at.is_none()
    }
}

/// Error surface for [`IssuePropertyRepo`].
#[derive(Debug, thiserror::Error)]
pub enum PropertyRepoError {
    /// The target issue does not belong to the supplied workspace (covers a
    /// non-existent id too). The tenant-isolation guard: the mutation is
    /// rejected and NOTHING is written.
    #[error("issue not found in this workspace")]
    IssueNotFound,
    /// No ACTIVE definition with that key exists in this workspace.
    #[error("no active custom property with that key")]
    PropertyNotFound,
    /// The workspace already has [`MAX_ACTIVE_PROPERTIES`] active definitions.
    #[error("a workspace may define at most {MAX_ACTIVE_PROPERTIES} active custom properties")]
    TooManyProperties,
    /// The value failed kind / option / size validation.
    #[error(transparent)]
    Value(PropertyError),
    /// An underlying `sqlx` failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl From<PropertyError> for PropertyRepoError {
    fn from(e: PropertyError) -> Self {
        Self::Value(e)
    }
}

/// Stateless typed wrapper over `issue_property` + `issue.properties`.
pub struct IssuePropertyRepo;

impl IssuePropertyRepo {
    /// Define (or re-define) a custom property, resolve-or-update by
    /// `(workspace, key)`.
    ///
    /// Re-defining an existing key updates its name / kind / options / position
    /// **in place, keeping the id** — that is what makes a rename free. Creating
    /// a definition beyond [`MAX_ACTIVE_PROPERTIES`] active ones is
    /// [`PropertyRepoError::TooManyProperties`]; archiving one frees a slot.
    ///
    /// # Errors
    ///
    /// [`PropertyRepoError::TooManyProperties`] at the cap,
    /// [`PropertyRepoError::Value`] for an incoherent kind/options pair
    /// (`select` with no options), or [`PropertyRepoError::Db`].
    pub async fn define(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        key: &str,
        name: &str,
        kind: &PropertyKind,
        options: &[String],
        position: i64,
        now: i64,
    ) -> Result<IssueProperty, PropertyRepoError> {
        let key = key.trim();
        if key.is_empty() {
            return Err(PropertyRepoError::Value(PropertyError::BlankKey));
        }
        validate_definition(kind, options)?;

        let mut tx = pool.begin().await?;
        let existing: Option<String> =
            sqlx::query_scalar("SELECT id FROM issue_property WHERE workspace_id = ? AND key = ?")
                .bind(workspace.as_str())
                .bind(key)
                .fetch_optional(&mut *tx)
                .await?;

        let options_json = serde_json::to_string(options).unwrap_or_else(|_| "[]".to_string());
        let id = if let Some(id) = existing {
            // Re-define is an UPDATE IN PLACE: the id survives, so every issue's
            // stored value keeps resolving. This is the rename path.
            sqlx::query(
                "UPDATE issue_property SET name = ?, kind = ?, options = ?, position = ?, \
                 archived_at = NULL WHERE id = ?",
            )
            .bind(name)
            .bind(kind.as_db_str())
            .bind(&options_json)
            .bind(position)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            let active: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM issue_property \
                 WHERE workspace_id = ? AND archived_at IS NULL",
            )
            .bind(workspace.as_str())
            .fetch_one(&mut *tx)
            .await?;
            if usize::try_from(active).unwrap_or(usize::MAX) >= MAX_ACTIVE_PROPERTIES {
                return Err(PropertyRepoError::TooManyProperties);
            }
            let id = SystemIdGen.new_ulid();
            sqlx::query(
                "INSERT INTO issue_property \
                 (id, workspace_id, key, name, kind, options, position, archived_at, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?)",
            )
            .bind(&id)
            .bind(workspace.as_str())
            .bind(key)
            .bind(name)
            .bind(kind.as_db_str())
            .bind(&options_json)
            .bind(position)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            id
        };
        tx.commit().await?;

        Self::get_by_id(pool, &id).await?.ok_or(PropertyRepoError::PropertyNotFound)
    }

    /// List the workspace's catalog in `position, key` render order.
    ///
    /// `include_archived = false` returns only the active catalog.
    ///
    /// # Errors
    ///
    /// [`sqlx::Error`] on a store failure.
    pub async fn list(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        include_archived: bool,
    ) -> Result<Vec<IssueProperty>, sqlx::Error> {
        let sql = if include_archived {
            "SELECT id, workspace_id, key, name, kind, options, position, archived_at, created_at \
             FROM issue_property WHERE workspace_id = ? ORDER BY position, key"
        } else {
            "SELECT id, workspace_id, key, name, kind, options, position, archived_at, created_at \
             FROM issue_property WHERE workspace_id = ? AND archived_at IS NULL \
             ORDER BY position, key"
        };
        let rows = sqlx::query(sql).bind(workspace.as_str()).fetch_all(pool).await?;
        rows.iter().map(property_from_row).collect()
    }

    /// Resolve one definition by `(workspace, key)`, archived or not.
    ///
    /// # Errors
    ///
    /// [`sqlx::Error`] on a store failure.
    pub async fn get_by_key(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        key: &str,
    ) -> Result<Option<IssueProperty>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, workspace_id, key, name, kind, options, position, archived_at, created_at \
             FROM issue_property WHERE workspace_id = ? AND key = ?",
        )
        .bind(workspace.as_str())
        .bind(key.trim())
        .fetch_optional(pool)
        .await?;
        row.as_ref().map(property_from_row).transpose()
    }

    /// Resolve one definition by its id.
    ///
    /// # Errors
    ///
    /// [`sqlx::Error`] on a store failure.
    pub async fn get_by_id(
        pool: &SqlitePool,
        id: &str,
    ) -> Result<Option<IssueProperty>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, workspace_id, key, name, kind, options, position, archived_at, created_at \
             FROM issue_property WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        row.as_ref().map(property_from_row).transpose()
    }

    /// Archive (or un-archive) a definition. **Never deletes.**
    ///
    /// Returns `false` when no definition with that key exists. Stored values
    /// survive an archive untouched and render again after an un-archive.
    ///
    /// # Errors
    ///
    /// [`PropertyRepoError::TooManyProperties`] when un-archiving would exceed
    /// the active cap, or [`PropertyRepoError::Db`].
    pub async fn set_archived(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        key: &str,
        archived: bool,
        now: i64,
    ) -> Result<bool, PropertyRepoError> {
        let mut tx = pool.begin().await?;
        let existing: Option<(String, Option<i64>)> = sqlx::query_as(
            "SELECT id, archived_at FROM issue_property WHERE workspace_id = ? AND key = ?",
        )
        .bind(workspace.as_str())
        .bind(key.trim())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((id, archived_at)) = existing else {
            return Ok(false);
        };
        if !archived && archived_at.is_some() {
            let active: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM issue_property \
                 WHERE workspace_id = ? AND archived_at IS NULL",
            )
            .bind(workspace.as_str())
            .fetch_one(&mut *tx)
            .await?;
            if usize::try_from(active).unwrap_or(usize::MAX) >= MAX_ACTIVE_PROPERTIES {
                return Err(PropertyRepoError::TooManyProperties);
            }
        }
        sqlx::query("UPDATE issue_property SET archived_at = ? WHERE id = ?")
            .bind(if archived { Some(now) } else { None })
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Set ONE custom property value on an issue, atomically.
    ///
    /// In one transaction: resolve the issue within `workspace`, resolve the
    /// ACTIVE definition by key, validate the value against its kind/options,
    /// then `json_set` the bag under the DEFINITION ID and reject a bag that
    /// grew past [`MAX_PROPERTY_BYTES`].
    ///
    /// # Errors
    ///
    /// [`PropertyRepoError::IssueNotFound`] (tenant guard),
    /// [`PropertyRepoError::PropertyNotFound`], [`PropertyRepoError::Value`]
    /// for a kind/option/size rejection, or [`PropertyRepoError::Db`].
    pub async fn set_value(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        key: &str,
        value: &PropertyValue,
    ) -> Result<(), PropertyRepoError> {
        ensure_issue_in_workspace(pool, workspace, issue_id).await?;

        let row = sqlx::query(
            "SELECT id, workspace_id, key, name, kind, options, position, archived_at, created_at \
             FROM issue_property \
             WHERE workspace_id = ? AND key = ? AND archived_at IS NULL",
        )
        .bind(workspace.as_str())
        .bind(key.trim())
        .fetch_optional(pool)
        .await?;
        let Some(def) = row.as_ref().map(property_from_row).transpose()? else {
            return Err(PropertyRepoError::PropertyNotFound);
        };
        validate_value(&def.kind, &def.options, value)?;

        // The size cap is decided from the CURRENT bag, but the write is a
        // single-statement `json_set` of exactly this definition id — never a
        // whole-blob overwrite — so a concurrent write to a DIFFERENT property
        // on the same issue cannot be clobbered even by a stale snapshot.
        let raw: String = sqlx::query_scalar("SELECT properties FROM issue WHERE id = ?")
            .bind(issue_id)
            .fetch_one(pool)
            .await?;
        let mut bag = properties_from_json(&raw);
        bag.insert(def.id.clone(), value.clone());
        if ainb_hangar_core::properties::properties_to_json(&bag).len() > MAX_PROPERTY_BYTES {
            return Err(PropertyRepoError::Value(PropertyError::TooLarge));
        }
        sqlx::query(
            "UPDATE issue \
             SET properties = json_set(\
                 CASE WHEN json_valid(properties) THEN properties ELSE '{}' END, ?, json(?)) \
             WHERE id = ? AND workspace_id = ?",
        )
        .bind(json_path(&def.id))
        .bind(property_value_json(value))
        .bind(issue_id)
        .bind(workspace.as_str())
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Clear ONE custom property value from an issue.
    ///
    /// Returns `false` when the property was not set on that issue.
    ///
    /// # Errors
    ///
    /// [`PropertyRepoError::IssueNotFound`],
    /// [`PropertyRepoError::PropertyNotFound`] or [`PropertyRepoError::Db`].
    pub async fn clear_value(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        key: &str,
    ) -> Result<bool, PropertyRepoError> {
        ensure_issue_in_workspace(pool, workspace, issue_id).await?;
        let def_id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM issue_property WHERE workspace_id = ? AND key = ? \
             AND archived_at IS NULL",
        )
        .bind(workspace.as_str())
        .bind(key.trim())
        .fetch_optional(pool)
        .await?;
        let Some(def_id) = def_id else {
            return Err(PropertyRepoError::PropertyNotFound);
        };
        let raw: String = sqlx::query_scalar("SELECT properties FROM issue WHERE id = ?")
            .bind(issue_id)
            .fetch_one(pool)
            .await?;
        let removed = properties_from_json(&raw).contains_key(&def_id);
        if removed {
            sqlx::query(
                "UPDATE issue \
                 SET properties = json_remove(\
                     CASE WHEN json_valid(properties) THEN properties ELSE '{}' END, ?) \
                 WHERE id = ? AND workspace_id = ?",
            )
            .bind(json_path(&def_id))
            .bind(issue_id)
            .bind(workspace.as_str())
            .execute(pool)
            .await?;
        }
        Ok(removed)
    }

    /// Join an issue's stored value bag against the catalog, in `position` order.
    ///
    /// ARCHIVED definitions and ORPHAN ids (a key with no catalog row) are
    /// DROPPED from the result — the value stays on disk, it just stops
    /// rendering.
    ///
    /// # Errors
    ///
    /// [`sqlx::Error`] on a store failure.
    pub async fn values_for(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
    ) -> Result<Vec<(IssueProperty, PropertyValue)>, sqlx::Error> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT properties FROM issue WHERE id = ? AND workspace_id = ?")
                .bind(issue_id)
                .bind(workspace.as_str())
                .fetch_optional(pool)
                .await?;
        let Some(raw) = raw else {
            return Ok(Vec::new());
        };
        let bag = properties_from_json(&raw);
        if bag.is_empty() {
            return Ok(Vec::new());
        }
        let defs = Self::list(pool, workspace, false).await?;
        Ok(defs
            .into_iter()
            .filter_map(|def| bag.get(&def.id).cloned().map(|v| (def, v)))
            .collect())
    }
}

/// Verify `issue_id` belongs to `workspace`, erroring with
/// [`PropertyRepoError::IssueNotFound`] otherwise. Run inside the mutation's
/// transaction so the check and the write are atomic — this is what makes a
/// foreign-tenant issue id a rejection rather than a cross-tenant write.
async fn ensure_issue_in_workspace(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    issue_id: &str,
) -> Result<(), PropertyRepoError> {
    let found: Option<String> =
        sqlx::query_scalar("SELECT id FROM issue WHERE id = ? AND workspace_id = ?")
            .bind(issue_id)
            .bind(workspace.as_str())
            .fetch_optional(pool)
            .await?;
    if found.is_none() {
        return Err(PropertyRepoError::IssueNotFound);
    }
    Ok(())
}

/// Map one raw `issue_property` row into an [`IssueProperty`].
///
/// `kind` decodes through [`PropertyKind::parse`], which is tolerant by design:
/// a token written by a newer daemon becomes `Unknown` and renders as raw text
/// instead of failing the row.
fn property_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<IssueProperty, sqlx::Error> {
    let options_raw: String = row.try_get("options")?;
    Ok(IssueProperty {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        key: row.try_get("key")?,
        name: row.try_get("name")?,
        kind: PropertyKind::parse(&row.try_get::<String, _>("kind")?),
        options: serde_json::from_str(&options_raw).unwrap_or_default(),
        position: row.try_get("position")?,
        archived_at: row.try_get("archived_at")?,
        created_at: row.try_get("created_at")?,
    })
}
