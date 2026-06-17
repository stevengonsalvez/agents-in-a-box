//! Agent CLI subprocess execution — the `claude` provider (P1.7).
//!
//! [`Runner::run_claude`] spawns the `claude` binary inside a task's isolated
//! [`ExecEnv`], with a **deny-by-default** env (only the 12-var allowlist passes
//! through — see [`ENV_ALLOWLIST`]), streams its JSONL stdout line-by-line to
//! `{logs}/claude.jsonl`, pins the first `session_id` it sees, and enforces a
//! hard runtime deadline (kill on timeout). Mirrors Multica's `daemon.go`
//! session-pinning + allowlisted-exec pattern.
//!
//! # Provider abstraction
//!
//! Only `claude` ships in P1. The orchestration here (env build, JSONL tee,
//! session pin, timeout) is provider-agnostic; P5 adds codex/copilot as new
//! `Provider` impls rather than edits to [`Runner`] (see the [`Provider`] trait).
//!
//! # Outcome classification
//!
//! The runner does **not** itself touch the database. It returns a
//! [`RunOutcome`] the daemon's claim loop maps onto the FSM:
//! - clean exit (code 0)        → [`RunOutcome::Success`] → daemon `CompleteTask`,
//! - non-zero exit              → [`RunOutcome::Failed`] with
//!   [`FailureReason::AgentError`] (the agent itself errored / gave up),
//! - deadline exceeded → kill   → [`RunOutcome::Failed`] with
//!   [`FailureReason::Timeout`].

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use ainb_hangar_store::service::fail::FailureReason;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::execenv::ExecEnv;

/// The env vars a provider subprocess is allowed to inherit.
///
/// Deny-by-default: the child receives *only* these 12 vars (when present in the
/// caller-supplied source env), never the daemon's full environment, so a leaked
/// `SECRET_KEY`/token in the daemon's process never reaches an agent subprocess
/// (build-plan §4 security decision). Order is irrelevant; membership is what
/// the runner filters on.
pub const ENV_ALLOWLIST: &[&str] = &[
    "HOME",
    "PATH",
    "LANG",
    "LC_ALL",
    "TERM",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    // P6.4: provider-home pointers the daemon sets deliberately at dispatch so a
    // home-style provider (claude/codex/cursor) reads its materialised skills
    // from the task-isolated home rather than the operator's real `$HOME`. These
    // are daemon-controlled config (set from the materialise report), never
    // inherited ambient values, so allowlisting them leaks nothing.
    "CLAUDE_HOME",
    "CODEX_HOME",
    "CURSOR_HOME",
];

/// The provider-log file written under [`ExecEnv::logs`].
const LOG_FILE: &str = "claude.jsonl";

/// Static configuration for a [`Runner`].
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Absolute path to the `claude` binary (or a test stand-in script).
    pub claude_path: PathBuf,
    /// Hard wall-clock deadline; the subprocess is killed past it
    /// ([`FailureReason::Timeout`]). Multica default: 2.5h.
    pub max_runtime: Duration,
    /// How many trailing stdout/stderr lines to retain in [`RunnerResult`] for
    /// the audit/UI tail.
    pub tail_lines: usize,
}

/// The captured result of one provider run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerResult {
    /// Process exit code, or `None` if the process was killed by signal/timeout.
    pub exit_code: Option<i32>,
    /// The first `session_id` parsed from a `{"type":"system",...}` JSONL line,
    /// or `None` if the provider emitted none.
    pub session_id: Option<String>,
    /// Trailing stdout lines (up to [`RunnerConfig::tail_lines`]), newline-joined.
    pub stdout_tail: String,
    /// Trailing stderr lines (up to [`RunnerConfig::tail_lines`]), newline-joined.
    pub stderr_tail: String,
}

/// How a provider run finished, ready for the daemon to map onto the task FSM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The provider exited cleanly (code 0). The daemon should `CompleteTask`.
    Success(RunnerResult),
    /// The provider failed; `reason` is the FSM failure reason to record.
    Failed {
        /// Why the run failed.
        reason: FailureReason,
        /// The captured result (exit code, session id, output tails).
        result: RunnerResult,
    },
}

