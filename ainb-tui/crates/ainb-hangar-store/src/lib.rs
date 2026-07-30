//! Hangar persistence layer.
//!
//! Owns the `SQLite` schema (via embedded [`sqlx`] migrations) and the
//! repository wrappers that the daemon and services build on. This crate is the
//! single source of truth for the on-disk shape of `~/.agents-in-a-box/hangar.db`.
//!
//! # Migrations
//!
//! Migrations live in `migrations/` and follow the frozen naming convention
//! `NNNN_<terse_slug>.sql` (4-digit zero-padded ordinal, `snake_case` slug, no
//! `_up`/`_down` suffix — `SQLite` migrations are forward-only here). They are
//! embedded at compile time by [`sqlx::migrate!`], which on stable Rust
//! cannot register `migrations/` as a build dependency on its own; `build.rs`
//! emits `cargo:rerun-if-changed=migrations` so cargo rebuilds this crate
//! whenever a `.sql` file is added, edited, or removed. If a migration edit
//! is somehow still not picked up, that is a `build.rs` regression, not
//! expected behavior (`cargo clean -p ainb-hangar-store` remains the escape
//! hatch, but should not be needed).
//!
//! ## An applied migration file is IMMUTABLE — including its comments
//!
//! `sqlx` records a checksum of each migration's FULL FILE TEXT in
//! `_sqlx_migrations`. Editing an already-applied file — even a comment — changes
//! that checksum, and the next boot against an existing database aborts with
//! `migration N was previously applied but has been modified`, i.e. the daemon
//! refuses to start on every home that ever booted. Corrections to an applied
//! migration's prose therefore CANNOT be made in-file; document the correction
//! here (or in the owning repo module) instead. Only brand-new migrations are
//! editable, and only until they ship.
//!
//! ## Foreign keys ARE enforced
//!
//! Several applied migration comments (0009/0010/0016/0017/0018/0027/0036) claim
//! "`PRAGMA foreign_keys` is off in this crate". That is **false and frozen**:
//! `sqlx` turns `PRAGMA foreign_keys = ON` on by default for every `SQLite`
//! connection and [`Store::open_in`] never disables it, so every declared
//! `REFERENCES` IS engine-enforced (e.g. `agent.runtime_id` cannot be orphaned,
//! and `0010`'s `autopilot_run_id` link is a real constraint, not documentation).
//! Those comments cannot be corrected in place (see above) — this is the
//! authoritative statement. Where a table declares NO foreign key, that is a
//! deliberate schema choice (a polymorphic actor-ref, or a link an FK could not
//! tenant-scope), NOT a consequence of FKs being off; the service-layer guards
//! those repos apply are still required, because an FK proves only that a parent
//! row exists, never which workspace owns it.

use sqlx::SqlitePool;

mod store;
pub use store::Store;

/// Fresh-home bootstrap: the idempotent default workspace + owner + runtime +
/// starter-agent lay-down every entry point (CLI, daemon boot, `agent_create`)
/// shares, plus the stable [`bootstrap::default_runtime_id`] the seed and the
/// daemon's claim loop both key off.
pub mod bootstrap;

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
/// success-or-mismatch algorithm the four P1.3 services share. Mirrors the
/// reference control plane `task.go:1010`.
pub use service::finalize::{
    FinalizeError, FinalizeOutcome, finalize_idempotent as idempotent_finalize,
};

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

/// Probe whether the LIVE database has drifted away from the schema this
/// binary was compiled against, returning a human-readable description of the
/// drift (or the probe failure) — `None` means healthy.
///
/// The trap this catches: a stale daemon binary keeps serving a database that
/// a NEWER binary has since migrated forward. `sqlx` only validates migrations
/// at pool-open, so the running daemon never notices — its queries half-work
/// against the newer schema and every surface renders empty zeros instead of
/// an error. The daemon-health snapshot calls this on demand so the TUI can
/// scream "restart the daemon" instead of rendering a silently-blank pane.
///
/// Two signals, both folded into the returned string:
/// - the applied `MAX(version)` in `_sqlx_migrations` is AHEAD of the newest
///   migration embedded in this binary → stale binary;
/// - the probe query itself fails (file deleted, corrupt, locked) → the
///   database is unreachable outright.
pub async fn schema_drift(pool: &SqlitePool) -> Option<String> {
    let embedded_max = sqlx::migrate!("./migrations").iter().map(|m| m.version).max().unwrap_or(0);
    let applied_max: Result<Option<i64>, sqlx::Error> =
        sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await;
    match applied_max {
        Ok(Some(applied)) if applied > embedded_max => Some(format!(
            "database schema (migration {applied}) is AHEAD of this daemon binary \
             (migration {embedded_max}) — stale binary; restart the daemon from the \
             current build"
        )),
        Ok(_) => None,
        Err(e) => Some(format!("database unreachable: {e}")),
    }
}
