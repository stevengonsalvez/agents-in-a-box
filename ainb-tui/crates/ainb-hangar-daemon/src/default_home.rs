//! Fresh-home boot seed: make an empty `~/.agents-in-a-box/hangar.db` "just work".
//!
//! A brand-new home has no workspace, no runtime, and no agent, so the Daemon
//! pane shows no runtime and the Squad screen rejects a create with "no agent
//! available to lead a squad". [`ensure_default_home`] lays down, on every boot:
//!
//! 1. the default workspace + owner (via the shared
//!    [`ainb_hangar_store::bootstrap::ensure_default_workspace`]),
//! 2. the host runtime keyed on [`ainb_hangar_store::bootstrap::default_runtime_id`]
//!    — the SAME id the claim loop keys off, so seeded tasks actually get claimed,
//! 3. exactly ONE starter agent (`claude`) bound to that runtime, but ONLY when
//!    the workspace has zero agents.
//!
//! Every step is idempotent and non-clobbering. The starter-agent guard reads the
//! FULL agent count (active + archived), so a user who created, renamed, or
//! archived their own agent has count > 0 and the seed skips — a user's roster is
//! never clobbered, and a restart never re-seeds (the starter agent persists).
//! Collision-safe with the test tripwires that pre-seed slug `default`: the
//! workspace ensure finds the existing row and the agent-count guard skips.

use anyhow::Result;
use sqlx::SqlitePool;

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_store::bootstrap;

/// The starter agent's name — a plain, provider-named agent so the roster reads
/// naturally and the Squad gate clears out of the box.
const STARTER_AGENT_NAME: &str = "claude";

/// Seed the default workspace + runtime + one starter agent on a fresh home.
///
/// Idempotent and non-clobbering (see the module docs). Returns `Ok(())` on a
/// successful seed OR a benign no-op (everything already present).
///
/// # Errors
///
/// Propagates a [`sqlx::Error`] (as `anyhow`) if a lookup or insert fails. The
/// boot path logs and swallows this — a seed failure must never down the daemon.
pub async fn ensure_default_home(pool: &SqlitePool) -> Result<()> {
    let workspace_id = bootstrap::ensure_default_workspace(pool).await?;
    // An already-registered runtime's id wins (a runtime cannot be renamed), so
    // the seed, the claim loop, and every bound agent stay on ONE id.
    let runtime_id = crate::runtime_register::effective_runtime_id(pool).await;
    let now = SystemClock.now_ms();
    // Self-register the host runtime (logs the outcome, swallows a transient
    // failure). The starter-agent create below re-ensures the runtime, so a
    // swallowed error here still surfaces if it would strand the agent's FK.
    crate::runtime_register::self_register(pool, &runtime_id, now).await;

    // Non-clobber guard: seed a starter agent ONLY when the workspace has none.
    // A user who made/renamed/archived their own agent keeps count > 0 → skip;
    // a restart also skips (the seeded agent persists).
    if bootstrap::agent_count(pool, &workspace_id).await? == 0 {
        bootstrap::create_agent(
            pool,
            &workspace_id,
            STARTER_AGENT_NAME,
            bootstrap::DEFAULT_PROVIDER,
            None,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;

    async fn agent_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM agent").fetch_one(pool).await.unwrap()
    }

    #[tokio::test]
    async fn seeds_workspace_runtime_and_one_agent_on_empty_home() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        ensure_default_home(pool).await.unwrap();

        let ws: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(ws, 1, "one default workspace");
        let rt = ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo::get(
            pool,
            &bootstrap::default_runtime_id(),
        )
        .await
        .unwrap()
        .expect("the default runtime is registered");
        assert_eq!(rt.status, "online");
        assert_eq!(agent_count(pool).await, 1, "exactly one starter agent");
    }

    #[tokio::test]
    async fn is_a_noop_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        ensure_default_home(pool).await.unwrap();
        ensure_default_home(pool).await.unwrap();

        let ws: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workspace")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(ws, 1, "restart does not duplicate the workspace");
        assert_eq!(
            agent_count(pool).await,
            1,
            "restart does not re-seed the starter agent"
        );
        let rt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtime")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(rt, 1, "restart upserts the same runtime row");
    }

    #[tokio::test]
    async fn does_not_clobber_a_users_own_agent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // A user lays down the workspace + runtime and their own single agent.
        let ws = bootstrap::ensure_default_workspace(pool).await.unwrap();
        bootstrap::ensure_runtime(pool, &bootstrap::default_runtime_id(), 1)
            .await
            .unwrap();
        let mine = bootstrap::create_agent(pool, &ws, "my-reviewer", "codex", None).await.unwrap();

        // A subsequent boot seed must NOT add a second (starter) agent.
        ensure_default_home(pool).await.unwrap();
        assert_eq!(
            agent_count(pool).await,
            1,
            "the seed skips when a user agent exists"
        );
        let still = ainb_hangar_store::repo::agent::AgentRepo::get(pool, &mine.id)
            .await
            .unwrap()
            .expect("the user's own agent survives");
        assert_eq!(still.name, "my-reviewer");
    }
}
