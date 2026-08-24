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
//!
//! # Restart vs reconnect (migration 0092)
//!
//! Because every boot refreshes the SAME row, the registration alone could not
//! say whether the daemon that owned it is still alive. [`daemon_instance_id`]
//! mints one id per daemon PROCESS and [`resolve_runtime_boot`] presents it, so
//! the store can compare it with the stored owner and answer
//! [`RuntimeArrival`]: a different owner means this process took the runtime over
//! and the previous one's `dispatched`/`running` tasks are orphans; an identical
//! owner means the same live process re-registered and its work must be left
//! alone. The boot path acts on that in
//! [`crate::sweeper::reclaim_orphans_on_restart`].
//!
//! Exactly ONE call site claims the instance — [`resolve_runtime_boot`], from
//! [`crate::boot`]. [`effective_runtime_id`] (used by the boot seed) deliberately
//! does NOT: it runs earlier in the same process, and if it claimed first, the
//! real boot registration would see its own id and report a reconnect, skipping
//! the orphan reconcile the restart needs.

use std::sync::OnceLock;

use ainb_hangar_core::ids::InstanceId;
use ainb_hangar_store::bootstrap::{RuntimeArrival, RuntimeRegistration};
use anyhow::{Context, Result};
use sqlx::SqlitePool;

/// This daemon process's instance id, minted on first use.
///
/// Stable for the life of the process — that stability IS the signal (see the
/// module docs), so it is deliberately never re-minted per call or per
/// registration.
///
/// A ULID from the same [`ainb_hangar_core::idgen`] seam every other Hangar id
/// comes from, so no new dependency and no second id format.
pub fn daemon_instance_id() -> &'static InstanceId {
    static INSTANCE: OnceLock<InstanceId> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        use ainb_hangar_core::idgen::IdGen;
        InstanceId::from_str(ainb_hangar_core::idgen::SystemIdGen.new_ulid())
            .expect("a freshly minted ULID is never empty")
    })
}

/// What [`resolve_runtime_boot`] resolved: the id this daemon claims for, and
/// whether taking it over orphaned a previous instance's in-flight work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBoot {
    /// The runtime id actually in use (an existing row's id wins).
    pub runtime_id: String,
    /// Restart or reconnect, as decided against the stored instance id.
    pub arrival: RuntimeArrival,
}

/// [`effective_runtime_id`], plus the restart-vs-reconnect verdict: register
/// under this PROCESS's [`daemon_instance_id`] and report what that displaced.
///
/// This is the daemon's boot registration and the only place that claims
/// instance ownership (see the module docs). Infallible from the caller's view
/// for the same reason [`effective_runtime_id`] is — a fault falls back to the
/// configured id, and to `Restart` with no named predecessor, because "assume
/// the previous executor is gone" is the recoverable reading of an unknown
/// owner: requeuing a task whose process died is cheap, stranding one until the
/// 2.5h running TTL is not.
pub async fn resolve_runtime_boot(pool: &SqlitePool, now_ms: i64) -> RuntimeBoot {
    let configured = ainb_hangar_store::bootstrap::default_runtime_id();
    let instance = daemon_instance_id();
    let registered = ainb_hangar_store::bootstrap::register_runtime_instance(
        pool,
        &configured,
        instance,
        now_ms,
    )
    .await;
    match registered {
        Ok(Some(RuntimeRegistration {
            runtime_id,
            arrival,
        })) => {
            log_settled_id(&configured, &runtime_id);
            match &arrival {
                RuntimeArrival::Restart {
                    previous_instance_id,
                } => tracing::info!(
                    runtime_id = %runtime_id,
                    instance_id = %instance,
                    previous_instance_id = previous_instance_id
                        .as_ref()
                        .map_or("<none>", InstanceId::as_str),
                    "runtime registered as a RESTART; the previous instance's in-flight tasks \
                     are orphaned"
                ),
                RuntimeArrival::Reconnect => tracing::info!(
                    runtime_id = %runtime_id,
                    instance_id = %instance,
                    "runtime registered as a RECONNECT; the same instance still owns its work"
                ),
            }
            RuntimeBoot {
                runtime_id,
                arrival,
            }
        }
        // No workspace to attach to yet: nothing registered, carry the configured
        // id and the safe default verdict.
        Ok(None) => {
            tracing::info!(runtime_id = %configured, "runtime self-register skipped (no workspace yet)");
            unknown_owner_boot(configured)
        }
        Err(e) => {
            tracing::warn!(error = %e, "runtime self-register/resolve failed; using the configured id");
            unknown_owner_boot(configured)
        }
    }
}

