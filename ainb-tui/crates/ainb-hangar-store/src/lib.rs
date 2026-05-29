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

mod store;
pub use store::Store;

/// Typed repository wrappers over the Hangar schema.
///
/// One sub-module per logical table group: [`repo::agent`],
/// [`repo::agent_runtime`], [`repo::issue`], [`repo::skill`], and
/// [`repo::task`].
pub mod repo;

/// Task-FSM services: clock-aware lifecycle transitions (claim, start,
/// complete, fail, cancel) over the [`service::finalize`] idempotent primitive.
pub mod service;

/// The shared idempotent-finalize primitive, re-exported at the crate root.
///
/// Promoted out of [`service::finalize`] so the P1.4 retry sweeper (and any
/// future finalizer) can reuse the exact same 0-row-UPDATE → re-read →
/// success-or-mismatch algorithm the four P1.3 services share. Mirrors Multica
/// `task.go:1010`.
pub use service::finalize::{finalize_idempotent as idempotent_finalize, FinalizeError, FinalizeOutcome};

/// Test-only helpers (isolated `$HOME`, `ENV_LOCK`) for driving [`Store`].
///
/// Available in this crate's own tests and to any downstream crate that enables
/// the `test-support` feature (the daemon's tripwire does this for its boot
/// tests).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

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