impl RunOutcome {
    /// Borrow the captured [`RunnerResult`] regardless of success/failure.
    #[must_use]
    pub const fn result(&self) -> &RunnerResult {
        match self {
            Self::Success(r) | Self::Failed { result: r, .. } => r,
        }
    }
}

/// A `system`-type JSONL line, the only shape the runner needs to decode (to pin
/// `session_id`). Other line types are streamed to the log verbatim and ignored
/// here.
#[derive(Debug, Deserialize)]
struct SystemLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    session_id: Option<String>,
}

/// A provider that can be exec'd as an agent CLI subprocess.
///
/// P1 ships only [`Runner`] (claude). The trait exists so P5 providers
/// (codex/copilot) land as new impls without touching the claim loop. Kept
/// minimal — the daemon only needs to drive one run to an outcome.
pub trait Provider {
    /// The provider's wire name (`"claude"`, …).
    fn name(&self) -> &'static str;
}

/// Executes the `claude` provider as a subprocess.
#[derive(Debug, Clone)]
pub struct Runner {
    cfg: RunnerConfig,
}

impl Provider for Runner {
    fn name(&self) -> &'static str {
        "claude"
    }
}

impl Runner {
    /// Construct a runner from its static [`RunnerConfig`].
    #[must_use]
    pub const fn new(cfg: RunnerConfig) -> Self {
        Self { cfg }
    }

    /// Spawn `claude` in `env.workdir`, stream its JSONL stdout to
    /// `{env.logs}/claude.jsonl`, pin the first `session_id`, and enforce the
    /// configured deadline.
    ///
    /// `source_env` supplies the candidate environment; only the keys in
    /// [`ENV_ALLOWLIST`] are passed to the child (deny-by-default). The daemon
    /// typically passes its own [`std::env::vars`], but tests pass a tight set.
    ///
    /// Returns a [`RunOutcome`] — never an error for a non-zero exit or a
    /// timeout (those are FSM outcomes, not runner failures); only genuine I/O
    /// faults (spawn failure, log-write failure) surface as [`std::io::Error`].
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] if the binary cannot be spawned, the log
    /// file cannot be opened/written, or stdout cannot be read.
    pub async fn run_claude<I>(&self, env: &ExecEnv, source_env: I) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let allow: std::collections::HashSet<&str> = ENV_ALLOWLIST.iter().copied().collect();
        let allowed: Vec<(String, String)> =
            source_env.into_iter().filter(|(k, _)| allow.contains(k.as_str())).collect();

        let log_path = env.logs.join(LOG_FILE);
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&log_path)?;

        let mut child = Command::new(&self.cfg.claude_path)
            .current_dir(&env.workdir)
            .env_clear()
            .envs(allowed)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Run in its own process group so a timeout kill reaches the whole
            // tree, not just the immediate child. A provider that shells out
            // (`sh -c "… sleep …"`) leaves a grandchild holding the inherited
            // stdout pipe; killing only the parent would leave the reader
            // blocked on EOF until the grandchild exits.
            .process_group(0)
            .spawn()?;

        // The child is its own process-group leader (pgid == its pid), captured
        // before we move `child` into the wait so a timeout can `killpg` the
        // whole group.
        let pgid = child.id().map(i32::try_from).and_then(Result::ok);

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("child stdout not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("child stderr not captured"))?;

        // Tee stdout: append every line to the JSONL log, pin the first
        // session_id, and keep a bounded tail. The stderr reader only keeps a
        // tail. Both run concurrently with the wait so a chatty provider can
        // never deadlock on a full pipe buffer.
        let tail_lines = self.cfg.tail_lines;
        let stdout_task =
            tokio::spawn(async move { stream_stdout(stdout, log_file, tail_lines).await });
        let stderr_task = tokio::spawn(async move { tail_reader(stderr, tail_lines).await });

        let timed_out = match tokio::time::timeout(self.cfg.max_runtime, child.wait()).await {
            Ok(status) => {
                status?;
                false
            }
            Err(_elapsed) => {
                // Deadline blown: SIGKILL the whole process group so any
                // grandchild (e.g. a `sleep` under `sh -c`) dies too and
                // releases the stdout pipe, then reap the immediate child so no
                // zombie outlives the run.
                kill_group(pgid);
                let _ = child.start_kill();
                let _ = child.wait().await;
                true
            }
        };

        let (session_id, stdout_tail) = stdout_task
            .await
            .map_err(|e| std::io::Error::other(format!("stdout task join: {e}")))??;
        let stderr_tail = stderr_task
            .await
            .map_err(|e| std::io::Error::other(format!("stderr task join: {e}")))??;

        // `child.wait()` already completed above, so the status is reflected by
        // whether we timed out; re-derive the exit code from the killed/clean
        // path. On the clean path we re-query via `try_wait` which now returns
        // the cached status.
        let exit_code = if timed_out {
            None
        } else {
            child.try_wait()?.and_then(|s| s.code())
        };

        let result = RunnerResult {
            exit_code,
            session_id,
            stdout_tail,
            stderr_tail,
        };

        let outcome = if timed_out {
            tracing::warn!(reason = "timeout", "runner_claude_failed");
            RunOutcome::Failed {
                reason: FailureReason::Timeout,
                result,
            }
        } else if exit_code == Some(0) {
            RunOutcome::Success(result)
        } else {
            tracing::warn!(reason = "agent_error", exit_code = ?exit_code, "runner_claude_failed");
            RunOutcome::Failed {
                reason: FailureReason::AgentError,
                result,
            }
        };
        Ok(outcome)
    }
}

