//! Typed repository wrapper over the `profile` INDEX table (P5, D14-D16).
//!
//! The `profile` table is only an INDEX over the on-disk masters
//! (`~/.agents-in-a-box/profiles/<slug>.md`, T6) — never the storage. This repo is the
//! sole SQL surface the daemon's fs-watch reconciler drives to keep that index
//! current: upsert one row per master (`slug`, `tier`, `mtime`), list the
//! indexed profiles for the `profile/list` RPC, resolve one by slug, and prune a
//! row whose master file has disappeared.
//!
//! # Host scope, not workspace scope
//!
//! Unlike [`squad`](super::squad) / [`issue`](super::issue), profiles are NOT
//! workspace-scoped: a single master drives runs in any workspace (D16 — the
//! board assignee slug *is* the profile slug regardless of tenant). No method
//! takes a `WorkspaceId`; the slug PRIMARY KEY is the sole identity.
//!
//! # Validity is the caller's contract
//!
//! The `tier` CHECK (`premium` / `balanced` / `fast`) is the engine-enforced
//! guard; slug shape is validated by the core parser before an upsert
//! ([`ainb_hangar_core::profile::is_valid_slug`]). This repo binds already-valid
//! values — it does not re-validate.

use sqlx::{Row, SqlitePool};

/// One indexed profile row (the `profile/list` / `profile/get` read model).
///
/// A thin projection of the on-disk master's identity fields — the body always
/// lives on disk, never here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIndexRow {
    /// The profile slug (PK) — file stem + board-assignee slug (D16).
    pub slug: String,
    /// The logical model tier token (`premium` / `balanced` / `fast`).
    pub tier: String,
    /// The master file's last-modified time (epoch milliseconds).
    pub mtime: i64,
}

/// Stateless typed wrapper over the `profile` index table.
pub struct ProfileRepo;

impl ProfileRepo {
    /// Upsert the index row for `slug`, replacing `tier` + `mtime` if the row
    /// already exists (the fs-watch reconciler's per-master write).
    ///
    /// Idempotent on `slug`: re-indexing an unchanged master is a no-op write of
    /// the same values; re-indexing an edited one refreshes the tier + mtime.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a store failure (including the `tier` CHECK
    /// firing for a value outside the enum — a caller bug, since the core parser
    /// only ever produces valid tiers).
    pub async fn upsert(
        pool: &SqlitePool,
        slug: &str,
        tier: &str,
        mtime: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO profile (slug, tier, mtime) VALUES (?, ?, ?) \
             ON CONFLICT(slug) DO UPDATE SET tier = excluded.tier, mtime = excluded.mtime",
        )
        .bind(slug)
        .bind(tier)
        .bind(mtime)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// List every indexed profile, slug-ordered (a stable render order for the
    /// `profile/list` RPC and the editor screen's roster).
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a store failure.
    pub async fn list(pool: &SqlitePool) -> Result<Vec<ProfileIndexRow>, sqlx::Error> {
        let rows = sqlx::query("SELECT slug, tier, mtime FROM profile ORDER BY slug")
            .fetch_all(pool)
            .await?;
        Ok(rows.into_iter().map(row_to_index).collect())
    }

    /// Resolve one profile's index row by slug, or `None` when the slug is not
    /// indexed.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a store failure.
    pub async fn get(pool: &SqlitePool, slug: &str) -> Result<Option<ProfileIndexRow>, sqlx::Error> {
        let row = sqlx::query("SELECT slug, tier, mtime FROM profile WHERE slug = ?")
            .bind(slug)
            .fetch_optional(pool)
            .await?;
        Ok(row.map(row_to_index))
    }

    /// Delete the index row for `slug` (the reconciler's prune of a master whose
    /// file has disappeared). Deleting an absent slug is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a store failure.
    pub async fn delete(pool: &SqlitePool, slug: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM profile WHERE slug = ?")
            .bind(slug)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Every indexed slug, slug-ordered — the set the reconciler diffs against the
    /// on-disk masters to find rows to prune.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error`] on a store failure.
    pub async fn list_slugs(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query("SELECT slug FROM profile ORDER BY slug")
            .fetch_all(pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| r.get::<String, _>("slug"))
            .collect())
    }
}

/// Project a raw row into a [`ProfileIndexRow`].
fn row_to_index(row: sqlx::sqlite::SqliteRow) -> ProfileIndexRow {
    ProfileIndexRow {
        slug: row.get::<String, _>("slug"),
        tier: row.get::<String, _>("tier"),
        mtime: row.get::<i64, _>("mtime"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[tokio::test]
    async fn upsert_then_get_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        ProfileRepo::upsert(pool, "reviewer", "premium", 1000).await.unwrap();
        ProfileRepo::upsert(pool, "author", "balanced", 2000).await.unwrap();

        let got = ProfileRepo::get(pool, "reviewer").await.unwrap().unwrap();
        assert_eq!(
            got,
            ProfileIndexRow {
                slug: "reviewer".into(),
                tier: "premium".into(),
                mtime: 1000
            }
        );

        // Slug-ordered.
        let all = ProfileRepo::list(pool).await.unwrap();
        let slugs: Vec<_> = all.iter().map(|r| r.slug.as_str()).collect();
        assert_eq!(slugs, ["author", "reviewer"]);
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_refreshes_on_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        ProfileRepo::upsert(pool, "reviewer", "premium", 1000).await.unwrap();
        // Re-index an edited master: tier + mtime refresh, still one row.
        ProfileRepo::upsert(pool, "reviewer", "fast", 5000).await.unwrap();

        let all = ProfileRepo::list(pool).await.unwrap();
        assert_eq!(all.len(), 1, "conflict updates in place, never duplicates");
        assert_eq!(all[0].tier, "fast");
        assert_eq!(all[0].mtime, 5000);
    }

    #[tokio::test]
    async fn get_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        assert!(ProfileRepo::get(store.pool(), "nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_prunes_and_is_noop_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        ProfileRepo::upsert(pool, "reviewer", "premium", 1000).await.unwrap();
        ProfileRepo::delete(pool, "reviewer").await.unwrap();
        assert!(ProfileRepo::get(pool, "reviewer").await.unwrap().is_none());
        // Absent delete: no error.
        ProfileRepo::delete(pool, "reviewer").await.unwrap();
    }

    #[tokio::test]
    async fn list_slugs_is_the_prune_diff_set() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        ProfileRepo::upsert(pool, "b", "fast", 1).await.unwrap();
        ProfileRepo::upsert(pool, "a", "fast", 1).await.unwrap();
        assert_eq!(ProfileRepo::list_slugs(pool).await.unwrap(), ["a", "b"]);
    }

    #[tokio::test]
    async fn invalid_tier_is_rejected_by_the_check() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let err = ProfileRepo::upsert(store.pool(), "x", "turbo", 1).await;
        assert!(err.is_err(), "tier CHECK must reject an unknown band");
    }
}
