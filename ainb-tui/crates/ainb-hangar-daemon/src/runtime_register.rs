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

/// `daemon_id` recorded for a self-registered runtime.
///
/// The reference schema keys uniqueness on `(workspace_id, daemon_id, provider)`;
/// a single host daemon advertises one provider, so a stable literal keeps the
/// upsert deterministic (a restart targets the same tuple). The runtime's own
/// id (the PK) is what callers route to; `daemon_id` is descriptive metadata.
const SELF_DAEMON_ID: &str = "ainb-hangar-daemon";

/// Provider a self-registered runtime advertises.
///
/// The daemon's claim loop resolves the actual provider per-task from the
/// agent's backend; the runtime row's `provider` column is the advertised
/// default. `claude` mirrors the seed fixture + the v1 default backend.
const SELF_PROVIDER: &str = "claude";

/// Runtime mode for a self-registered runtime (always a local daemon today).
const SELF_RUNTIME_MODE: &str = "local";

/// Upsert this daemon's `agent_runtime` row, keyed on `runtime_id`.
///
/// Resolves the default (oldest) workspace and writes — or, on a restart,
/// refreshes — a single `agent_runtime` row with `id = runtime_id`, marking it
/// `online` with a fresh `last_seen_at`. Idempotent: the `ON CONFLICT(id)`
/// clause updates the existing row instead of erroring, so booting repeatedly is
/// safe.
///
/// Returns `Ok(true)` when a row was written/refreshed, `Ok(false)` when there
/// is no workspace to attach to yet (a brand-new home before the first
/// `issue create` bootstrap) — a benign no-op the daemon logs and continues past.
///
/// # Errors
///
/// Propagates a [`sqlx::Error`] (wrapped) if the workspace lookup or the upsert
/// itself fails for a reason other than the absent-workspace no-op.
pub async fn register_runtime(pool: &SqlitePool, runtime_id: &str, now_ms: i64) -> Result<bool> {
    let workspace_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM workspace ORDER BY created_at LIMIT 1")
            .fetch_optional(pool)
            .await
            .context("resolve default workspace for runtime self-register")?;

    let Some(workspace_id) = workspace_id else {
        // No workspace yet: nothing to attach a runtime to. The first
        // `issue create` lays one down; the next boot registers against it.
        return Ok(false);
    };

    // Idempotent upsert. `ON CONFLICT(id)` refreshes a row a previous boot wrote
    // (a restart) rather than tripping the PK / the unique
    // (workspace_id, daemon_id, provider) index.
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, last_seen_at, status) \
         VALUES (?, ?, ?, ?, ?, ?, 'online') \
         ON CONFLICT(id) DO UPDATE SET \
           status = 'online', \
           last_seen_at = excluded.last_seen_at",
    )
    .bind(runtime_id)
    .bind(&workspace_id)
    .bind(SELF_DAEMON_ID)
    .bind(SELF_PROVIDER)
    .bind(SELF_RUNTIME_MODE)
    .bind(now_ms)
    .execute(pool)
    .await
    .context("upsert self-registered agent_runtime row")?;

    Ok(true)
}

/// Boot-time entry point: self-register this daemon's runtime from the
/// environment, logging the outcome.
///
/// Reads `HANGAR_DAEMON_RUNTIME_ID` (the same identity the claim loop keys off);
/// an unset/empty value means an anonymous daemon with nothing to register, so
/// this is a no-op. Otherwise it calls [`register_runtime`] with a fresh
/// timestamp and logs the result. A failure is logged and swallowed — runtime
/// self-registration must never down the daemon (it still sweeps + serves).
pub async fn self_register_from_env(pool: &SqlitePool) {
    let Some(runtime_id) = std::env::var("HANGAR_DAEMON_RUNTIME_ID").ok().filter(|s| !s.is_empty())
    else {
        return;
    };
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&ainb_hangar_core::clock::SystemClock);
    match register_runtime(pool, &runtime_id, now).await {
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
        assert_eq!(row.provider, SELF_PROVIDER);
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
