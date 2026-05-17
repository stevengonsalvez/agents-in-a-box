//! Subprocess spawn + leak guard.
//!
//! ## Leak guard rationale
//!
//! If the host panics or is `SIGKILL`'d, plugin processes orphan and
//! become long-lived zombies attached to PID 1. Two OS-specific
//! mitigations:
//!
//! - **Linux**: `prctl(PR_SET_PDEATHSIG, SIGTERM)` inside the child's
//!   `pre_exec` hook. The kernel delivers `SIGTERM` to the child the
//!   moment the parent dies. Cheap, atomic, no userspace bookkeeping.
//!
//! - **macOS**: no `PR_SET_PDEATHSIG`. Best available substitute is
//!   `setpgid(0, 0)` in `pre_exec` (puts the child in its own process
//!   group equal to its pid) and `kill(-pgid, SIGTERM)` from
//!   [`Child::cleanup_on_drop`]. Misses the host-crashed case but
//!   covers `Drop` and graceful shutdown reliably.

use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::{Child as TokioChild, Command};

use crate::error::RuntimeError;

/// Spawn a child process for a plugin binary with the runtime's leak
/// guard installed.
///
/// Returns the tokio [`tokio::process::Child`] handle. Caller owns the
/// `Child` and is responsible for taking stdout/stdin and reading
/// from/writing to them.
pub fn spawn_plugin(binary_path: &PathBuf) -> Result<TokioChild, RuntimeError> {
    let mut cmd = Command::new(binary_path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // stderr piped so the runtime can drain it into tracing.
        .stderr(Stdio::piped())
        // Keep the child's process group separate from the host's so
        // a Ctrl-C on the host doesn't immediately blast the plugin —
        // the runtime decides when to kill via the lifecycle FSM.
        .kill_on_drop(true);

    install_leak_guard(&mut cmd);

    let child = cmd.spawn()?;
    Ok(child)
}

/// Install the OS-specific pre-exec hook used by the leak guard.
#[cfg(target_os = "linux")]
fn install_leak_guard(cmd: &mut Command) {
    // tokio::process::Command exposes `pre_exec` directly on Unix —
    // no `CommandExt` import needed.
    // PR_SET_PDEATHSIG = 1 (sys/prctl.h). SIGTERM = 15.
    const PR_SET_PDEATHSIG: libc::c_int = 1;
    // SAFETY: pre_exec runs in the child after fork before exec.
    // prctl is async-signal-safe, no allocation, no locks.
    unsafe {
        cmd.pre_exec(|| {
            let rc = libc::prctl(PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
            if rc == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // Move into our own process group so the host's TTY ^C
            // doesn't propagate. Mirrors the macOS code path.
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn install_leak_guard(cmd: &mut Command) {
    // SAFETY: setpgid is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn install_leak_guard(_cmd: &mut Command) {
    // Windows: `kill_on_drop(true)` is the strongest guarantee tokio
    // gives. We don't ship a Windows host today, so stop short of a
    // jobobject implementation.
}

/// Send SIGTERM to the child's process group.
///
/// Used by graceful shutdown and idle reaping. `setpgid(0, 0)` in the
/// pre-exec hook makes the child a process-group leader equal to its
/// pid, so `kill(-pid, SIGTERM)` reaches every descendant.
#[cfg(unix)]
pub fn signal_pgrp(pid: i32, signal: i32) -> std::io::Result<()> {
    // SAFETY: kill is async-signal-safe; we pass a negative pid which
    // libc interprets as a process group target.
    unsafe {
        if libc::kill(-pid, signal) == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn signal_pgrp(_pid: i32, _signal: i32) -> std::io::Result<()> {
    Ok(())
}

/// `SIGTERM` constant exposed for callers that don't want to depend on
/// libc directly.
#[cfg(unix)]
pub const SIGTERM: i32 = libc::SIGTERM;

#[cfg(not(unix))]
pub const SIGTERM: i32 = 15;
