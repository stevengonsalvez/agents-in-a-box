//! The in-flight run kill registry (tcp T3 / F6).
//!
//! A cancel arrives on the RPC server task, but the run it must stop lives on
//! the claim loop's [`JoinSet`](tokio::task::JoinSet) — a different task. This
//! process-global registry is the seam between them: while a run is executing,
//! the claim loop holds a [`RunCancelGuard`] whose [`CancellationToken`] it
//! selects on; the cancel RPC looks the task up by id and [`signal`]s that
//! token, unblocking the run's `select!` so it kills its provider (a headless
//! process group via `kill_on_drop`, or the interactive tmux session by exact
//! name) and finalises through the dedicated cancelled seam.
//!
//! # Why a process-global
//!
//! The registry is inherently process-scoped state — every in-flight run in
//! *this* daemon — and the RPC server ([`crate::rpc::serve`]) and the claim loop
//! ([`crate::run_loop::run`]) are spawned as independent tasks with no shared
//! owner to thread a handle through. A single [`LazyLock`] instance (the same
//! pattern the PR-status cache uses in [`crate::pr_status`]) is shared by both
//! without widening a dozen `serve`/`dispatch`/`run` signatures. Task ids are
//! ULIDs — globally unique — so the map is never ambiguous across concurrent
//! runs or successive daemon lifetimes.
//!
//! # What it does NOT do
//!
//! The token only *signals* a run to stop; the authoritative
//! `running -> cancelled` DB transition is [`CancelTaskService`] (the RPC does
//! it BEFORE signalling, so the DB is cancelled the instant the RPC returns),
//! and the SQL idempotent-finalize arbitrates the cancel-vs-natural-finish race
//! (a run that finished first leaves the cancel a no-op, and vice versa).
//!
//! [`CancelTaskService`]: ainb_hangar_store::service::cancel::CancelTaskService
//! [`signal`]: CancelRegistry::signal

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use tokio_util::sync::CancellationToken;

/// The one process-global kill registry (see the module docs).
static REGISTRY: LazyLock<CancelRegistry> = LazyLock::new(CancelRegistry::default);

/// The daemon's shared in-flight run kill registry.
#[must_use]
pub fn registry() -> &'static CancelRegistry {
    &REGISTRY
}

/// Maps a live run's task id to the [`CancellationToken`] the claim loop selects
/// on, so a cancel RPC on another task can stop it.
#[derive(Default)]
pub struct CancelRegistry {
    inner: Mutex<HashMap<String, CancellationToken>>,
}

impl CancelRegistry {
    /// Register `task_id` as an in-flight run and return a guard the claim loop
    /// selects on. The returned [`RunCancelGuard`] deregisters the task on drop
    /// (every exit path — normal finish, cancel, or panic-unwind), so the map
    /// stays bounded to genuinely-live runs.
    ///
    /// A pre-existing entry for the id (impossible with unique ULIDs, but
    /// defended) is replaced, never duplicated.
    pub fn register(&self, task_id: &str) -> RunCancelGuard {
        let token = CancellationToken::new();
        if let Ok(mut map) = self.inner.lock() {
            map.insert(task_id.to_string(), token.clone());
        }
        RunCancelGuard {
            task_id: task_id.to_string(),
            token,
        }
    }

    /// Signal the run registered for `task_id` to stop, returning `true` when a
    /// live run was found and signalled (`false` when no in-flight run is
    /// registered — the task is queued-but-unclaimed, already terminal, or run
    /// by a different daemon).
    ///
    /// Cancelling an already-cancelled token is a harmless no-op, so a double
    /// signal is safe.
    pub fn signal(&self, task_id: &str) -> bool {
        let Ok(map) = self.inner.lock() else {
            return false;
        };
        match map.get(task_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Whether `task_id` has a live registered run IN THIS PROCESS. The worktree
    /// GC's liveness guard (tcp yjj): a manual board transition can terminal-mark a
    /// task whose provider is still running, and the GC must not delete a live
    /// run's checkout out from under it. A poisoned lock reports `true` — on
    /// uncertainty, treat the run as live (never delete what might be running).
    #[must_use]
    pub fn is_live(&self, task_id: &str) -> bool {
        self.inner.lock().map_or(true, |map| map.contains_key(task_id))
    }

    /// Remove `task_id`'s entry (the guard's drop path). A poisoned lock leaves
    /// the entry — a bounded, ULID-keyed leak that never mis-signals a later run.
    fn unregister(&self, task_id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(task_id);
        }
    }
}

/// A live run's registration in the [`CancelRegistry`]. The claim loop awaits
/// [`RunCancelGuard::cancelled`] in its run `select!`; dropping the guard
/// deregisters the task.
pub struct RunCancelGuard {
    task_id: String,
    token: CancellationToken,
}

impl RunCancelGuard {
    /// Resolve once the run has been signalled to cancel (the cancel RPC called
    /// [`CancelRegistry::signal`] for this task). Cheap to poll in a `select!`.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

impl Drop for RunCancelGuard {
    fn drop(&mut self) {
        registry().unregister(&self.task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::CancelRegistry;

    /// A signal to a registered task trips its guard's `cancelled` future; an
    /// unknown task returns `false`.
    #[tokio::test]
    async fn signal_trips_a_registered_run() {
        let reg = CancelRegistry::default();
        // Using the local instance (not the global) keeps the test isolated.
        let token = tokio_util::sync::CancellationToken::new();
        reg.inner.lock().unwrap().insert("01HZTASK".to_string(), token.clone());

        assert!(!reg.signal("nope"), "unknown task signals nothing");
        assert!(!token.is_cancelled(), "an unrelated task stays live");

        assert!(reg.signal("01HZTASK"), "a registered task is signalled");
        assert!(token.is_cancelled(), "the signalled token is tripped");
        // The awaited future resolves promptly now the token is cancelled.
        token.cancelled().await;
    }

    /// Dropping a guard deregisters the task, so a later signal finds nothing —
    /// the map never mis-signals a reaped run.
    #[test]
    fn guard_drop_deregisters() {
        // Exercises the real global (guards deregister through `registry()`).
        let reg = super::registry();
        {
            let _guard = reg.register("01HZDROPME");
            assert!(reg.signal("01HZDROPME"), "live while the guard is held");
        }
        assert!(
            !reg.signal("01HZDROPME"),
            "deregistered once the guard dropped"
        );
    }
}
