//! Subprocess-level tests for [`ainb_plugin_abtop::detect::detect_abtop`].
//!
//! These run the actual `which` path with a stub `abtop` binary staged
//! in a tempdir prepended to `PATH`. Detection is present/absent only
//! (no version probe), so there's no exec to stub — just the PATH
//! resolution:
//!
//! - Ready: a stub `abtop` exists on PATH.
//! - Missing/NotOnPath: empty `PATH`.
//!
//! `PATH` mutation is process-global and races every other test in the
//! binary that also reads `PATH`. We serialize via a single `Mutex`
//! per the [[reference_env_lock_for_parallel_tests]] pattern and never
//! run these in parallel.
//!
//! Gated on `cfg(unix)`: the stub is a `#!/bin/sh` script marked
//! executable via `chmod`, and the support matrix is macOS + Linux.
//! See [[reference_rust_unix_only_apis]].

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use ainb_plugin_abtop::detect::{DetectResult, MissingReason, detect_abtop};
use tokio::sync::Mutex;

/// Process-wide mutex around `std::env::set_var("PATH", ...)`. Cargo
/// runs tests within a binary on multiple threads by default; any test
/// that mutates env vars MUST acquire this before doing so. We use
/// `tokio::sync::Mutex` (not `std::sync::Mutex`) because the guard is
/// held across `detect_abtop().await` — a std Mutex held across an
/// await deadlocks under multi-threaded tokio runtimes.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// Drop guard that restores the original `PATH` when the test ends —
/// even on panic.
struct PathGuard {
    original: Option<std::ffi::OsString>,
}

impl PathGuard {
    fn install(new_path: &str) -> Self {
        let original = std::env::var_os("PATH");
        // SAFETY: documented to be unsound only when other threads read
        // env vars concurrently. We hold `ENV_LOCK` for the duration of
        // the test, which is the project's serialization contract for
        // env mutation.
        unsafe { std::env::set_var("PATH", new_path) };
        Self { original }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => unsafe { std::env::set_var("PATH", v) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

/// Stage a stub `abtop` shell script inside `dir`, marked executable.
/// The body is irrelevant — detection only resolves the binary via
/// `which` and never execs it — but a runnable `#!/bin/sh` no-op keeps
/// the stub honest.
fn stage_abtop_stub(dir: &Path) -> PathBuf {
    let bin = dir.join("abtop");
    fs::write(&bin, "#!/bin/sh\nexit 0\n").expect("write stub abtop script");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&bin).expect("stat stub").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).expect("chmod stub");
    bin
}

#[tokio::test]
async fn detects_ready_when_abtop_on_path() {
    let _lock = ENV_LOCK.lock().await;
    // `_path_guard` MUST be declared AFTER `_lock`. Rust drops locals in
    // reverse declaration order, so the guard restores PATH first, then
    // the lock is released — letting the next test acquire the lock
    // against a clean env. Flipping the order races.
    let dir = tempfile::tempdir().expect("tempdir");
    stage_abtop_stub(dir.path());

    let _path_guard = PathGuard::install(dir.path().to_str().expect("utf8 tempdir"));

    let result = detect_abtop().await;
    match result {
        DetectResult::Ready { path } => {
            assert_eq!(path, dir.path().join("abtop"));
        }
        other => panic!("expected Ready, got {other:?}"),
    }
}

#[tokio::test]
async fn reports_missing_when_path_empty() {
    let _lock = ENV_LOCK.lock().await;
    let _path_guard = PathGuard::install("");

    let result = detect_abtop().await;
    assert!(
        matches!(result, DetectResult::Missing(MissingReason::NotOnPath)),
        "expected Missing(NotOnPath), got {result:?}",
    );
}
