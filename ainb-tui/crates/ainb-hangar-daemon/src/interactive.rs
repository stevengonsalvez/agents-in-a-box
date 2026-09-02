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
    // `=name` is exact. A bare `-t` resolves exact, then prefix, so a reap of a
    // dead "ainb-run-abc" could kill a live "ainb-run-abc2".
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &format!("={session_name}")])
        .status()
        .await;
}

/// A spawned interactive tmux session awaiting completion.
pub struct TmuxRun {
    session_name: String,
    exit_file: PathBuf,
    /// The generated pane wrapper. Held so the run can UNLINK it at teardown:
    /// the script embeds the plaintext child env (a codex agent's `agent_env`,
    /// keychain-resident API keys), and before parity #30 it lingered on disk
    /// forever at 0700 (see [`TmuxRun::purge_wrapper`]).
    wrapper: PathBuf,
    max_runtime: Duration,
}

/// What [`pre_trust_claude_workdir`] did, so the caller can log it and a test
/// can pin the no-HOME and unreadable-file branches without touching a real
/// config.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PreTrust {
    /// `projects[<workdir>]` now carries the trust keys.
    Written,
    /// The child env carries no `HOME`; nothing to write and nowhere to write.
    NoHome,
    /// The config exists but could not be read or parsed as an object; it was
    /// left byte-identical (never overwritten with a stub).
    LeftAlone(String),
    /// The merged config could not be written.
    WriteFailed(String),
}

