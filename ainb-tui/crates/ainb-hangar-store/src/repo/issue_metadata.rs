//! Typed repository over the per-issue AGENT METADATA scratch bag
//! (`issue.metadata`, migration 0066, multica parity #17).
//!
//! Flat primitive KV for pipeline bookkeeping (`pr_number`, `pipeline_status`,
//! `waiting_on`…). No catalog, no typed definitions — the deliberate opposite of
//! [`IssuePropertyRepo`](crate::repo::issue_property::IssuePropertyRepo)'s
//! user-facing typed fields.
//!
//! # All mutations are single-key atomic
//!
//! Quoting the reference's own handler header: *"All mutations are single-key
//! atomic. `UpdateIssue` does NOT touch metadata — any whole-blob overwrite
//! would race with concurrent agent writes."* `metadata` therefore joins neither
//! `IssueUpdate` nor any `UPDATE issue SET` outside this module.
//!
//! # Caps, in the reference's order
//!
//! key regex → primitive-only value → [`MAX_METADATA_KEYS`] (only when the key
//! is NEW, so an overwrite at the cap succeeds) → [`MAX_METADATA_BYTES`].

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::properties::{
    MAX_METADATA_BYTES, MAX_METADATA_KEYS, MetadataValue, PropertyError, metadata_from_json,
    metadata_to_json, validate_metadata_key,
};
use sqlx::SqlitePool;
use std::collections::BTreeMap;

use crate::repo::issue_property::PropertyRepoError;

/// Stateless typed wrapper over the `issue.metadata` column.
pub struct IssueMetadataRepo;

impl IssueMetadataRepo {
    /// Read an issue's whole metadata bag, key-sorted.
    ///
    /// A foreign-tenant (or unknown) issue id yields
    /// [`PropertyRepoError::IssueNotFound`], never another tenant's bag.
    ///
    /// # Errors
    ///
    /// [`PropertyRepoError::IssueNotFound`] or [`PropertyRepoError::Db`].
    pub async fn get(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
    ) -> Result<BTreeMap<String, MetadataValue>, PropertyRepoError> {
        let raw: Option<String> =
            sqlx::query_scalar("SELECT metadata FROM issue WHERE id = ? AND workspace_id = ?")
                .bind(issue_id)
                .bind(workspace.as_str())
                .fetch_optional(pool)
                .await?;
        raw.map(|r| metadata_from_json(&r))
            .ok_or(PropertyRepoError::IssueNotFound)
    }

    /// Set ONE metadata key atomically.
    ///
    /// # Errors
    ///
    /// [`PropertyRepoError::IssueNotFound`] (tenant guard), or
    /// [`PropertyRepoError::Value`] carrying [`PropertyError::BadKey`],
    /// [`PropertyError::TooManyKeys`] or [`PropertyError::TooLarge`].
    pub async fn set(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        key: &str,
        value: &MetadataValue,
    ) -> Result<(), PropertyRepoError> {
        validate_metadata_key(key)?;

        let mut tx = pool.begin().await?;
        let raw: Option<String> =
            sqlx::query_scalar("SELECT metadata FROM issue WHERE id = ? AND workspace_id = ?")
                .bind(issue_id)
                .bind(workspace.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let Some(raw) = raw else {
            return Err(PropertyRepoError::IssueNotFound);
        };
        let mut bag = metadata_from_json(&raw);
        // The cap bites only on a NEW key: overwriting an existing key while at
        // the cap is fine, matching the reference.
        if !bag.contains_key(key) && bag.len() >= MAX_METADATA_KEYS {
            return Err(PropertyRepoError::Value(PropertyError::TooManyKeys));
        }
        bag.insert(key.to_string(), value.clone());
        let encoded = metadata_to_json(&bag);
        if encoded.len() > MAX_METADATA_BYTES {
            return Err(PropertyRepoError::Value(PropertyError::TooLarge));
        }
        sqlx::query("UPDATE issue SET metadata = ? WHERE id = ?")
            .bind(&encoded)
            .bind(issue_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Delete ONE metadata key atomically. Returns `false` when it was absent.
    ///
    /// # Errors
    ///
    /// [`PropertyRepoError::IssueNotFound`] or [`PropertyRepoError::Db`].
    pub async fn delete(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        key: &str,
    ) -> Result<bool, PropertyRepoError> {
        let mut tx = pool.begin().await?;
        let raw: Option<String> =
            sqlx::query_scalar("SELECT metadata FROM issue WHERE id = ? AND workspace_id = ?")
                .bind(issue_id)
                .bind(workspace.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        let Some(raw) = raw else {
            return Err(PropertyRepoError::IssueNotFound);
        };
        let mut bag = metadata_from_json(&raw);
        let removed = bag.remove(key).is_some();
        if removed {
            sqlx::query("UPDATE issue SET metadata = ? WHERE id = ?")
                .bind(metadata_to_json(&bag))
                .bind(issue_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(removed)
    }
}
