//! Interactive-mode provider launch: a REAL, attachable tmux session per task
//! (ccc / D6).
//!
//! A board card launched with `mode = interactive` (the D6 `Run ▾` menu) does
//! NOT go through the headless [`Runner::run_claude`](crate::runner::Runner)
//! pipe-and- capture path. Instead the daemon spawns the provider inside a
//! detached tmux session — exactly the shape `ainb run` uses ([`ainb-core`'s
//! `TmuxSession`], `tmux new-session -d -s <name> -c <workdir> …`) — so the
//! agent is a live, attachable terminal that shows up in `tmux ls` like any
//! other session. The session name is `tmux_hangar-<task_id>` (the task id is a
//! ULID, so it is exact and collision-safe), recorded on the task row the
//! moment the session is created ([`crate::run_loop`]) so the attach-from-card
//! affordance can surface a copyable `tmux attach -t <name>` mid-run.
//!
//! # Completion detection
//!
//! The pane runs a generated wrapper that execs the provider under `env -i`
//! (deny-by-default env, identical to the headless allowlist via
//! [`crate::runner::compose_child_env`]) and writes the provider's exit code to
//! a sibling file before the pane closes. [`TmuxRun::wait`] polls
//! [`tmux_session_exists`] until the session is reaped, then maps the recorded
//! exit code onto the same [`RunOutcome`] the headless runner returns — so the
//! daemon's finalize seam (`running -> done | failed`) is byte-identical for
//! both modes. A blown deadline kills the session by its exact name and returns
//! [`FailureReason::Timeout`].
//!
//! [ainb-core's `TmuxSession`]: the host's `crates/ainb-core/src/tmux/session.rs`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ainb_fleet_core::fleet::send::tmux_session_exists;
use ainb_hangar_store::service::fail::FailureReason;
use tokio::process::Command;

use crate::runner::{RunOutcome, RunnerResult};

/// The tmux width the interactive session is created at (`-x`). Wider than the
/// host's 80-col default so an attached agent has room; the user can resize on
/// attach.
const SESSION_WIDTH: &str = "200";
/// The tmux height the interactive session is created at (`-y`).
const SESSION_HEIGHT: &str = "50";
/// The file (under the task's `logs` dir) the wrapper writes the provider's
/// exit code into, read back by [`TmuxRun::wait`] once the session is reaped.
const EXIT_FILE: &str = "interactive.exit";
/// The generated pane wrapper script (under the task's `logs` dir).
const WRAPPER_FILE: &str = "interactive-run.sh";
/// The POSIX `sysexits.h` `EX_TEMPFAIL` — a provider signalling a transient,
/// retryable runtime failure (same contract as the headless runner).
const EX_TEMPFAIL: i32 = 75;
/// How often [`TmuxRun::wait`] polls for session reaping.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// The exact, collision-safe tmux session name for a task's interactive run.
///
/// `tmux_hangar-<task_id>`: the `tmux_` prefix matches the host's
/// session-naming convention (so it is discoverable alongside `ainb run`
/// sessions), and the task id is a ULID — globally unique and already tmux-safe
/// (Crockford base32, no characters tmux would reject) — so the name never
/// collides.
#[must_use]
pub fn session_name_for(task_id: &str) -> String {
    format!("tmux_hangar-{task_id}")
}

/// Kill a tmux session by its EXACT name (never a wildcard / kill-server).
///
/// The daemon's shutdown reap (a54, [`crate::run_loop`]) calls this for every
/// in-flight interactive session so a detached pane never outlives the daemon.
/// Best-effort: a session already gone yields a non-zero status that is ignored
/// (killing an absent session is a harmless no-op).
pub(crate) async fn kill_session(session_name: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session_name]).status().await;
}

/// A spawned interactive tmux session awaiting completion.
pub struct TmuxRun {
    session_name: String,
    exit_file: PathBuf,
    max_runtime: Duration,
}

