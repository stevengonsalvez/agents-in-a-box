//! Hangar persistence layer.
//!
//! Owns the `SQLite` schema (via embedded [`sqlx`] migrations) and the
//! repository wrappers that the daemon and services build on. This crate is the
//! single source of truth for the on-disk shape of `~/.ainb/hangar.db`.
//!
//! # Migrations
//!
//! Migrations live in `migrations/` and follow the frozen naming convention
//! `NNNN_<terse_slug>.sql` (4-digit zero-padded ordinal, `snake_case` slug, no
//! `_up`/`_down` suffix — `SQLite` migrations are forward-only here). They are
//! embedded at compile time by [`sqlx::migrate!`], so a migration edit that is
//! not picked up usually means a stale build cache — run
//! `cargo clean -p ainb-hangar-store` and rebuild.

use sqlx::SqlitePool;

/// Apply every embedded migration to `pool`, bringing a fresh or partially
/// migrated database up to the current schema version.
///
/// Idempotent: `sqlx` records applied migrations in `_sqlx_migrations` and
/// skips any already present, so calling this on every daemon boot is safe.
///
/// # Errors
///
/// Returns an error if a migration fails to apply (for example a checksum
/// mismatch on a previously applied migration, or malformed SQL).
pub async fn apply_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
