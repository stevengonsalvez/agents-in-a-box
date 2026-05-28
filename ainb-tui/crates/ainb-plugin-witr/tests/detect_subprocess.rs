//! Subprocess-level tests for [`ainb_plugin_witr::detect::detect_witr`].
//!
//! These run the actual `which`-+-exec path with a stub `witr` binary
//! staged in a tempdir prepended to `PATH`. The pure parser surface is
//! unit-tested inside `src/detect.rs`; this file covers the side
//! effects only:
//!
//! - Ready: stub prints valid release line + correct exit.
//! - Missing/NotOnPath: empty `PATH`.
//! - Outdated: stub prints a v0.3.1 line (below `MIN_VERSION`).
//! - Missing/VersionExecFailed: stub exits non-zero.
//! - Missing/VersionExecFailed: stub sleeps past timeout.
//!
//! `PATH` mutation is process-global and races every other test in
//! the binary that also reads `PATH`. We serialize via a single
//! `Mutex` per the [[reference_env_lock_for_parallel_tests]] pattern
//! and never run these in parallel.
//!
//! Stub binaries are shell scripts marked executable via `chmod`. The
//! file is gated on `cfg(unix)` because both the `chmod` syscall and
//! the `#!/bin/sh` interpreter assumption only hold on Unix. Witr v1
//! support matrix is macOS arm64 + Linux x86_64 — Windows is out of
//! scope. See [[reference_rust_unix_only_apis]].

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use ainb_plugin_witr::detect::{DetectResult, MissingReason, detect_witr};
use tokio::sync::Mutex;

/// Process-wide mutex around `std::env::set_var("PATH", ...)`. Cargo
/// runs tests within a binary on multiple threads by default; any
/// test that mutates env vars MUST acquire this before doing so. We
/// use `tokio::sync::Mutex` (not `std::sync::Mutex`) because the
/// guard is held across `detect_witr().await` — std Mutex held across
/// an await deadlocks under multi-threaded tokio runtimes.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

/// Drop guard that restores the original `PATH` when the test ends —
/// even on panic. Without this a failing test would leak its stubbed
/// `PATH` into siblings, masking the actual failure with cascading
/// "binary not found" errors.
struct PathGuard {
    original: Option<std::ffi::OsString>,
}

impl PathGuard {
    fn install(new_path: &str) -> Self {
        let original = std::env::var_os("PATH");
        // SAFETY: documented to be unsound only when other threads
        // read env vars concurrently. We hold `ENV_LOCK` for the
        // duration of the test, which is the project's serialization
        // contract for env mutation.
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

/// Stage a stub `witr` shell script inside `dir`, marked executable,
/// printing exactly `version_line` to stdout and exiting 0. Returns
/// the directory so the caller can hand it to [`PathGuard::install`].
fn stage_witr_stub(dir: &Path, version_line: &str) -> PathBuf {
    let bin = dir.join("witr");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' '{version_line}'\n  exit 0\nfi\nprintf 'stub-witr: unhandled args: %s\\n' \"$*\" >&2\nexit 1\n",
    );
    fs::write(&bin, script).expect("write stub witr script");
    let mut perms = fs::metadata(&bin).expect("stat stub").permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).expect("chmod stub");
    bin
}

/// Stage a stub that exits non-zero on `--version` — for the
/// VersionExecFailed path. Gates on the actual argv so a future
/// detect refactor that probes a different flag fails this stub
/// loudly instead of incidentally succeeding.
fn stage_witr_stub_failing(dir: &Path) -> PathBuf {
    let bin = dir.join("witr");
    let script = "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf 'fake exec failure\\n' >&2\n  exit 7\nfi\nprintf 'stub-witr: unexpected args: %s\\n' \"$*\" >&2\nexit 2\n".to_string();
    fs::write(&bin, script).expect("write failing stub");
    let mut perms = fs::metadata(&bin).expect("stat stub").permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).expect("chmod stub");
    bin
}

/// Stage a stub that sleeps longer than `VERSION_EXEC_TIMEOUT` so we
/// can pin the timeout branch of [`detect_witr`]. Uses `/bin/sleep`
/// directly because the test installs a PATH containing only the
/// stub's tempdir — bare `sleep` wouldn't resolve.
fn stage_witr_stub_hanging(dir: &Path) -> PathBuf {
    let bin = dir.join("witr");
    let script =
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  /bin/sleep 30\n  exit 0\nfi\nexit 2\n"
            .to_string();
    fs::write(&bin, script).expect("write hanging stub");
    let mut perms = fs::metadata(&bin).expect("stat stub").permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    fs::set_permissions(&bin, perms).expect("chmod stub");
    bin
}