/// Spawn `program` (with `argv`) inside a detached tmux session named
/// `session_name`, running in `workdir` with the deny-by-default `child_env`,
/// and return a [`TmuxRun`] to await its completion.
///
/// The pane runs a generated wrapper (written under `logs`) that execs the
/// provider under `env -i` and records its exit code, so completion is detected
/// by session reaping + the recorded code rather than a captured pipe.
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the wrapper cannot be written or `tmux
/// new-session` fails to create the session.
pub async fn spawn(
    program: &Path,
    workdir: &Path,
    argv: &[String],
    child_env: &[(String, String)],
    session_name: &str,
    logs: &Path,
    max_runtime: Duration,
) -> std::io::Result<TmuxRun> {
    let exit_file = logs.join(EXIT_FILE);
    // A stale exit file from a prior attempt would be mis-read as this run's
    // outcome — clear it before spawning.
    let _ = std::fs::remove_file(&exit_file);

    // The wrapper bakes the deny-by-default child env (which can carry a codex
    // agent's `agent_env` and, via the dispatch seam, keychain-resident API keys)
    // into a script on disk. The headless path passes that env in-process and
    // never persists it, so this file MUST be owner-only (0o700) — created with
    // restrictive perms up front (not chmod-after-write) so there is no window
    // where another user could read the secrets it embeds.
    let wrapper = logs.join(WRAPPER_FILE);
    write_owner_only_executable(
        &wrapper,
        &wrapper_script(program, argv, child_env, &exit_file),
    )?;

    let workdir_str = workdir
        .to_str()
        .ok_or_else(|| std::io::Error::other("workdir path is not valid UTF-8"))?;
    let wrapper_str = wrapper
        .to_str()
        .ok_or_else(|| std::io::Error::other("wrapper path is not valid UTF-8"))?;

    // tmux runs a single trailing shell-command argument through `/bin/sh -c`,
    // which word-splits it — so a wrapper path containing a space (a `$HOME` with
    // a space) would break or mis-exec. Pass an explicit, single-quoted `exec`
    // command so the path is handed to the pane shell literally.
    let pane_command = format!("exec {}", sh_quote(wrapper_str));

    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session_name,
            "-c",
            workdir_str,
            "-x",
            SESSION_WIDTH,
            "-y",
            SESSION_HEIGHT,
            &pane_command,
        ])
        .status()
        .await?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "tmux new-session failed for {session_name}"
        )));
    }
    tracing::info!(session = session_name, "interactive tmux session spawned");
    Ok(TmuxRun {
        session_name: session_name.to_string(),
        exit_file,
        max_runtime,
    })
}