/// Read the child's stdout line-by-line, appending each line to `log_file`,
/// pinning the first `system` line's `session_id`, and retaining a bounded tail.
///
/// Returns `(first_session_id, stdout_tail)`.
async fn stream_stdout(
    stdout: tokio::process::ChildStdout,
    mut log_file: std::fs::File,
    tail_lines: usize,
) -> std::io::Result<(Option<String>, String)> {
    use std::io::Write;

    let mut reader = BufReader::new(stdout).lines();
    let mut session_id: Option<String> = None;
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();

    while let Some(line) = reader.next_line().await? {
        writeln!(log_file, "{line}")?;
        if session_id.is_none() {
            if let Ok(parsed) = serde_json::from_str::<SystemLine>(&line) {
                if parsed.kind == "system" {
                    if let Some(sid) = parsed.session_id {
                        session_id = Some(sid);
                    }
                }
            }
        }
        push_tail(&mut tail, line, tail_lines);
    }
    log_file.flush()?;
    Ok((session_id, join_tail(tail)))
}

/// Read a child pipe to EOF, retaining only a bounded trailing tail.
async fn tail_reader<R>(pipe: R, tail_lines: usize) -> std::io::Result<String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(pipe).lines();
    let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    while let Some(line) = reader.next_line().await? {
        push_tail(&mut tail, line, tail_lines);
    }
    Ok(join_tail(tail))
}

/// Push `line` onto the bounded tail buffer, evicting the oldest if at capacity.
/// A `tail_lines` of 0 means "keep nothing".
fn push_tail(tail: &mut std::collections::VecDeque<String>, line: String, tail_lines: usize) {
    if tail_lines == 0 {
        return;
    }
    if tail.len() == tail_lines {
        tail.pop_front();
    }
    tail.push_back(line);
}

/// Newline-join a tail buffer.
fn join_tail(tail: std::collections::VecDeque<String>) -> String {
    tail.into_iter().collect::<Vec<_>>().join("\n")
}

/// SIGKILL an entire process group by its leader pid.
///
/// The child was spawned with `process_group(0)`, so its pid is also its pgid;
/// `killpg(-pgid)` reaches the provider and every grandchild it spawned. A
/// best-effort send: an `ESRCH` (group already gone) is ignored. `None` pgid
/// means the child never started.
fn kill_group(pgid: Option<i32>) {
    let Some(pid) = pgid else { return };
    let _ = nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
    );
}
