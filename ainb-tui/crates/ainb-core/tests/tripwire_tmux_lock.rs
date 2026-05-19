//! Cross-process serializing lock for tmux-bound tripwire tests.
//!
//! The 5 `tripwire_*.rs` integration tests that spawn `tmux + ainb` each
//! live in their own cargo test binary, so cargo runs them as parallel
//! processes regardless of `--test-threads=1` (that flag serialises
//! tests inside a single binary, not binaries against each other).
//! Under the resulting CPU/IO contention the burndown plugin's
//! eager-spawn + session-reader's first-scan can exceed the 30–45s
//! polling deadlines and produce a heisenbug failure ("burndown never
//! rendered real data after `i`") even though each test passes solo
//! in <10s.
//!
//! Included via `#[path = "tripwire_tmux_lock.rs"] mod tripwire_tmux_lock;`
//! and acquired at the very top of each tmux-bound test with
//! `let _lock = tripwire_tmux_lock::TmuxSerialLock::acquire();`.
//!
//! The lock is an O_EXCL-create on a known pidfile. If the recorded
//! holder PID is dead (crash, killed), the lock is stolen on the next
//! poll. `Drop` removes the file so the next waiter can acquire.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const LOCK_PATH: &str = "/tmp/ainb-tripwire-tmux.lock";
const ACQUIRE_TIMEOUT_SECS: u64 = 600;
const POLL_INTERVAL_MS: u64 = 250;

#[allow(dead_code)] // each test binary includes this module via #[path]; not every binary uses every item
pub struct TmuxSerialLock {
    path: PathBuf,
}

#[allow(dead_code)] // see note above on multi-binary inclusion
impl TmuxSerialLock {
    pub fn acquire() -> Self {
        let path = PathBuf::from(LOCK_PATH);
        let deadline = Instant::now() + Duration::from_secs(ACQUIRE_TIMEOUT_SECS);
        let our_pid = std::process::id();

        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{our_pid}");
                    return Self { path };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(holder) = read_holder_pid(&path) {
                        if !pid_alive(holder) {
                            eprintln!(
                                "TmuxSerialLock: stealing stale lock from dead PID {holder}"
                            );
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                    }
                    if Instant::now() > deadline {
                        panic!(
                            "TmuxSerialLock: timed out after {ACQUIRE_TIMEOUT_SECS}s \
                             waiting for {LOCK_PATH}"
                        );
                    }
                    thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
                }
                Err(e) => panic!("TmuxSerialLock: open failed: {e}"),
            }
        }
    }
}

impl Drop for TmuxSerialLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_holder_pid(path: &PathBuf) -> Option<i32> {
    let mut buf = String::new();
    File::open(path).ok()?.read_to_string(&mut buf).ok()?;
    buf.trim().parse::<i32>().ok()
}

/// Cheap `kill -0` probe to check if a PID is still alive without
/// pulling in libc/nix. Returns false if the PID is gone or the shell
/// `kill` builtin isn't on PATH (which would be a fatal CI environment
/// problem worth surfacing as a stuck lock anyway).
fn pid_alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
