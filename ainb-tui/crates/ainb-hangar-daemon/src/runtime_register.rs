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
//! `agent_runtime` row keyed on its own runtime id, marking it `online`. It is
//! **idempotent** — a restart updates the same row (`status` + `last_seen_at`)
//! rather than conflicting — so booting twice never duplicates or errors.
//!
//! The row's `workspace_id` FK requires a `workspace` row to exist. A daemon
//! booted against a brand-new home with no workspace yet has nothing to attach
//! to, so registration is a no-op in that case (returns `false`); the first
//! `ainb hangar issue create` lazily bootstraps the workspace, and the next
//! daemon boot registers against it.

use anyhow::{Context, Result};
use sqlx::SqlitePool;

/// Upsert this daemon's `agent_runtime` row, keyed on `runtime_id`.
///
/// A thin wrapper over [`ainb_hangar_store::bootstrap::ensure_runtime`], the one
/// shared upsert every entry point uses (the CLI `agent create`, the boot seed,
/// this self-register). Resolves the default (oldest) workspace and writes — or,
/// on a restart, refreshes — a single `agent_runtime` row with `id = runtime_id`,
/// marking it `online` with a fresh `last_seen_at`. Idempotent via
/// `ON CONFLICT(id)`.
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
/// the resolved [`ainb_hangar_store::bootstrap::default_runtime_id`] — the SAME
/// id the claim loop keys off, keeping the seed, the claim runtime, and every
/// created agent's binding in lockstep.
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
