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
//! `claude` shipped in P1. The orchestration here (env build, JSONL tee, session
//! pin, timeout, OS sandbox) is provider-agnostic — captured once in
//! [`Runner::run_provider`] and parameterised by a [`ProviderSpec`] (the wire
//! name, the per-provider log file, and the argv to spawn). e38.16 adds the
//! `codex` exec path ([`Runner::run_codex`]) as a second `ProviderSpec` rather
//! than a fork of the run loop, so a third provider is one more spec.
//!
//! # Outcome classification
//!
//! The runner does **not** itself touch the database. It returns a
//! [`RunOutcome`] the daemon's claim loop maps onto the FSM:
//! - clean exit (code 0)        → [`RunOutcome::Success`] → daemon `CompleteTask`,
//! - exit [`EX_TEMPFAIL`] (75)   → [`RunOutcome::Failed`] with
//!   [`FailureReason::RuntimeOffline`] — a provider's POSIX `sysexits.h`
//!   "temporary failure, retry later" code, the infra/retryable failure the
//!   daemon's retry chain (e38.28) re-dispatches as a child task,
//! - any other non-zero exit    → [`RunOutcome::Failed`] with
//!   [`FailureReason::AgentError`] (the agent itself errored / gave up — terminal,
//!   not retried),
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

/// The POSIX `sysexits.h` `EX_TEMPFAIL` (75): "temporary failure, indicating
/// something that is not really an error … the request can be retried later".
///
/// A provider that detects its runtime/API is transiently unreachable exits with
/// this distinguished code so the daemon classifies the run as
/// [`FailureReason::RuntimeOffline`] (infra, retryable) rather than
/// [`FailureReason::AgentError`] (the agent gave up, terminal). This is the seam
/// that lets a retryable failure flow into the e38.28 retry chain; every OTHER
/// non-zero exit stays `AgentError`.
const EX_TEMPFAIL: i32 = 75;

/// The provider-log file written under [`ExecEnv::logs`] for the `claude`
/// provider.
const CLAUDE_LOG_FILE: &str = "claude.jsonl";
/// The provider-log file written under [`ExecEnv::logs`] for the `codex`
/// provider (e38.16). Each provider streams to its own log so a workspace that
/// runs both backends keeps their JSONL transcripts separate.
const CODEX_LOG_FILE: &str = "codex.jsonl";
/// The codex non-interactive subcommand. The real `codex` CLI runs a headless
/// task as `codex exec …` (the established non-interactive shape — see the
/// `coding-agent` skill); the runner always leads codex's argv with it.
const CODEX_EXEC_SUBCOMMAND: &str = "exec";
/// The codex model flag (`codex exec -m <model> …`).
const CODEX_MODEL_FLAG: &str = "-m";

/// Static configuration for a [`Runner`].
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Absolute path to the `claude` binary (or a test stand-in script).
    pub claude_path: PathBuf,
    /// Absolute path to the `codex` binary (or a test stand-in script). Used by
    /// [`Runner::run_codex`] (e38.16); a daemon that never dispatches a codex
    /// task simply never spawns it.
    pub codex_path: PathBuf,
    /// Hard wall-clock deadline; the subprocess is killed past it
    /// ([`FailureReason::Timeout`]). Multica default: 2.5h.
    pub max_runtime: Duration,
    /// How many trailing stdout/stderr lines to retain in [`RunnerResult`] for
    /// the audit/UI tail.
    pub tail_lines: usize,
    /// e38.23: confine the provider subprocess in an OS-level FS sandbox
    /// (Seatbelt on macOS / Landlock on Linux) so the agent can only read/write
    /// the task's isolated roots. **Default ON** (the override seam); the
    /// existing `claude` provider keeps working confined (it needs only network
    /// and the workdir, both allowed). On a platform with no sandbox primitive
    /// the spawn transparently runs unconfined (the sandbox layer reports
    /// `Enforcement::None`) rather than failing the task.
    pub sandbox: bool,
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

/// Which provider exec path the daemon routes a task to (e38.16).
///
/// Resolved from the task's agent → runtime → `provider` wire name. An
/// unrecognised provider falls back to [`Self::Claude`] so a misconfigured or
/// not-yet-implemented backend still dispatches (rather than stranding the
/// task) — the same default-to-claude convention the rest of the daemon uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// The `claude` provider — [`Runner::run_claude`]. The default exec path: a
    /// task whose provider is unrecognised or unresolvable still dispatches here.
    #[default]
    Claude,
    /// The `codex` provider — [`Runner::run_codex`].
    Codex,
}

impl Backend {
    /// Resolve a provider wire name (`"claude"`, `"codex"`, …) to a backend.
    ///
    /// Matching is case-insensitive. Any name that is not a wired exec path maps
    /// to [`Self::Claude`] (the safe default), mirroring
    /// [`crate::materialise::ProviderSkillLayout::from_provider`]'s catch-all.
    #[must_use]
    pub fn from_provider(provider: &str) -> Self {
        match provider.to_ascii_lowercase().as_str() {
            "codex" => Self::Codex,
            _ => Self::Claude,
        }
    }

