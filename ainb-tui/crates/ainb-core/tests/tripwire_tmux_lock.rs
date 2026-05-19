//! Cross-process serializing lock for tripwire tests that spawn the
//! real `ainb` binary plus the plugin runtime (session-reader,
//! burndown).
//!
//! The tripwire integration tests live in separate cargo test binaries,
//! so cargo runs them as parallel processes regardless of
//! `--test-threads=1` (that flag serialises tests inside a single
//! binary, not binaries against each other). Two failure modes
//! materialise under that contention:
//!
//! 1. **tmux-bound tests**: a single `send-keys` can be dropped before
//!    the host's event loop drains it, parking the test on HomeScreen
//!    until timeout ("burndown never rendered real data after `i`").
//! 2. **`ainb usage` CLI tests**: spawning ainb + session-reader
//!    concurrently across binaries starves session-reader's publish
//!    pipeline so its first usage_data snapshot never reaches the
//!    host's snapshot store within the budget ("session-reader plugin
//!    didn't publish usage data within Ns"). The same regression
//!    surfaces in fixture_e2e::send_key_forwards_handle_key_notification
//!    in the plugin-runtime crate (currently `#[ignore]`'d).
//!
//! Both classes pass solo (<10s) but fail under L1 ci. This lock
//! serialises them at the OS level via an O_EXCL pidfile so only one
//! plugin-spawning tripwire is running at any given moment across the
//! entire `cargo test --workspace` invocation. If the recorded holder
//! PID is dead (crash, killed), the lock is stolen on the next poll.
//! `Drop` removes the file so the next waiter can acquire.
//!
//! Included via
//! `#[path = "tripwire_tmux_lock.rs"] mod tripwire_tmux_lock;`
//! and acquired at the very top of each affected test with
//! `let _lock = tripwire_tmux_lock::TmuxSerialLock::acquire();`.
//!
//! The struct + module names retain `tmux` for backwards-compat with
//! the original 5-tripwire deployment; the underlying lock is now
//! shared with non-tmux plugin-spawn tests.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const LOCK_PATH: &str = "/tmp/ainb-tripwire-plugin-spawn.lock";
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