#[tokio::test]
async fn detects_ready_against_stub() {
    let _lock = ENV_LOCK.lock().await;
    // `_path_guard` MUST be declared AFTER `_lock`. Rust drops locals
    // in reverse declaration order, so the guard restores PATH first,
    // then the lock is released — letting the next test acquire the
    // lock against a clean env. Flipping the order races.
    let dir = tempfile::tempdir().expect("tempdir");
    stage_witr_stub(
        dir.path(),
        "witr v0.3.2 (commit a1b2c3d, built 2025-04-10T12:00:00Z)",
    );

    let _path_guard = PathGuard::install(dir.path().to_str().expect("utf8 tempdir"));

    let result = detect_witr().await;
    match result {
        DetectResult::Ready {
            path,
            version,
            commit,
            built_at,
        } => {
            assert_eq!(path, dir.path().join("witr"));
            assert_eq!(version.to_string(), "0.3.2");
            assert_eq!(commit.as_deref(), Some("a1b2c3d"));
            assert_eq!(built_at.as_deref(), Some("2025-04-10T12:00:00Z"));
        }
        other => panic!("expected Ready, got {other:?}"),
    }
}

#[tokio::test]
async fn reports_outdated_when_below_minimum() {
    let _lock = ENV_LOCK.lock().await;
    // `_path_guard` MUST be declared AFTER `_lock`. Rust drops locals
    // in reverse declaration order, so the guard restores PATH first,
    // then the lock is released — letting the next test acquire the
    // lock against a clean env. Flipping the order races.
    let dir = tempfile::tempdir().expect("tempdir");
    stage_witr_stub(
        dir.path(),
        "witr v0.3.1 (commit a1b2c3d, built 2025-03-01T00:00:00Z)",
    );

    let _path_guard = PathGuard::install(dir.path().to_str().expect("utf8 tempdir"));

    let result = detect_witr().await;
    match result {
        DetectResult::Outdated {
            found_version,
            minimum,
        } => {
            assert_eq!(found_version.to_string(), "0.3.1");
            assert_eq!(minimum.to_string(), "0.3.2");
        }
        other => panic!("expected Outdated, got {other:?}"),
    }
}

#[tokio::test]
async fn reports_missing_when_path_empty() {
    let _lock = ENV_LOCK.lock().await;
    // `_path_guard` MUST be declared AFTER `_lock`. Rust drops locals
    // in reverse declaration order, so the guard restores PATH first,
    // then the lock is released — letting the next test acquire the
    // lock against a clean env. Flipping the order races.
    let _path_guard = PathGuard::install("");

    let result = detect_witr().await;
    assert!(
        matches!(result, DetectResult::Missing(MissingReason::NotOnPath)),
        "expected Missing(NotOnPath), got {result:?}",
    );
}

#[tokio::test]
async fn reports_version_exec_failure_when_stub_exits_non_zero() {
    let _lock = ENV_LOCK.lock().await;
    // `_path_guard` MUST be declared AFTER `_lock`. Rust drops locals
    // in reverse declaration order, so the guard restores PATH first,
    // then the lock is released — letting the next test acquire the
    // lock against a clean env. Flipping the order races.
    let dir = tempfile::tempdir().expect("tempdir");
    stage_witr_stub_failing(dir.path());

    let _path_guard = PathGuard::install(dir.path().to_str().expect("utf8 tempdir"));

    let result = detect_witr().await;
    match result {
        DetectResult::Missing(MissingReason::VersionExecFailed(msg)) => {
            assert!(
                msg.contains("exited") || msg.contains("exit"),
                "diagnostic should mention exit status: {msg}",
            );
        }
        other => panic!("expected Missing(VersionExecFailed), got {other:?}"),
    }
}

#[tokio::test]
async fn reports_version_exec_failure_when_stub_hangs_past_timeout() {
    // Pins the `timeout` branch of detect_witr. Stub sleeps 30s; the
    // detect timeout is 5s. We kill the child via tokio's
    // process-drop semantics when the timeout fires — the test
    // returns within ~5s, not 30s, and the parent stub script is
    // reaped by the OS as the pipe closes.
    let _lock = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().expect("tempdir");
    stage_witr_stub_hanging(dir.path());

    let _path_guard = PathGuard::install(dir.path().to_str().expect("utf8 tempdir"));

    let result = detect_witr().await;
    match result {
        DetectResult::Missing(MissingReason::VersionExecFailed(msg)) => {
            assert!(
                msg.contains("timed out"),
                "diagnostic should mention timeout: {msg}",
            );
        }
        other => panic!("expected Missing(VersionExecFailed) for timeout, got {other:?}"),
    }
}