    /// The provider's wire name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// The per-agent provider config the runner threads into a provider's argv
/// (e38.16). Sourced from the agent row's migration-0015 config columns.
///
/// `model` and `cli_args` flow onto the provider's command line; the agent's
/// `agent_env` is threaded separately (it goes into the child env, not the
/// argv) via the `extra_env` argument of [`Runner::run_codex_with_env`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderInvocation {
    /// Optional model override (e.g. `gpt-5-codex`); `None` = provider default.
    pub model: Option<String>,
    /// Extra provider CLI arguments appended verbatim after the subcommand
    /// (e.g. `["--full-auto"]`).
    pub cli_args: Vec<String>,
}

/// A provider's per-run identity: its wire name, its log file, and the argv to
/// append after the program (e38.16).
///
/// The orchestration in [`Runner::run_provider`] is identical across providers;
/// only these three differ. A new provider is one more `ProviderSpec` builder
/// (see [`Runner::claude_spec`] / [`Runner::codex_spec`]) rather than a new copy
/// of the run loop.
struct ProviderSpec {
    /// The provider's wire name (`"claude"`, `"codex"`), for logs/tracing.
    name: &'static str,
    /// The provider-log file under [`ExecEnv::logs`].
    log_file: &'static str,
    /// The argv to append after the program path (subcommand + flags + args).
    argv: Vec<String>,
}

