//! Parent-death watcher: a SIGKILL/crash backstop against orphaned plugins.
//!
//! ## Why this exists
//!
//! Plugins are spawned as subprocesses of the host (`ainb tui`). The host
//! installs OS-specific leak guards (see `ainb-plugin-runtime::process`):
//!
//! - **Linux**: `prctl(PR_SET_PDEATHSIG, SIGTERM)` in the child's
//!   `pre_exec`. The kernel signals the child the instant the parent dies —
//!   crash-safe, no userspace polling. Linux is therefore fully covered and
//!   this watcher is a no-op there.
//!
//! - **macOS**: there is *no* `PR_SET_PDEATHSIG` equivalent. The host falls
//!   back to `setpgid(0, 0)` + `kill(-pgid, SIGTERM)` from its `Drop`. That
//!   handles graceful shutdown and Ctrl-C, but a `SIGKILL`'d / crashed host
//!   never runs `Drop`, so the plugin (sitting in its own process group)
//!   survives and reparents to `launchd` (PPID = 1). `kill_on_drop` does not
//!   help either — `SIGKILL` bypasses `Drop`.
//!
//! The robust macOS fix is to watch for parent death *from inside the
//! plugin* and self-exit. We poll `getppid()` once per second:
//!
//! - returns `1` → reparented to `launchd` → host died → exit.
//! - differs from the pid captured at startup → original parent gone → exit.
//!
//! This is **additive**: the normal stdin-EOF graceful-exit path in
//! [`crate::server::Server::run`] is untouched. The watcher only fires when
//! the host vanishes without closing our stdin (the crash / `SIGKILL` case).
//!
//! ## Safety
//!
//! `nix::unistd::getppid()` is a safe wrapper around the syscall, so this
//! module compiles under the crate-wide `unsafe_code = "forbid"`. The poll is
//! a 1 s interval, never a tight spin.

/// Poll interval for the parent-death check. One second is responsive enough
/// that an orphan is reaped within ~1 s of the host dying, while costing a
/// single cheap syscall per second.
#[cfg(target_os = "macos")]
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// `launchd` / `init` pid. A `getppid()` of `1` means our original parent died
/// and we were reparented — the unambiguous "host is gone" signal on macOS.
const INIT_PID: i32 = 1;

/// Pure predicate: given the parent pid captured at startup and the parent pid
/// observed now, decide whether the plugin should self-exit.
///
/// Returns `true` when:
/// - the current parent is `init`/`launchd` (pid `1`) — we were reparented, or
/// - the current parent differs from the original — the original parent exited
///   and (on some scheduling orders) a new pid took its slot.
///
/// Kept separate from the polling loop so it can be unit-tested without
/// actually terminating the test process.
#[must_use]
pub const fn should_exit(original_parent: i32, current_parent: i32) -> bool {
    current_parent == INIT_PID || current_parent != original_parent
}

/// Spawn the macOS parent-death watcher.
///
/// Captures the current parent pid, then spawns a background tokio task that
/// polls `getppid()` every [`POLL_INTERVAL`]. When [`should_exit`] returns
/// `true`, it logs one line to stderr (drained by the host's stderr
/// forwarder, if it is still alive) and calls `std::process::exit(0)`.
///
/// On non-macOS targets this is a no-op: Linux is covered by the host-side
/// `PR_SET_PDEATHSIG`, and we don't ship a Windows host. Either way it must
/// never break those builds, hence the `#[cfg]` split.
#[cfg(target_os = "macos")]
pub fn spawn_parent_death_watcher() {
    let original_parent = nix::unistd::getppid().as_raw();

    // Degenerate case: if our parent is *already* init at startup, the host
    // died during/before spawn. Exit immediately instead of waiting a full
    // interval. (Normal spawn always has the live host as parent.)
    if original_parent == INIT_PID {
        eprintln!("ainb-plugin: parent already init at startup — self-exiting");
        std::process::exit(0);
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let current_parent = nix::unistd::getppid().as_raw();
            if should_exit(original_parent, current_parent) {
                eprintln!(
                    "ainb-plugin: parent process died (ppid {original_parent} -> \
                     {current_parent}) — self-exiting to avoid orphaning"
                );
                // Flush stderr best-effort before exit so the log line is not
                // lost. exit(0): orphan-avoidance is a clean shutdown, not a
                // failure.
                let _ = std::io::Write::flush(&mut std::io::stderr());
                std::process::exit(0);
            }
        }
    });
}

/// No-op on non-macOS targets — see [`spawn_parent_death_watcher`] (macOS).
#[cfg(not(target_os = "macos"))]
pub fn spawn_parent_death_watcher() {
    // Linux: the host installs `PR_SET_PDEATHSIG`, which the kernel honours on
    // parent death — no in-plugin watcher needed. Other targets have no host.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_exit_while_parent_unchanged() {
        // Original parent still alive and unchanged: stay running.
        assert!(!should_exit(4242, 4242));
    }

    #[test]
    fn exits_when_reparented_to_init() {
        // Reparented to launchd/init — the canonical macOS orphan signal.
        assert!(should_exit(4242, INIT_PID));
    }

    #[test]
    fn exits_when_parent_pid_changes() {
        // Original parent exited and a different pid now reports as parent.
        assert!(should_exit(4242, 9001));
    }

    #[test]
    fn init_pid_is_one() {
        // Guards the constant against accidental edits.
        assert_eq!(INIT_PID, 1);
    }
}