/// Serialises every trust merge in this daemon: two interactive launches on one
/// HOME (the normal fleet case) used to race the same read-modify-write and one
/// lost its entry. Live `claude` processes rewrite the file on their own
/// schedule too; that window is narrowed to one read+rename under this lock.
static CLAUDE_CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Pre-trust `workdir` in the Claude config the child will read, so an
/// interactive `claude` launched in a freshly provisioned worktree does not park
/// forever at the "Is this a project you trust?" dialog (no human is at the pane
/// when the daemon spawns it; the run then dies on the deadline as `timeout`).
///
/// Merges `projects[<workdir>] = { hasTrustDialogAccepted, hasTrustDialogHooksAccepted }`
/// into `<HOME>/.claude.json`, where `HOME` is the one in `child_env` (the
/// deny-by-default env the child actually inherits). The write is scoped to
/// that ONE project key; the machine-wide bypass-permissions acceptance is
/// passed per launch as `--settings` instead (see `runner::claude_spec`), so
/// nothing here widens trust beyond the worktree. Every other key survives, a
/// missing file is created, and an unreadable or malformed one is left alone
/// (the launch then surfaces the dialog exactly as before). The temp file is
/// unique per call and takes the original file's mode (a fresh file gets 0600,
/// the mode Claude Code writes), so an atomic rename never widens permissions.
///
/// Note: the host's `ainb run` path (`worktree_manager::add_claude_trust`)
/// targets `~/.claude/claude.json`, a path Claude Code does not read; this is
/// the writer that hits the real file.
pub(crate) fn pre_trust_claude_workdir(child_env: &[(String, String)], workdir: &Path) -> PreTrust {
    let Some(home) = child_env.iter().find(|(k, _)| k == "HOME").map(|(_, v)| PathBuf::from(v))
    else {
        return PreTrust::NoHome;
    };
    let path = home.join(".claude.json");
    let _guard = CLAUDE_CONFIG_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    let (mut config, mode): (serde_json::Value, u32) = match std::fs::read(&path) {
        Ok(raw) => match serde_json::from_slice::<serde_json::Value>(&raw) {
            Ok(v) if v.is_object() => (v, file_mode(&path).unwrap_or(0o600)),
            Ok(_) => return PreTrust::LeftAlone("not a JSON object".to_string()),
            Err(e) => return PreTrust::LeftAlone(format!("unparseable: {e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (serde_json::json!({}), 0o600),
        Err(e) => return PreTrust::LeftAlone(format!("unreadable: {e}")),
    };
    let Some(root) = config.as_object_mut() else {
        return PreTrust::LeftAlone("not a JSON object".to_string());
    };
    let projects = root.entry("projects").or_insert_with(|| serde_json::json!({}));
    if !projects.is_object() {
        *projects = serde_json::json!({});
    }
    let Some(projects) = projects.as_object_mut() else {
        return PreTrust::LeftAlone("projects is not an object".to_string());
    };
    let entry = projects
        .entry(workdir.to_string_lossy().to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        // A hand-edited or corrupted project entry: replace it rather than
        // silently keeping a value the trust keys cannot be merged into.
        *entry = serde_json::json!({});
    }
    let Some(obj) = entry.as_object_mut() else {
        return PreTrust::LeftAlone("project entry is not an object".to_string());
    };
    obj.insert(
        "hasTrustDialogAccepted".into(),
        serde_json::Value::Bool(true),
    );
    obj.insert(
        "hasTrustDialogHooksAccepted".into(),
        serde_json::Value::Bool(true),
    );

    let bytes = match serde_json::to_vec_pretty(&config) {
        Ok(b) => b,
        Err(e) => return PreTrust::WriteFailed(format!("serialize: {e}")),
    };
    match write_atomic_with_mode(&path, &bytes, mode) {
        Ok(()) => PreTrust::Written,
        Err(e) => PreTrust::WriteFailed(e.to_string()),
    }
}

/// The unix mode bits of `path`, or `None` when it cannot be read.
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o777)
}

/// Write `bytes` to a unique sibling temp file created with `mode`, then rename
/// it over `path`. The temp name carries the pid and a counter so two writers
/// in one process never share it, and a stale temp from a crashed writer is
/// never picked up.
fn write_atomic_with_mode(path: &Path, bytes: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.hangar-{}-{seq}.tmp", std::process::id()));
    let result = (|| {
        let mut f =
            std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
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
        wrapper,
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
                // `outcome_from_exit_file` IS the finalize seam — it purges the
                // wrapper (parity #30).
                return Ok(self.outcome_from_exit_file());
            }
            if Instant::now() >= deadline {
                self.kill_and_confirm_reaped().await;
                self.purge_wrapper();
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
        self.purge_wrapper();
        RunOutcome::Failed {
            reason,
            result: interactive_result(None),
        }
    }

    /// Best-effort UNLINK of the pane wrapper, at the FINALIZE seam only.
    ///
    /// The wrapper bakes the plaintext child env into a file on disk. The
    /// headless path passes that env in-process and never persists it, so the
    /// interactive path must not leave a durable copy behind once the run is
    /// over (parity #30 — the acceptance sweep greps the task's logs dir).
    ///
    /// Timing matters: this runs AFTER teardown is confirmed, never mid-run,
    /// because `/bin/sh` re-reads a running script and unlinking it early would
    /// break a live session. Failure is ignored — a run's outcome must not hinge
    /// on a cleanup unlink.
    fn purge_wrapper(&self) {
        let _ = std::fs::remove_file(&self.wrapper);
    }

    /// Kill the session by its EXACT name (never a wildcard / kill-server) and
    /// poll — bounded — until tmux confirms it is actually gone, so a timeout /
    /// abort never returns while an orphan pane is still alive.
    async fn kill_and_confirm_reaped(&self) {
        let killed = Command::new("tmux")
            .args(["kill-session", "-t", &format!("={}", self.session_name)])
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
    /// The session is gone by the time this runs, so it is also the FINALIZE
    /// seam that unlinks the secret-bearing pane wrapper.
    fn outcome_from_exit_file(&self) -> RunOutcome {
        self.purge_wrapper();
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
            wrapper: dir.join(WRAPPER_FILE),
            max_runtime: Duration::from_secs(1),
        }
    }

    /// Parity #30: the pane wrapper bakes the plaintext child env onto disk, so
    /// once the run reaches a terminal outcome (the session is gone and the exit
    /// code is being classified) the file MUST be unlinked. Before this, it
    /// lingered forever at 0700 and every `sk-…` an agent ran with stayed
    /// greppable under the task's logs dir.
    #[test]
    fn terminal_outcome_unlinks_the_secret_bearing_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let run = run_with_exit(dir.path(), Some("0"));
        let wrapper = dir.path().join(WRAPPER_FILE);
        std::fs::write(
            &wrapper,
            "#!/bin/sh\nenv -i SECRET_TOKEN='sk-live-DEADBEEF01' x\n",
        )
        .unwrap();
        assert!(wrapper.exists(), "precondition: the wrapper is on disk");

        let _ = run.outcome_from_exit_file();

        assert!(
            !wrapper.exists(),
            "the wrapper embeds the plaintext child env and must not survive teardown"
        );
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

    /// The trust merge targets the HOME the CHILD sees (from the deny-by-default
    /// env, not the daemon's), creates the file when absent, preserves every
    /// unrelated key and project on a second call, and writes ONLY the two
    /// per-project trust keys (never a machine-wide acceptance).
    #[test]
    fn pre_trust_merges_the_workdir_into_the_child_homes_claude_json() {
        let home = tempfile::tempdir().unwrap();
        let env = vec![(
            "HOME".to_string(),
            home.path().to_string_lossy().to_string(),
        )];
        let wd = Path::new("/srv/worktrees/task-1");

        assert_eq!(pre_trust_claude_workdir(&env, wd), PreTrust::Written);
        let path = home.path().join(".claude.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            v["projects"]["/srv/worktrees/task-1"]["hasTrustDialogAccepted"],
            true
        );
        assert_eq!(
            v["projects"]["/srv/worktrees/task-1"]["hasTrustDialogHooksAccepted"],
            true
        );
        assert!(
            v.get("bypassPermissionsModeAccepted").is_none(),
            "no machine-wide acceptance is written; that rides the launch argv"
        );
        assert_eq!(file_mode(&path), Some(0o600), "a fresh config is private");

        // Seed unrelated state and a second project, then re-trust: nothing lost.
        let seeded = serde_json::json!({
            "oauthAccount": {"emailAddress": "x@y"},
            "projects": {
                "/other": {"allowedTools": ["Bash"], "hasTrustDialogAccepted": false},
                "/srv/worktrees/task-1": {"allowedTools": ["Read"]}
            }
        });
        std::fs::write(&path, serde_json::to_string(&seeded).unwrap()).unwrap();
        assert_eq!(pre_trust_claude_workdir(&env, wd), PreTrust::Written);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            v["oauthAccount"]["emailAddress"], "x@y",
            "unrelated keys survive"
        );
        assert_eq!(
            v["projects"]["/other"]["hasTrustDialogAccepted"], false,
            "other projects untouched"
        );
        assert_eq!(
            v["projects"]["/srv/worktrees/task-1"]["allowedTools"][0], "Read",
            "existing project keys kept"
        );
        assert_eq!(
            v["projects"]["/srv/worktrees/task-1"]["hasTrustDialogAccepted"],
            true
        );
    }

    /// The rename never widens the file: a 0600 config stays 0600, and the
    /// caller's temp file is gone afterwards.
    #[test]
    fn pre_trust_preserves_the_configs_mode_and_leaves_no_temp() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let env = vec![(
            "HOME".to_string(),
            home.path().to_string_lossy().to_string(),
        )];
        assert_eq!(
            pre_trust_claude_workdir(&env, Path::new("/w/x")),
            PreTrust::Written
        );
        assert_eq!(file_mode(&path), Some(0o600));
        let leftovers: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("hangar-"))
            .collect();
        assert!(leftovers.is_empty(), "temp file cleaned up: {leftovers:?}");
    }

    /// No HOME in the child env means nothing to write: the launch proceeds and
    /// the dialog surfaces as before rather than a stray file appearing.
    #[test]
    fn pre_trust_without_home_is_a_no_op() {
        assert_eq!(
            pre_trust_claude_workdir(&[], Path::new("/srv/x")),
            PreTrust::NoHome
        );
    }

    /// An unparseable or non-object config is left byte-identical: the old
    /// `unwrap_or({})` fallback would have replaced the operator's whole config
    /// with a three-key stub on a transient read or parse failure.
    #[test]
    fn pre_trust_leaves_a_broken_config_untouched() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude.json");
        let env = vec![(
            "HOME".to_string(),
            home.path().to_string_lossy().to_string(),
        )];
        for broken in ["{not json", "[1,2,3]"] {
            std::fs::write(&path, broken).unwrap();
            assert!(matches!(
                pre_trust_claude_workdir(&env, Path::new("/w/x")),
                PreTrust::LeftAlone(_)
            ));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                broken,
                "byte-identical"
            );
        }
    }

    /// A corrupted (non-object) project entry is replaced so the trust keys land
    /// instead of being silently dropped while the log claims success.
    #[test]
    fn pre_trust_replaces_a_non_object_project_entry() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".claude.json");
        std::fs::write(&path, r#"{"projects":{"/w/x":"garbage"}}"#).unwrap();
        let env = vec![(
            "HOME".to_string(),
            home.path().to_string_lossy().to_string(),
        )];
        assert_eq!(
            pre_trust_claude_workdir(&env, Path::new("/w/x")),
            PreTrust::Written
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["projects"]["/w/x"]["hasTrustDialogAccepted"], true);
    }
}