/// A provider that can be exec'd as an agent CLI subprocess.
///
/// The trait exists so dispatch can name the active provider without reaching
/// into [`Runner`]'s concrete exec methods. Kept minimal.
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

    /// Build the (tokio) spawn command for `program`, wrapped in the OS-level FS
    /// sandbox when [`RunnerConfig::sandbox`] is on.
    ///
    /// `program` is the provider binary (claude or codex): every provider spawns
    /// through this one wrapper, so the codex exec path gets exactly the same
    /// confinement as claude (e38.16). The confinement policy is derived from the
    /// task's [`ExecEnv`]: writes are confined to the task root
    /// (`workdir`/`output`/`logs` all live under it) + the process temp dir;
    /// reads are confined to the system roots a real agent needs + the task root;
    /// network egress to the model API stays allowed. With the sandbox off, or on
    /// an unsupported platform, the command is the bare provider binary (the env
    /// allowlist + process-group kill still apply).
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] only if a *supported* sandbox primitive is
    /// expected but unavailable, or a sandbox setup IO fault occurs. An
    /// unsupported platform is NOT an error — it degrades to a passthrough.
    fn build_command(&self, program: &std::path::Path, env: &ExecEnv) -> std::io::Result<Command> {
        if !self.cfg.sandbox {
            let cmd = ainb_hangar_sandbox::SandboxedCommand::passthrough(program).into_inner();
            return Ok(Command::from(cmd));
        }

        let policy = ainb_hangar_sandbox::SandboxPolicy::confined_to(env.root());
        let sandboxed = ainb_hangar_sandbox::sandboxed_command(program, &policy)
            .map_err(|e| std::io::Error::other(format!("sandbox setup: {e}")))?;
        if sandboxed.enforcement() == ainb_hangar_sandbox::Enforcement::None {
            tracing::warn!("OS sandbox unavailable on this platform; provider runs unconfined");
        }
        // Convert the std command (with the inline Seatbelt wrapping on macOS /
        // the `pre_exec` Landlock hook on Linux already baked in) into a tokio
        // command. `From` preserves the program, args, and any `pre_exec`
        // closure, so the FS confinement carries over with the command — no
        // external profile file or guard to keep alive.
        Ok(Command::from(sandboxed.into_inner()))
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
        // claude takes no extra argv at v1 (the task is handed to it via its
        // materialised home + workdir, not the command line), so the spec argv is
        // empty and there is no per-agent env beyond the allowlisted source env.
        let spec = Self::claude_spec();
        self.run_provider(
            &self.cfg.claude_path,
            env,
            source_env,
            std::iter::empty(),
            spec,
        )
        .await
    }

    /// Spawn `codex` in `env.workdir` via its non-interactive `exec` subcommand
    /// (e38.16), stream its JSONL stdout to `{env.logs}/codex.jsonl`, pin the
    /// first `session_id`, and enforce the configured deadline.
    ///
    /// `invocation` threads the agent's migration-0015 config onto the codex
    /// argv: `codex exec [-m <model>] [<cli_args>…]`. The child env is the
    /// allowlist-filtered `source_env` (no per-agent env on this overload — use
    /// [`Self::run_codex_with_env`] to layer `agent_env`).
    ///
    /// The spawn goes through the same OS-level FS sandbox as
    /// [`Self::run_claude`] (e38.23), so codex is confined to the task's isolated
    /// roots identically.
    ///
    /// # Errors
    ///
    /// As [`Self::run_claude`].
    pub async fn run_codex<I>(
        &self,
        env: &ExecEnv,
        source_env: I,
        invocation: &ProviderInvocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        self.run_codex_with_env(env, source_env, std::iter::empty(), invocation).await
    }

    /// [`Self::run_codex`], plus a set of per-agent `extra_env` overrides layered
    /// onto the child env *after* the allowlist filter (e38.16).
    ///
    /// `source_env` is the daemon's ambient env, filtered to [`ENV_ALLOWLIST`]
    /// (deny-by-default — a leaked daemon secret never reaches codex). `extra_env`
    /// is the agent's deliberate `agent_env` config: these are operator-set
    /// per-agent values, not ambient secrets, so — like the keychain keys in
    /// [`crate::dispatch::build_task_env`] — they bypass the ambient allowlist and
    /// reach the child verbatim. The secret-leak boundary (the ambient filter)
    /// is unchanged.
    ///
    /// # Errors
    ///
    /// As [`Self::run_claude`].
    pub async fn run_codex_with_env<I, E>(
        &self,
        env: &ExecEnv,
        source_env: I,
        extra_env: E,
        invocation: &ProviderInvocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
        E: IntoIterator<Item = (String, String)>,
    {
        let spec = Self::codex_spec(invocation);
        self.run_provider(&self.cfg.codex_path, env, source_env, extra_env, spec).await
    }

    /// The `claude` provider spec: claude log file, no argv.
    const fn claude_spec() -> ProviderSpec {
        ProviderSpec {
            name: "claude",
            log_file: CLAUDE_LOG_FILE,
            argv: Vec::new(),
        }
    }

    /// The `codex` provider spec: codex log file + the non-interactive argv
    /// `exec [-m <model>] [<cli_args>…]` (e38.16).
    fn codex_spec(invocation: &ProviderInvocation) -> ProviderSpec {
        let mut argv = vec![CODEX_EXEC_SUBCOMMAND.to_string()];
        if let Some(model) = &invocation.model {
            argv.push(CODEX_MODEL_FLAG.to_string());
            argv.push(model.clone());
        }
        argv.extend(invocation.cli_args.iter().cloned());
        ProviderSpec {
            name: "codex",
            log_file: CODEX_LOG_FILE,
            argv,
        }
    }

    /// The provider-agnostic run core shared by every provider (e38.16).
    ///
    /// Spawns `program` (through the OS sandbox) with `spec.argv` in
    /// `env.workdir`, builds the child env from the allowlist-filtered
    /// `source_env` plus the verbatim `extra_env` overrides, tees stdout to
    /// `{env.logs}/{spec.log_file}` while pinning the first `session_id`, and
    /// enforces the deadline — returning the same [`RunOutcome`] shape for any
    /// provider. Only the program, argv, log file, and the env composition differ
    /// per provider; the orchestration is identical.
    async fn run_provider<I, E>(
        &self,
        program: &std::path::Path,
        env: &ExecEnv,
        source_env: I,
        extra_env: E,
        spec: ProviderSpec,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
        E: IntoIterator<Item = (String, String)>,
    {
        let allow: std::collections::HashSet<&str> = ENV_ALLOWLIST.iter().copied().collect();
        // Deny-by-default ambient filter, then layer the agent's explicit env
        // overrides on top (so a per-agent value wins over an allowlisted ambient
        // one of the same name, and arbitrary agent keys still reach the child).
        let mut child_env: Vec<(String, String)> =
            source_env.into_iter().filter(|(k, _)| allow.contains(k.as_str())).collect();
        child_env.extend(extra_env);

        let log_path = env.logs.join(spec.log_file);
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&log_path)?;

        // e38.23: build the spawn command through the OS-level FS sandbox so the
        // provider can only read/write the task's isolated roots. The sandbox
        // wraps the program (Seatbelt `sandbox-exec` on macOS / a `pre_exec`
        // Landlock ruleset on Linux); on an unsupported platform it returns a
        // transparent passthrough (`Enforcement::None`) so a task still runs.
        // The env allowlist + process-group kill below are unchanged — the
        // sandbox is an *additional* FS-confinement layer, not a replacement for
        // the secret-leak env boundary.
        let mut command = self.build_command(program, env)?;
        let mut child = command
            .args(&spec.argv)
            .current_dir(&env.workdir)
            .env_clear()
            .envs(child_env)
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
            tracing::warn!(provider = spec.name, reason = "timeout", "runner_failed");
            RunOutcome::Failed {
                reason: FailureReason::Timeout,
                result,
            }
        } else if exit_code == Some(0) {
            RunOutcome::Success(result)
        } else if exit_code == Some(EX_TEMPFAIL) {
            // `EX_TEMPFAIL` (75): the provider signalled a transient runtime
            // failure. Classify as infra/retryable so the daemon's retry chain
            // re-dispatches a child task, rather than treating it as a terminal
            // agent error.
            tracing::warn!(
                provider = spec.name,
                reason = "runtime_offline",
                "runner_failed"
            );
            RunOutcome::Failed {
                reason: FailureReason::RuntimeOffline,
                result,
            }
        } else {
            tracing::warn!(provider = spec.name, reason = "agent_error", exit_code = ?exit_code, "runner_failed");
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