/// The fallback boot verdict when the registration could not run: the configured
/// id, and `Restart` with no predecessor (unknown owner ⇒ assume the previous
/// executor is gone).
const fn unknown_owner_boot(runtime_id: String) -> RuntimeBoot {
    RuntimeBoot {
        runtime_id,
        arrival: RuntimeArrival::Restart {
            previous_instance_id: None,
        },
    }
}

/// Log which id a registration settled on: a `warn!` naming both when an
/// existing runtime's id overrode the configured one (a runtime cannot be
/// renamed), an `info!` otherwise.
fn log_settled_id(configured: &str, settled: &str) {
    if settled == configured {
        tracing::info!(runtime_id = %settled, "self-registered agent runtime");
    } else {
        tracing::warn!(
            configured = %configured,
            existing = %settled,
            "HANGAR_DAEMON_RUNTIME_ID={configured} ignored; existing runtime {settled} \
             is in use; a runtime cannot be renamed after first boot"
        );
    }
}

/// Resolve the runtime id this daemon registers under AND claims for.
///
/// An already-registered runtime's id WINS: a runtime cannot be renamed once an
/// agent's `runtime_id` FK references it (see the module docs), so a changed
/// `HANGAR_DAEMON_RUNTIME_ID` is ignored — with a `warn!` naming both ids — rather
/// than silently registering nothing and stranding every task. Only a brand-new
/// home (no runtime row yet) adopts the configured/default id.
///
/// Registers AND resolves in ONE atomic upsert
/// ([`ainb_hangar_store::bootstrap::ensure_runtime`], which returns the id it
/// settled on), so there is no read-then-write window in which the resolved id
/// could stop being the registered one.
///
/// Infallible from the caller's view: a fault falls back to the configured id
/// (logged), because refusing to boot over a transient error is worse than
/// carrying on with the configured identity.
///
/// Does NOT claim instance ownership — this is the boot SEED's resolve, and the
/// process that executes the runtime's tasks claims through
/// [`resolve_runtime_boot`]. See the module docs for why claiming from both would
/// mask the restart.
pub async fn effective_runtime_id(pool: &SqlitePool, now_ms: i64) -> String {
    let configured = ainb_hangar_store::bootstrap::default_runtime_id();
    match ainb_hangar_store::bootstrap::ensure_runtime(pool, &configured, now_ms).await {
        Ok(Some(settled)) => {
            log_settled_id(&configured, &settled);
            settled
        }
        // No workspace to attach to yet: nothing registered, carry the configured id.
        Ok(None) => {
            tracing::info!(runtime_id = %configured, "runtime self-register skipped (no workspace yet)");
            configured
        }
        Err(e) => {
            tracing::warn!(error = %e, "runtime self-register/resolve failed; using the configured id");
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
/// Returns `Ok(Some(id))` — the id the row actually settled on (the pre-existing
/// one when a runtime is already registered) — or `Ok(None)` when there is no
/// workspace to attach to yet (a brand-new home before the boot seed).
///
/// # Errors
///
/// Propagates a [`sqlx::Error`] (wrapped) if the workspace lookup or the upsert
/// itself fails for a reason other than the absent-workspace no-op.
pub async fn register_runtime(
    pool: &SqlitePool,
    runtime_id: &str,
    now_ms: i64,
) -> Result<Option<String>> {
    ainb_hangar_store::bootstrap::ensure_runtime(pool, runtime_id, now_ms)
        .await
        .context("upsert self-registered agent_runtime row")
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

        let settled = register_runtime(pool, "rt-self", 1_000).await.unwrap();
        assert_eq!(
            settled.as_deref(),
            Some("rt-self"),
            "a workspace exists, so the runtime must register under the given id"
        );

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
        assert_eq!(
            register_runtime(pool, "rt-self", 1_000).await.unwrap().as_deref(),
            Some("rt-self")
        );
        // Second boot (restart) with a later heartbeat: must NOT error on the PK
        // / unique-index conflict, and must refresh the existing row.
        assert_eq!(
            register_runtime(pool, "rt-self", 2_000).await.unwrap().as_deref(),
            Some("rt-self")
        );

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

    /// The recorded owner of `runtime_id`, or `None` when unclaimed.
    async fn owner(pool: &SqlitePool, runtime_id: &str) -> Option<InstanceId> {
        ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo::instance_id(pool, runtime_id)
            .await
            .expect("read instance_id")
    }

    /// Booting against a runtime a DIFFERENT instance owns is a restart: the
    /// verdict names the displaced instance, and this process takes the row over.
    #[tokio::test]
    async fn boot_after_another_instance_reports_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_workspace(pool).await;

        // A previous daemon process owned the runtime and then died.
        let previous = InstanceId::from_str("inst-dead").unwrap();
        let runtime_id = ainb_hangar_store::bootstrap::default_runtime_id();
        ainb_hangar_store::bootstrap::register_runtime_instance(
            pool,
            &runtime_id,
            &previous,
            1_000,
        )
        .await
        .unwrap()
        .expect("previous instance registered");

        let boot = resolve_runtime_boot(pool, 2_000).await;
        assert_eq!(boot.runtime_id, runtime_id);
        assert_eq!(
            boot.arrival,
            RuntimeArrival::Restart {
                previous_instance_id: Some(previous)
            },
            "a dead predecessor's in-flight tasks are orphans"
        );
        assert_eq!(
            owner(pool, &runtime_id).await.as_ref(),
            Some(daemon_instance_id()),
            "this process now owns the runtime"
        );
    }

    /// A SECOND registration by the same process is a reconnect — the instance id
    /// is minted once per process, so the row already names us and nothing of ours
    /// is orphaned.
    #[tokio::test]
    async fn resolve_runtime_boot_twice_in_one_process_is_a_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_workspace(pool).await;

        assert!(
            resolve_runtime_boot(pool, 1_000).await.arrival.is_restart(),
            "an unowned runtime reads as a restart"
        );
        assert_eq!(
            resolve_runtime_boot(pool, 2_000).await.arrival,
            RuntimeArrival::Reconnect,
            "the same process must not look like it displaced itself"
        );
    }

    /// The boot SEED's resolve must not claim the instance: if it did, the real
    /// boot registration would see its own id, report a reconnect, and skip the
    /// orphan reconcile a genuine restart needs.
    #[tokio::test]
    async fn effective_runtime_id_does_not_claim_the_instance() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_workspace(pool).await;

        let runtime_id = effective_runtime_id(pool, 1_000).await;
        assert_eq!(
            owner(pool, &runtime_id).await,
            None,
            "the seed materialises the row without owning it"
        );
        assert!(
            resolve_runtime_boot(pool, 2_000).await.arrival.is_restart(),
            "so the boot registration still sees an unowned runtime"
        );
    }

    #[tokio::test]
    async fn register_runtime_is_a_noop_with_no_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        // No workspace seeded: registration is a benign no-op, never an error
        // (the FK would otherwise fail).
        let settled = register_runtime(pool, "rt-self", 1_000).await.unwrap();
        assert_eq!(settled, None, "no workspace ⇒ nothing to register");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runtime")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "no row written without a workspace");
    }
}
