//! Self-registration of the daemon's runtime on boot (e38.20).
//!
//! A freshly-booted daemon claims tasks for the runtime named by
//! `HANGAR_DAEMON_RUNTIME_ID` (see [`crate::run_loop::DaemonConfig`]), but before
//! this module nothing ever wrote an `agent_runtime` row outside of test
//! fixtures — so the row that the claim loop, the agent picker, and the
//! daemon-health pane all key off simply did not exist for a real daemon. The
//! TUI showed no runtime, and a CLI-bootstrapped workspace had an agent with no
//! place to run.
//!
//! [`register_runtime`] closes that gap: at boot the daemon upserts an
//! `agent_runtime` row for its `(workspace, daemon_id, provider)` tuple, marking
//! it `online`. It is **idempotent** — a restart refreshes the same row
//! (`status` + `last_seen_at`) rather than conflicting — so booting twice never
//! duplicates or errors.
//!
//! # A runtime cannot be renamed
//!
//! `agent.runtime_id` is a NOT NULL `REFERENCES agent_runtime(id)` FK and SQLite
//! enforces foreign keys (sqlx sets `PRAGMA foreign_keys = ON`), so a registered
//! runtime's `id` can never change once an agent binds it. [`effective_runtime_id`]
//! therefore resolves the daemon's claim identity from the DB up front: an
//! already-registered runtime's id WINS over whatever `HANGAR_DAEMON_RUNTIME_ID` /
//! the default says (with a warning), so the registered row, the agents bound to
//! it, and the claim loop always agree. Only a brand-new home adopts the
//! configured id.
//!
//! The row's `workspace_id` FK requires a `workspace` row to exist. A daemon
//! booted against a brand-new home with no workspace yet has nothing to attach
//! to, so registration is a no-op in that case (returns `false`); the boot seed
//! lays the workspace down first, so in practice this only guards odd orderings.

use anyhow::{Context, Result};
use sqlx::SqlitePool;

/// Resolve the runtime id this daemon registers under AND claims for.
///
/// An already-registered runtime's id WINS: a runtime cannot be renamed once an
/// agent's `runtime_id` FK references it (see the module docs), so a changed
/// `HANGAR_DAEMON_RUNTIME_ID` is ignored — with a `warn!` naming both ids — rather
/// than silently registering nothing and stranding every task. Only a brand-new
/// home (no runtime row yet) adopts the configured/default id.
///
/// Read-only + infallible from the caller's view: a lookup fault falls back to the
/// configured id (logged), because refusing to boot over a transient read is worse
/// than registering the configured identity.
pub async fn effective_runtime_id(pool: &SqlitePool) -> String {
    let configured = ainb_hangar_store::bootstrap::default_runtime_id();
    match ainb_hangar_store::bootstrap::existing_host_runtime_id(pool).await {
        Ok(Some(existing)) => {
            if existing != configured {
                tracing::warn!(
                    configured = %configured,
                    existing = %existing,
                    "HANGAR_DAEMON_RUNTIME_ID={configured} ignored; existing runtime {existing} \
                     is in use; a runtime cannot be renamed after first boot"
                );
            }
            existing
        }
        Ok(None) => configured,
        Err(e) => {
            tracing::warn!(error = %e, "runtime id resolve failed; using the configured id");
            configured
        }
    }
}

/// Upsert this daemon's `agent_runtime` row for its
/// `(workspace, daemon_id, provider)` tuple.
///
/// A thin wrapper over [`ainb_hangar_store::bootstrap::ensure_runtime`], the one
/// shared upsert every entry point uses (the CLI `agent create`, the boot seed,
/// this self-register). Resolves the default (oldest) workspace and writes — or,
/// on a restart, refreshes (`status` + `last_seen_at`) — the host runtime row.
/// Idempotent, and it never changes an existing row's `id` (that would break the
/// `agent.runtime_id` FK); pass [`effective_runtime_id`] so the id you register is
/// the id already in use.
///
/// Returns `Ok(true)` when a row was written/refreshed, `Ok(false)` when there
/// is no workspace to attach to yet (a brand-new home before the boot seed) — a
/// benign no-op the daemon logs and continues past.
///
/// # Errors
///
/// Propagates a [`sqlx::Error`] (wrapped) if the workspace lookup or the upsert
/// itself fails for a reason other than the absent-workspace no-op.
pub async fn register_runtime(pool: &SqlitePool, runtime_id: &str, now_ms: i64) -> Result<bool> {
    ainb_hangar_store::bootstrap::ensure_runtime(pool, runtime_id, now_ms)
        .await
        .context("upsert self-registered agent_runtime row")
}

/// Self-register the host runtime under the given id, logging the outcome. A
/// failure is logged and swallowed — self-registration must never down the
/// daemon (it still sweeps + serves).
///
/// Called by the boot seed ([`crate::default_home::ensure_default_home`]) with
/// [`effective_runtime_id`] — the SAME id the claim loop keys off, keeping the
/// seed, the claim runtime, and every created agent's binding in lockstep.
pub async fn self_register(pool: &SqlitePool, runtime_id: &str, now_ms: i64) {
    match register_runtime(pool, runtime_id, now_ms).await {
        Ok(true) => tracing::info!(runtime_id = %runtime_id, "self-registered agent runtime"),
        Ok(false) => {
            tracing::info!(runtime_id = %runtime_id, "runtime self-register skipped (no workspace yet)");
        }
        Err(e) => {
            tracing::warn!(error = %e, runtime_id = %runtime_id, "runtime self-register failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;

    /// Bootstrap a default workspace (mirrors the CLI's lazy bootstrap) so the
    /// runtime row's FK is satisfiable.
    async fn seed_workspace(pool: &SqlitePool) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-1")
            .bind("default")
            .bind("Default")
            .bind(1_i64)
            .execute(pool)
            .await
            .expect("seed workspace");
    }

    #[tokio::test]
    async fn register_runtime_inserts_an_online_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_workspace(pool).await;

        let wrote = register_runtime(pool, "rt-self", 1_000).await.unwrap();
        assert!(wrote, "a workspace exists, so the runtime must register");

        let row = ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo::get(pool, "rt-self")
            .await
            .unwrap()
            .expect("the self-registered runtime row exists");
        assert_eq!(row.id, "rt-self");
        assert_eq!(row.workspace_id, "ws-1");
        assert_eq!(row.provider, "claude", "the host runtime advertises claude");
        assert_eq!(row.status, "online");
        assert_eq!(row.last_seen_at, Some(1_000));
    }

    #[tokio::test]
    async fn register_runtime_is_idempotent_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_workspace(pool).await;

        // First boot.
        assert!(register_runtime(pool, "rt-self", 1_000).await.unwrap());
        // Second boot (restart) with a later heartbeat: must NOT error on the PK
        // / unique-index conflict, and must refresh the existing row.
        assert!(register_runtime(pool, "rt-self", 2_000).await.unwrap());

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtime")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "a restart upserts, never duplicates");

        let row = ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo::get(pool, "rt-self")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.last_seen_at,
            Some(2_000),
            "the restart refreshed last_seen_at"
        );
        assert_eq!(row.status, "online");
    }

    #[tokio::test]
    async fn register_runtime_is_a_noop_with_no_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // No workspace seeded: registration is a benign no-op, never an error
        // (the FK would otherwise fail).
        let wrote = register_runtime(pool, "rt-self", 1_000).await.unwrap();
        assert!(!wrote, "no workspace ⇒ nothing to register");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtime")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "no row written without a workspace");
    }
}
