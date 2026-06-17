//! Real-binary smoke: exec the actual installed `abtop --once` through
//! the plugin's [`exec_abtop_once`] and prove the exec path is wired
//! against the real binary.
//!
//! **Skips gracefully** when abtop isn't on PATH (CI runners without
//! it), so it's safe in `cargo test --workspace`.
//!
//! ## Why this test tolerates a timeout
//!
//! abtop is a crossterm/ratatui *alternate-screen* TUI. `--once` only
//! snapshots cleanly when its stdout is a real interactive terminal.
//! Under `cargo test`, the plugin captures abtop's stdout via a pipe
//! (non-TTY), where abtop blocks in its event loop instead of printing
//! a one-shot snapshot. [`exec_abtop_once`] reaps that hang via its
//! [`EXEC_TIMEOUT`] + `kill_on_drop` — which is exactly the production
//! behaviour we want to prove is safe.
//!
//! So this test accepts two honest outcomes when abtop IS installed:
//!
//! 1. `ExecResult::Ok(text)` — abtop produced a snapshot to the pipe.
//!    Then the captured text MUST contain the stable em-dash header
//!    `"abtop —"` (U+2014), which is the first line abtop prints:
//!    `abtop — N sessions, M mcp servers`.
//! 2. `ExecResult::Timeout` / `ExecResult::NonZero` — abtop blocked or
//!    refused the non-TTY pipe and was reaped/exited. This is the
//!    common case in a piped test harness and proves the exec is
//!    bounded and never hangs the suite.
//!
//! `ExecResult::SpawnFailed` is a genuine regression (the binary
//! resolved via `which` but couldn't be spawned at all), so we fail
//! loudly on it.

#![cfg(unix)]

use ainb_plugin_abtop::exec::{ExecResult, exec_abtop_once};

/// The stable string a captured abtop snapshot must contain: the
/// em-dash header. abtop's first line is `abtop — N sessions, …`.
const ABTOP_HEADER: &str = "abtop —";

#[tokio::test]
async fn real_abtop_once_execs_and_is_bounded() {
    let Ok(path) = which::which("abtop") else {
        eprintln!("abtop not on PATH — skipping real-binary smoke");
        return;
    };

    let result = exec_abtop_once(&path, &[]).await;

    match result {
        ExecResult::Ok(text) => {
            assert!(
                text.contains(ABTOP_HEADER),
                "real abtop --once output must contain the {ABTOP_HEADER:?} header, got: {:?}",
                text.lines().next().unwrap_or(""),
            );
            eprintln!(
                "real abtop OK: first line = {:?}",
                text.lines().next().unwrap_or("")
            );
        }
        ExecResult::Timeout => {
            // Expected when stdout is a non-TTY pipe: abtop's TUI event
            // loop blocks and we reap it. Proves the exec is bounded.
            eprintln!(
                "real abtop --once timed out against a non-TTY pipe (expected); exec was bounded + reaped"
            );
        }
        ExecResult::NonZero { code, stderr } => {
            // abtop refusing a non-TTY (e.g. "Device not configured")
            // is acceptable — it exited cleanly rather than hanging.
            eprintln!("real abtop --once exited {code:?} against non-TTY: {stderr}");
        }
        ExecResult::SpawnFailed(e) => {
            panic!("abtop resolved via `which` but spawn failed — genuine regression: {e}");
        }
    }
}
