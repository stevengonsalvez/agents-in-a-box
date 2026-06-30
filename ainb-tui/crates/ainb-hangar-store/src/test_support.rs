//! Test-only helpers for driving [`crate::Store`] against an isolated home.
//!
//! Cargo runs tests in parallel within a single process, so any test that
//! mutates process-global state (here, the `AINB_HANGAR_HOME` env var) races
//! every other such test. The [`ENV_LOCK`] mutex serialises those mutations and
//! [`with_isolated_home`] guarantees the prior value is restored even when the
//! closure panics. This mirrors the `ENV_LOCK` discipline used elsewhere in the
//! workspace for parallel env-mutating tests.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use tempfile::TempDir;

use crate::Store;

/// Process-wide mutex serialising mutations of the `AINB_HANGAR_HOME` env var.
///
/// Every test that redirects the Hangar home must go through
/// [`with_isolated_home`] (which holds this lock for the duration of the
/// closure) or — for tests that mutate the env var directly — must acquire this
/// lock explicitly via [`lock_env`] and hold it across the mutation. No other
/// code in the workspace should set `AINB_HANGAR_HOME`.
pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Acquire [`ENV_LOCK`], recovering from a poisoned lock so a prior panicking
/// test cannot wedge the rest of the suite.
///
/// Use this from any test that mutates `AINB_HANGAR_HOME` outside
/// [`with_isolated_home`] (e.g. to set/restore a sentinel around a nested
/// isolated-home run). Hold the returned guard across the whole mutation
/// window. Pass the guard into [`with_isolated_home_locked`] when you also need
/// an isolated home inside the same critical section — re-locking [`ENV_LOCK`]
/// while already holding it would deadlock (`std::sync::Mutex` is not
/// reentrant).
///
/// # Panics
///
/// Does not panic; a poisoned lock is recovered transparently.
pub fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Run `f` with `AINB_HANGAR_HOME` pointed at a fresh tempdir, then restore the
/// previous value (or unset it) regardless of how `f` returns.
///
/// The tempdir is created, [`ENV_LOCK`] is acquired (recovering from a poisoned
/// lock so one panicking test does not wedge the rest of the suite), the env var
/// is set to the tempdir path, and `f` is invoked with the tempdir path. After
/// `f` returns, the env var is restored to whatever it was before and the
/// tempdir is removed.
///
/// `f` receives the isolated home directory so it can assert the database landed
/// under it. Because `AINB_HANGAR_HOME` is treated as the directory that
/// directly holds the database, the db is created at `<home>/hangar.db` (no
/// `.agents-in-a-box` segment) by [`Store::open_default`].
///
/// # Panics
///
/// Panics if a tempdir cannot be created.
pub fn with_isolated_home<F, R>(f: F) -> R
where
    F: FnOnce(&Path) -> R,
{
    let guard = lock_env();
    with_isolated_home_locked(&guard, f)
}

/// Like [`with_isolated_home`], but assumes the caller already holds
/// [`ENV_LOCK`] (proven by the borrowed `_guard`).
///
/// Use this when a test needs to mutate `AINB_HANGAR_HOME` itself AND run an
/// isolated-home block within the same critical section. Acquire the lock once
/// via [`lock_env`], then thread the guard through here — this avoids
/// re-locking the non-reentrant [`ENV_LOCK`], which would deadlock.
///
/// # Panics
///
/// Panics if a tempdir cannot be created.
pub fn with_isolated_home_locked<F, R>(_guard: &MutexGuard<'static, ()>, f: F) -> R
where
    F: FnOnce(&Path) -> R,
{
    /// Restores (or unsets) the home env var on drop, so the previous value is
    /// recovered even if `f` panics.
    struct Restore<'a> {
        key: &'a str,
        prior: Option<std::ffi::OsString>,
    }
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    let tmp: TempDir = tempfile::tempdir().expect("create tempdir for isolated home");
    let key = Store::home_env();
    let prior = std::env::var_os(key);

    std::env::set_var(key, tmp.path());
    let _restore = Restore { key, prior };

    f(tmp.path())
}