impl TmuxRun {
    /// Poll until the tmux session is reaped (the provider exited and the pane
    /// closed) or the deadline blows, then map the recorded exit code onto a
    /// [`RunOutcome`] — the same shape the headless runner returns.
    ///
    /// On a blown deadline the session is killed by its exact name and the run
    /// is [`FailureReason::Timeout`]. A session that vanished without a
    /// readable exit code is treated as an agent error (terminal), never a
    /// silent success.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] only on an unexpected IO fault; a non-zero
    /// exit or a timeout is a normal FSM outcome, not an error.
    pub async fn wait(&self) -> std::io::Result<RunOutcome> {
        let deadline = Instant::now() + self.max_runtime;
        loop {
            if !tmux_session_exists(&self.session_name).await {
                return Ok(self.outcome_from_exit_file());
            }
            if Instant::now() >= deadline {
                self.kill_and_confirm_reaped().await;
                tracing::warn!(
                    session = %self.session_name,
                    reason = "timeout",
                    "interactive_run_failed"
                );
                return Ok(RunOutcome::Failed {
                    reason: FailureReason::Timeout,
                    result: interactive_result(None),
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Kill the spawned session by exact name and return a failed
    /// [`RunOutcome`].
    ///
    /// Used by the daemon when it cannot record the session name on the task
    /// row: an interactive run whose attach handle is unrecoverable is not
    /// worth completing (the card could never surface an attach command for
    /// it), so the session is torn down and the task fails with a retryable
    /// reason.
    pub async fn abort(&self, reason: FailureReason) -> RunOutcome {
        self.kill_and_confirm_reaped().await;
        RunOutcome::Failed {
            reason,
            result: interactive_result(None),
        }
    }

    /// Kill the session by its EXACT name (never a wildcard / kill-server) and
    /// poll — bounded — until tmux confirms it is actually gone, so a timeout /
    /// abort never returns while an orphan pane is still alive.
    async fn kill_and_confirm_reaped(&self) {
        let killed = Command::new("tmux")
            .args(["kill-session", "-t", &self.session_name])
            .status()
            .await;
        if let Ok(s) = killed {
            if !s.success() {
                tracing::warn!(session = %self.session_name, "tmux kill-session returned non-zero");
            }
        }
        // A best-effort reap barrier: `kill-session` returns before tmux has fully
        // torn the session down, so confirm it is gone rather than trusting the
        // exit status alone.
        let reap_deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < reap_deadline {
            if !tmux_session_exists(&self.session_name).await {
                return;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        tracing::warn!(session = %self.session_name, "session still present after kill barrier");
    }

    /// Read the wrapper's recorded exit code and classify it, mirroring the
    /// headless runner's outcome mapping (`0` → success, `75` → retryable
    /// runtime offline, any other / missing → terminal agent error).
    fn outcome_from_exit_file(&self) -> RunOutcome {
        let code = std::fs::read_to_string(&self.exit_file)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        let result = interactive_result(code);
        match code {
            Some(0) => RunOutcome::Success(result),
            Some(EX_TEMPFAIL) => {
                tracing::warn!(
                    session = %self.session_name,
                    reason = "runtime_offline",
                    "interactive_run_failed"
                );
                RunOutcome::Failed {
                    reason: FailureReason::RuntimeOffline,
                    result,
                }
            }
            other => {
                tracing::warn!(
                    session = %self.session_name,
                    reason = "agent_error",
                    exit_code = ?other,
                    "interactive_run_failed"
                );
                RunOutcome::Failed {
                    reason: FailureReason::AgentError,
                    result,
                }
            }
        }
    }
}

/// A [`RunnerResult`] for an interactive run: only the exit code is meaningful
/// — there is no captured JSONL stream (the session is a live terminal), so the
/// session id / usage / output tails are absent. The durable handle to the run
/// is the tmux session name, recorded on the task row.
const fn interactive_result(exit_code: Option<i32>) -> RunnerResult {
    RunnerResult {
        exit_code,
        session_id: None,
        usage: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
    }
}

/// Generate the pane wrapper: exec the provider under a deny-by-default `env
/// -i`, then record its exit code so [`TmuxRun::wait`] can classify the run
/// after the session is reaped.
///
/// Every interpolated value (env pairs, program, args, exit-file path) is
/// POSIX-single-quoted, so a path or value containing spaces or shell
/// metacharacters is passed through literally.
fn wrapper_script(
    program: &Path,
    argv: &[String],
    child_env: &[(String, String)],
    exit_file: &Path,
) -> String {
    let env_pairs = child_env
        .iter()
        .map(|(k, v)| sh_quote(&format!("{k}={v}")))
        .collect::<Vec<_>>()
        .join(" ");
    let args = argv.iter().map(|a| sh_quote(a)).collect::<Vec<_>>().join(" ");
    let program = sh_quote(&program.to_string_lossy());
    let exit_file = sh_quote(&exit_file.to_string_lossy());
    // `env -i` clears the ambient environment and sets ONLY the allowlisted
    // child_env (PATH included, so the provider still resolves). The exit code is
    // written AFTER the provider returns and BEFORE this wrapper exits, so it is
    // on disk before tmux reaps the session.
    format!("#!/bin/sh\nenv -i {env_pairs} {program} {args}\nprintf '%s' \"$?\" > {exit_file}\n")
}

/// POSIX-single-quote `s`: wrap in single quotes, escaping any embedded single
/// quote as `'\''`. Safe for interpolation into a `/bin/sh` command line.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Write `contents` to `path` as an OWNER-ONLY executable (`0o700`), created
/// with those permissions from the outset.
///
/// The wrapper embeds the deny-by-default child env (which can carry a codex
/// agent's `agent_env` and, via the dispatch seam, keychain-resident API keys),
/// so it must never be group/other-readable. Creating the file with `0o700` via
/// `OpenOptions::mode` (rather than write-then-chmod) means there is no window
/// where the secrets it embeds are readable by another user.
fn write_owner_only_executable(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o700)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a [`TmuxRun`] whose exit file holds `code` (or no file when
    /// `None`), under a fresh tempdir, so the outcome mapping can be
    /// exercised without tmux.
    fn run_with_exit(dir: &Path, code: Option<&str>) -> TmuxRun {
        let exit_file = dir.join(EXIT_FILE);
        if let Some(c) = code {
            std::fs::write(&exit_file, c).unwrap();
        }
        TmuxRun {
            session_name: "tmux_hangar-test".to_string(),
            exit_file,
            max_runtime: Duration::from_secs(1),
        }
    }

    #[test]
    fn session_name_is_prefixed_and_carries_the_task_id() {
        assert_eq!(session_name_for("01HZTASK"), "tmux_hangar-01HZTASK");
    }

    #[test]
    fn exit_zero_maps_to_success() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            run_with_exit(dir.path(), Some("0")).outcome_from_exit_file(),
            RunOutcome::Success(_)
        ));
    }

    #[test]
    fn tempfail_maps_to_retryable_runtime_offline() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            run_with_exit(dir.path(), Some("75")).outcome_from_exit_file(),
            RunOutcome::Failed {
                reason: FailureReason::RuntimeOffline,
                ..
            }
        ));
    }

    #[test]
    fn other_nonzero_maps_to_terminal_agent_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            run_with_exit(dir.path(), Some("1")).outcome_from_exit_file(),
            RunOutcome::Failed {
                reason: FailureReason::AgentError,
                ..
            }
        ));
    }

    #[test]
    fn missing_exit_file_is_an_agent_error_never_a_silent_success() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_with_exit(dir.path(), None).outcome_from_exit_file();
        assert!(matches!(
            outcome,
            RunOutcome::Failed {
                reason: FailureReason::AgentError,
                ..
            }
        ));
        assert_eq!(outcome.result().exit_code, None, "no code recovered");
    }

    /// A value carrying a single quote and spaces is passed through the wrapper
    /// literally (POSIX single-quote escaping), never breaking out of the
    /// command.
    #[test]
    fn wrapper_single_quotes_env_and_args_safely() {
        let script = wrapper_script(
            Path::new("/bin/agent"),
            &["--flag".to_string(), "a b'c".to_string()],
            &[("K".to_string(), "v'; rm -rf /".to_string())],
            Path::new("/tmp/x.exit"),
        );
        // The injection attempt is neutralised: the `'; rm -rf /` lands inside a
        // single-quoted token, never as a bare shell command.
        assert!(
            script.contains(r"'K=v'\''; rm -rf /'"),
            "env value escaped: {script}"
        );
        assert!(script.contains(r"'a b'\''c'"), "arg escaped: {script}");
        assert!(
            script.contains("env -i "),
            "provider execs under env -i: {script}"
        );
        assert!(
            script.contains(r#"printf '%s' "$?" > '/tmp/x.exit'"#),
            "exit code is recorded after the provider returns: {script}"
        );
    }
}
