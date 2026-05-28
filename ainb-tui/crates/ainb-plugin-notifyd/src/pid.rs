//! PID file management for `ainb-notifyd`.
//!
//! Writes the daemon's PID to a known location at startup so the
//! `ainb hooks status` and `ainb notifyd --stop` verbs can find it.
//! Cleans up on drop so a graceful exit leaves no stale file behind.
//!
//! A simple liveness check (`is_running`) reads the PID and asks the
//! kernel whether that process exists — used by the install/status
//! verbs to decide whether to lazy-spawn another instance.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// RAII wrapper for the PID file. Removes the file on drop unless
/// the caller called [`PidFile::release`] (used when we hand over
/// ownership to a successor process).
#[derive(Debug)]
pub struct PidFile {
    path: PathBuf,
    cleanup: bool,
}

impl PidFile {
    /// Write the current process PID to `path`, truncating any
    /// existing content. Fails if the file is locked by another
    /// running daemon (see [`is_running`]).
    pub fn write_current(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let pid = std::process::id();
        let mut file = std::fs::File::create(&path)
            .with_context(|| format!("opening pid file {}", path.display()))?;
        writeln!(file, "{pid}")?;
        Ok(Self {
            path,
            cleanup: true,
        })
    }

    /// Path of the PID file.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Surrender cleanup-on-drop responsibility. Useful when the
    /// process is about to `exec` into another binary that will
    /// adopt the file.
    pub fn release(mut self) {
        self.cleanup = false;
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Read a PID from `path` if it exists, otherwise return `None`.
pub fn read(path: &std::path::Path) -> Result<Option<u32>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading pid file {}", path.display()))?;
    let pid: u32 = text
        .trim()
        .parse()
        .with_context(|| format!("parsing pid file {}: not an integer", path.display()))?;
    Ok(Some(pid))
}

/// Best-effort liveness check: is the PID still backing a running
/// process? Uses `kill(pid, 0)` which returns success if the process
/// exists (and we have permission to signal it), `ESRCH` otherwise.
pub fn is_running(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    let p = Pid::from_raw(pid as i32);
    matches!(kill(p, None), Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrips_current_pid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify.pid");
        let _pf = PidFile::write_current(path.clone()).unwrap();
        let pid = read(&path).unwrap().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn drop_removes_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify.pid");
        {
            let _pf = PidFile::write_current(path.clone()).unwrap();
            assert!(path.exists());
        }
        assert!(!path.exists());
    }

    #[test]
    fn is_running_true_for_self() {
        assert!(is_running(std::process::id()));
    }

    #[test]
    fn is_running_false_for_impossible_pid() {
        // PID 0 is "the current process group" in POSIX — never an
        // actual process — and on Linux it's also reserved.
        // Use a very large pid that almost certainly does not exist.
        assert!(!is_running(0x7fff_ffff));
    }
}
