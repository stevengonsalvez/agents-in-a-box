//! Run the external `abtop --once [args]` binary and capture its
//! stdout as text.
//!
//! abtop has no machine-readable mode — `--once` prints a
//! human-readable snapshot of running AI coding agents and exits. We
//! forward that text verbatim; there's nothing to parse.
//!
//! The exec runs under the plugin's `spawn_subprocess` capability. Two
//! guarantees this module owns:
//!
//! 1. **No shell.** We spawn via `tokio::process::Command::new(path)`
//!    with `.arg("--once")` plus any forwarded args as an argv array —
//!    not `sh -c`. Shell metacharacters in forwarded args are passed
//!    verbatim to abtop, never interpreted by a shell, so command
//!    injection is structurally impossible.
//!
//! 2. **Bounded + reaped.** The exec is bounded by [`EXEC_TIMEOUT`].
//!    abtop is a crossterm/ratatui alternate-screen TUI; `--once` only
//!    snapshots cleanly when stdout is a real interactive terminal.
//!    Against a non-TTY pipe (which is exactly how we capture it) it
//!    can block in its event loop, so the timeout is load-bearing, not
//!    defensive. Tokio does **not** kill a child when the `output()`
//!    future is dropped unless `kill_on_drop` is set (default `false` —
//!    the child would otherwise keep running and leak on every
//!    timeout). We set [`Command::kill_on_drop(true)`] so the `timeout`
//!    cancellation drops the future and tokio kills the child.

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

/// Maximum wall-clock budget for a single `abtop --once` exec. abtop's
/// snapshot is near-instant on a real TTY; the generous ceiling exists
/// to reap a hung event loop when stdout is a pipe rather than a
/// terminal.
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(5);

/// Soft cap on stdout bytes we hold for a captured snapshot. abtop's
/// snapshot is small; this just stops a misbehaving binary from
/// spilling unbounded output into memory before we forward it.
const STDOUT_MAX_BYTES: usize = 256 * 1024;

/// Outcome of one [`exec_abtop_once`] call. The render + CLI paths
/// match on these exhaustively — every variant maps to a distinct
/// user-visible state (text / timeout banner / error banner /
/// spawn failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecResult {
    /// abtop exited 0; carries its captured stdout text.
    Ok(String),
    /// `abtop --once` ran past [`EXEC_TIMEOUT`] and was reaped.
    Timeout,
    /// abtop exited non-zero. Carries the exit code and trimmed stderr.
    NonZero {
        /// Exit code, or `None` if the process was killed by a signal.
        code: Option<i32>,
        /// Trimmed stderr — surfaced verbatim in the error banner.
        stderr: String,
    },
    /// Spawn / I/O error before we could collect stdout (e.g.
    /// permission denied on the abtop binary). Carries the underlying
    /// error message.
    SpawnFailed(String),
}

/// Truncate `s` to at most [`STDOUT_MAX_BYTES`] bytes at a UTF-8 char
/// boundary so a runaway abtop can't bloat the captured snapshot. Only
/// appends `…` when truncation actually fired.
fn clamp_stdout(s: &str) -> String {
    if s.len() <= STDOUT_MAX_BYTES {
        return s.to_string();
    }
    let mut end = STDOUT_MAX_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

/// Run `abtop --once [args]` against the binary at `path` and capture
/// its stdout as text.
///
/// `path` must come from [`crate::detect::DetectResult::Ready.path`] —
/// we don't re-resolve via `which` here so a manifest-renamed abtop
/// can't slip past the detect gate.
///
/// `args` are forwarded after `--once` (e.g. `--theme`, `--demo`).
/// They're passed as an argv array, so no shell interprets them.
///
/// Never panics. Bounded by [`EXEC_TIMEOUT`]; a hung TUI event loop is
/// reaped via `kill_on_drop`.
pub async fn exec_abtop_once(path: &Path, args: &[String]) -> ExecResult {
    let exec = Command::new(path)
        .arg("--once")
        .args(args)
        .stdin(std::process::Stdio::null())
        // Kill the child if the `timeout` below cancels (drops) this
        // future — tokio's default leaves it running, leaking an abtop
        // process on every timeout.
        .kill_on_drop(true)
        .output();

    let output = match timeout(EXEC_TIMEOUT, exec).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "abtop --once spawn failed");
            return ExecResult::SpawnFailed(e.to_string());
        }
        Err(_) => {
            tracing::warn!(
                timeout_secs = EXEC_TIMEOUT.as_secs(),
                "abtop --once timed out",
            );
            return ExecResult::Timeout;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let code = output.status.code();
        tracing::debug!(code, %stderr, "abtop --once non-zero exit");
        return ExecResult::NonZero { code, stderr };
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    ExecResult::Ok(clamp_stdout(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_stdout_passes_short_input_unchanged() {
        assert_eq!(clamp_stdout("abtop — 3 sessions"), "abtop — 3 sessions");
    }

    #[test]
    fn clamp_stdout_truncates_at_char_boundary_with_ellipsis() {
        let huge = "x".repeat(STDOUT_MAX_BYTES + 100);
        let clamped = clamp_stdout(&huge);
        assert!(clamped.ends_with('…'));
        assert!(clamped.len() <= STDOUT_MAX_BYTES + '…'.len_utf8());
    }

    #[test]
    fn clamp_stdout_below_cap_multibyte_not_truncated() {
        // 3-byte glyph repeated to land below the cap; the boundary
        // walk on STDOUT_MAX_BYTES must not falsely append `…`.
        let s: String = "—".repeat(STDOUT_MAX_BYTES / 6);
        assert!(s.len() < STDOUT_MAX_BYTES);
        let clamped = clamp_stdout(&s);
        assert_eq!(clamped, s);
        assert!(!clamped.ends_with('…'));
    }
}
