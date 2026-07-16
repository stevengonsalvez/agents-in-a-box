//! Agent CLI subprocess execution — the `claude` provider (P1.7).
//!
//! [`Runner::run_claude`] spawns the `claude` binary inside a task's isolated
//! [`ExecEnv`], with a **deny-by-default** env (only the 12-var allowlist passes
//! through — see [`ENV_ALLOWLIST`]), streams its JSONL stdout line-by-line to
//! `{logs}/claude.jsonl`, pins the first `session_id` it sees, and enforces a
//! hard runtime deadline (kill on timeout). Mirrors the reference control plane's
//! `daemon.go` session-pinning + allowlisted-exec pattern.
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

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use ainb_hangar_store::service::fail::FailureReason;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::execenv::ExecEnv;

/// Where a provider run executes (spec F5).
///
/// The pre-F5 run always executed in the task's own `ExecEnv::workdir`, which
/// lives *under* `ExecEnv::root()` — the base the OS sandbox confines writes to.
/// F5 lets a run execute in a provisioned git worktree / scratch repo that lives
/// OUTSIDE that tree (`~/.agents-in-a-box/worktrees/<slug>` etc.), so the run's
/// cwd must be pointed there AND the sandbox must be widened to allow writes into
/// it — otherwise the confinement would block the very checkout the agent is
/// supposed to work in.
///
/// [`RunLocation::in_task_tree`] is the pre-F5 default (cwd = the task workdir,
/// no extra root — the workdir is already inside the sandbox base).
#[derive(Debug, Clone)]
pub struct RunLocation {
    /// The provider subprocess's working directory.
    pub cwd: PathBuf,
    /// A FS root beyond `ExecEnv::root()` the sandbox must additionally allow the
    /// provider to read+write — the provisioned worktree / scratch dir — or
    /// `None` when the cwd is the in-tree fallback workdir (already covered).
    pub extra_root: Option<PathBuf>,
}

impl RunLocation {
    /// The pre-F5 default: run in the task's own `workdir` with no extra sandbox
    /// root (the workdir is already under the sandbox's write base).
    #[must_use]
    pub fn in_task_tree(env: &ExecEnv) -> Self {
        Self {
            cwd: env.workdir.clone(),
            extra_root: None,
        }
    }
}

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
    // ccc / D11: the parent-session linkage the daemon stamps onto every task it
    // spawns (see `run_loop`). It is daemon-controlled config, not an inherited
    // ambient secret, so allowlisting it leaks nothing — and it MUST survive the
    // deny-by-default filter, or the lifecycle hook's fleet-membership gate never
    // resolves and the run's AskUserQuestion never reaches the attention pipeline.
    ainb_fleet_core::session_registry::PARENT_ENV,
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
/// Codex refuses to run outside a git repo ("Not inside a trusted directory and
/// --skip-git-repo-check was not specified", exit 1) — a guard against turning an
/// agent loose where its edits cannot be reviewed or reverted. A task's workdir is
/// only a git worktree on the F5 repo path; a chat / autopilot task runs in the
/// daemon's own freshly-created in-tree workdir, which is NOT a repo, so codex
/// would exit instantly there. The daemon supplies its own confinement (per-task
/// isolated dir + FS sandbox + teardown), which is what codex's check is proxying
/// for, so skip it rather than strand every non-repo codex task.
///
/// That justification is only honest while the FS sandbox is actually ON, and
/// TODAY IT IS NOT. A confined provider cannot reach its own credential, so a
/// headless run fails "Not logged in" and completes only with
/// `HANGAR_DAEMON_DISABLE_SANDBOX=1` — i.e. in the one configuration codex
/// actually runs, its own guard is skipped AND the confinement offered in
/// exchange is absent. This flag is currently spending protection the daemon is
/// not providing. That is a known, accepted interim state, not a claim of
/// safety: the credential is being moved to a parent-injected env var
/// (`CLAUDE_CODE_OAUTH_TOKEN`), after which confinement is re-enabled by default
/// and this rationale becomes true again (bead `ai-coder-rules-48b`).
///
/// HEADLESS ONLY: it is an `exec` subcommand flag, and passing it to the
/// interactive top-level command is a hard parse error (verified: `codex
/// --skip-git-repo-check` → "unexpected argument", exit 2). See
/// [`Runner::codex_spec`] for why that is also the right security posture.
const CODEX_SKIP_GIT_CHECK_FLAG: &str = "--skip-git-repo-check";
/// The codex sandbox-policy flag (`codex exec -s <mode>`), verified against
/// codex-cli 0.144.0 (`-s, --sandbox <SANDBOX_MODE>`, possible values
/// `read-only`, `workspace-write`, `danger-full-access`).
const CODEX_SANDBOX_FLAG: &str = "-s";
/// The sandbox policy the daemon selects for a HEADLESS codex `exec` run:
/// `danger-full-access` — codex applies NO internal FS/network confinement.
///
/// This is not a convenience toggle; without it a codex agent can do no work.
/// `codex exec` DEFAULTS to a read-only sandbox, so a headless run with no `-s`
/// flag lets the model *invoke* a tool yet silently drops the write — the task
/// exits 0 having produced nothing (verified against codex-cli 0.144.0: a brief
/// instructing `printf … > file` ran the tool, exited 0, and wrote NO file with
/// no `-s`; the identical brief under `-s workspace-write` and
/// `-s danger-full-access` both wrote the file and exited 0). That is precisely
/// the "reaches `done` having done no real work" bug class the live tripwire
/// exists to catch, and it is a total functional break for the codex provider,
/// not a subtle policy choice.
///
/// Why `danger-full-access` rather than `workspace-write`: the daemon's OWN
/// OS-level FS sandbox (Seatbelt/Landlock, e38.23) is the confinement boundary —
/// the same justification already documented for [`CODEX_SKIP_GIT_CHECK_FLAG`].
/// Layering codex's internal Seatbelt UNDER the daemon's Seatbelt on macOS nests
/// two sandboxes and breaks path resolution; `danger-full-access` disables
/// codex's own confinement so the daemon's is the single boundary. This mirrors
/// copilot's mandatory `--allow-all-tools` exactly (both delegate confinement to
/// the daemon FS sandbox + env allowlist, both surfaced by the P5.6
/// `warnings::danger-full-access` operator warning). NOTE: "the daemon's is the
/// single boundary" holds only while the daemon FS sandbox is ON; in the current
/// `HANGAR_DAEMON_DISABLE_SANDBOX` interim there is no boundary for ANY provider
/// (claude/codex/copilot alike) — the real confinement floor is turning the daemon
/// sandbox on, tracked separately, not hardening codex in isolation. HEADLESS ONLY: the
/// interactive path deliberately has no FS sandbox and a human is attached to
/// answer codex's own trust/approval prompts, so it keeps codex's default
/// confinement (see [`Runner::codex_spec`]).
const CODEX_SANDBOX_HEADLESS: &str = "danger-full-access";
/// The provider-log file written under [`ExecEnv::logs`] for the `copilot`
/// provider. Its own log keeps a copilot transcript separate from claude/codex.
const COPILOT_LOG_FILE: &str = "copilot.jsonl";
/// The claude flag that makes a run non-interactive. Verified against Claude
/// Code 2.1.210, whose usage is `claude [options] [command] [prompt]` and whose
/// help reads `-p, --print   Print response and exit (useful for pipes)`.
///
/// It is a BOOLEAN that takes no value — the brief is a trailing POSITIONAL, not
/// this flag's argument. (Contrast [`COPILOT_HEADLESS_PROMPT_FLAG`], which looks
/// identical (`-p`) but genuinely takes the prompt as its value. Same spelling,
/// opposite grammar — hence the deliberately different names.)
const CLAUDE_PRINT_FLAG: &str = "-p";
/// The claude model flag (`claude --model <model>`).
const CLAUDE_MODEL_FLAG: &str = "--model";
/// The claude flag that auto-approves EVERY tool call, including `Bash`.
///
/// # Why the daemon must be explicit about permissions
///
/// With NO permission flag, claude's tool gating is decided by
/// `$HOME/.claude/settings.json` (`defaultMode` / `allow`) — i.e. by the
/// OPERATOR'S PERSONAL CONFIG, which the daemon neither owns nor controls. That
/// makes headless behaviour silently machine-dependent. Measured against Claude
/// Code 2.1.210 (`Bash` proven by an env nonce the model cannot fabricate with
/// `Write`):
///
/// | `$HOME` | sandbox | argv | `Write` | `Bash` |
/// |---|---|---|---|---|
/// | operator's, `defaultMode: bypassPermissions` | off | (none) | allowed | allowed |
/// | clean (no settings) | off | (none) | denied | denied |
/// | clean | off | `--permission-mode acceptEdits` | allowed | **denied** |
/// | clean | off | `--dangerously-skip-permissions` | allowed | allowed |
/// | operator's | **ON** | (none) | **denied** | **denied** |
///
/// The last row is the one that matters: the sandbox's read roots exclude the
/// operator's `$HOME`, so `~/.claude/settings.json` is UNREADABLE and a personal
/// `defaultMode` cannot apply. Confined + flagless therefore always denies.
///
/// A denial is not loud. Claude emits `permission_denials`, answers in prose ("I
/// attempted to create the file but the write permission wasn't granted"), and
/// **exits 0** — and [`RunOutcome::Success`] is keyed on exit code 0, so the
/// daemon marks such a task `done` over an untouched workdir. Carrying the flag
/// makes the posture explicit, deterministic, and independent of whose machine
/// the daemon runs on.
///
/// See [`Runner::claude_spec`] for the trust posture this implies.
const CLAUDE_SKIP_PERMISSIONS_FLAG: &str = "--dangerously-skip-permissions";
/// The copilot HEADLESS prompt flag (`copilot -p "<text>"`). Verified against
/// Copilot CLI 1.0.68: "Execute a prompt in non-interactive mode (exits after
/// completion)" — so it is exactly wrong for an attachable session, which is why
/// [`Mode`] picks between this and [`COPILOT_INTERACTIVE_PROMPT_FLAG`].
const COPILOT_HEADLESS_PROMPT_FLAG: &str = "-p";
/// The copilot INTERACTIVE prompt flag (`copilot -i "<text>"`). Verified against
/// Copilot CLI 1.0.68: "-i, --interactive <prompt>   Start interactive mode and
/// automatically execute this prompt" — a real session, seeded with the brief.
///
/// Copilot has NO positional prompt (`copilot [options] [command]`; a bare
/// positional is rejected with "Invalid command format"), so this value-taking
/// flag is the only way to seed an interactive copilot.
const COPILOT_INTERACTIVE_PROMPT_FLAG: &str = "-i";
/// The end-of-options separator: everything after it is a positional, never a
/// flag.
///
/// The brief is arbitrary issue text, so it can start with `-` (an issue titled
/// `- fix the login bug` is ordinary bullet-style prose). Without this separator
/// the provider's parser reads the brief as flags. Verified against the real
/// binaries:
///
/// * `codex exec "-fix the login bug"` → `error: unexpected argument '-f' found`
///   (clap itself suggests "to pass '-f' as a value, use '-- -f'"); with `--` it
///   parses.
/// * `claude -p "-reply with …"` → the leading `-r` is silently absorbed as
///   claude's own `-r/--resume`, which then fails on the REST of the brief
///   ("Provided value \"eply with …\" is not a UUID"). A misparse into a
///   different flag, not merely a rejection; with `--` the brief is delivered
///   verbatim (verified: a `-`-leading brief round-tripped its answer back).
///
/// It also settles two adjacent hazards for free: a value-taking flag in the
/// agent's `cli_args` can no longer swallow the brief (the separator terminates
/// option parsing first), and a brief that is exactly a subcommand name can no
/// longer hijack it (verified: `codex exec review` → "Specify --uncommitted …";
/// `codex exec -- review` treats it as the prompt).
///
/// It is NOT used for copilot: its brief rides a value-taking flag (`-p`/`-i`),
/// which already consumes a dash-leading value verbatim, and inserting `--`
/// there BREAKS it — `copilot -p -- "-fix the login bug"` makes `--` the prompt
/// value and then rejects the brief as `error: unknown option` (verified).
const ARG_SEPARATOR: &str = "--";
/// The copilot flag that permits tool use without an interactive confirmation
/// prompt. Verified against GitHub Copilot CLI 1.0.68: `--allow-all-tools` is
/// documented as "required for non-interactive mode", so a headless run without
/// it stalls on a permission prompt it can never answer (stdin is null). The FS
/// sandbox + env allowlist remain the real confinement boundary.
const COPILOT_ALLOW_ALL_TOOLS_FLAG: &str = "--allow-all-tools";
/// The copilot model flag (`copilot --model <model>`), verified against Copilot
/// CLI 1.0.68 (`$ copilot --model gpt-5.4`).
const COPILOT_MODEL_FLAG: &str = "--model";

/// Static configuration for a [`Runner`].
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Absolute path to the `claude` binary (or a test stand-in script).
    pub claude_path: PathBuf,
    /// Absolute path to the `codex` binary (or a test stand-in script). Used by
    /// [`Runner::run_codex`] (e38.16); a daemon that never dispatches a codex
    /// task simply never spawns it.
    pub codex_path: PathBuf,
    /// Absolute path to the `copilot` binary (or a test stand-in script). Used by
    /// [`Runner::run_copilot`]; a daemon that never dispatches a copilot task
    /// simply never spawns it.
    pub copilot_path: PathBuf,
    /// Hard wall-clock deadline; the subprocess is killed past it
    /// ([`FailureReason::Timeout`]). Reference default: 2.5h.
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

/// Token/cost usage parsed from a provider's final `result` JSONL line (e38.35).
///
/// The agent CLI (claude / codex) emits a terminal `{"type":"result",…}` line
/// carrying a `usage` object (input/output tokens) and `total_cost_usd`. The
/// runner pins it the way it pins `session_id`, so the daemon can persist it at
/// the finalize seam and the usage dashboard can roll it up. A run that reports
/// no result-usage leaves [`RunnerResult::usage`] as `None`.
///
/// Carries an `f64` cost, so it is `PartialEq` only (no `Eq`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsage {
    /// Prompt/input tokens the provider reported.
    pub input_tokens: i64,
    /// Completion/output tokens the provider reported.
    pub output_tokens: i64,
    /// Total cost in US dollars the provider reported (0 when none reported).
    pub cost_usd: f64,
}

/// The captured result of one provider run.
///
/// Holds an `f64` cost via [`ProviderUsage`], so it is `PartialEq` only. The
/// `Default` (all fields empty / `None`) is the "no captured output" result a
/// cancelled run carries — its provider was killed before any JSONL was read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunnerResult {
    /// Process exit code, or `None` if the process was killed by signal/timeout.
    pub exit_code: Option<i32>,
    /// The first `session_id` parsed from a `{"type":"system",...}` JSONL line,
    /// or `None` if the provider emitted none.
    pub session_id: Option<String>,
    /// Token/cost usage parsed from the final `{"type":"result",...}` JSONL line,
    /// or `None` if the provider reported none (e38.35).
    pub usage: Option<ProviderUsage>,
    /// Trailing stdout lines (up to [`RunnerConfig::tail_lines`]), newline-joined.
    pub stdout_tail: String,
    /// Trailing stderr lines (up to [`RunnerConfig::tail_lines`]), newline-joined.
    pub stderr_tail: String,
}

/// How a provider run finished, ready for the daemon to map onto the task FSM.
///
/// Holds an `f64` cost via [`RunnerResult`], so it is `PartialEq` only.
#[derive(Debug, Clone, PartialEq)]
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
    /// The run was cancelled by a human mid-flight (tcp T3 / F6): the claim loop
    /// caught its kill signal and stopped the provider (a headless process group
    /// via `kill_on_drop`, or the interactive tmux session by exact name). The
    /// daemon finalises through the dedicated cancelled seam (`running ->
    /// cancelled`) — NOT the failure path, so a cancel never auto-moves the card
    /// to a `failed` column nor spawns a retry child.
    Cancelled(RunnerResult),
}

impl RunOutcome {
    /// Borrow the captured [`RunnerResult`] regardless of outcome.
    #[must_use]
    pub const fn result(&self) -> &RunnerResult {
        match self {
            Self::Success(r) | Self::Cancelled(r) | Self::Failed { result: r, .. } => r,
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

/// A `result`-type JSONL line, decoded to pin token/cost usage (e38.35).
///
/// The agent CLI's terminal `{"type":"result",…}` line carries a `usage` object
/// and `total_cost_usd`. Every field is `#[serde(default)]` so a result line that
/// omits usage (or a future shape change) decodes to zeros rather than failing
/// the whole run.
#[derive(Debug, Deserialize)]
struct ResultLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    total_cost_usd: f64,
    #[serde(default)]
    usage: Option<UsageBlock>,
}

/// The `usage` sub-object of a [`ResultLine`]: the token tallies (e38.35).
#[derive(Debug, Default, Deserialize)]
struct UsageBlock {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
}

/// Which provider exec path the daemon routes a task to (e38.16).
///
/// Resolved from the task's AGENT `provider` when set (migration 0041), else its
/// runtime's advertised `provider`. Every wired provider — `claude`, `codex`,
/// `copilot` — has its own exec path; only a genuinely unrecognised name falls
/// back to [`Self::Claude`] so a misconfigured agent still dispatches (rather than
/// stranding the task).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// The `claude` provider — [`Runner::run_claude`]. The default exec path: a
    /// task whose provider is unrecognised or unresolvable still dispatches here.
    #[default]
    Claude,
    /// The `codex` provider — [`Runner::run_codex`].
    Codex,
    /// The `copilot` provider (GitHub Copilot CLI) — [`Runner::run_copilot`].
    Copilot,
}

impl Backend {
    /// Resolve a provider wire name (`"claude"`, `"codex"`, `"copilot"`) to a
    /// backend.
    ///
    /// Matching is case-insensitive. Only a genuinely UNKNOWN name falls back to
    /// [`Self::Claude`] (the safe default) — every wired provider routes to its own
    /// exec path, mirroring [`crate::materialise::ProviderSkillLayout::from_provider`].
    #[must_use]
    pub fn from_provider(provider: &str) -> Self {
        match provider.to_ascii_lowercase().as_str() {
            "codex" => Self::Codex,
            "copilot" => Self::Copilot,
            _ => Self::Claude,
        }
    }

    /// The provider's wire name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
        }
    }
}

/// Which contract a provider's argv is built for.
///
/// The two are OPPOSITE asks of the same binary, and every provider spells them
/// differently, so the mode is an explicit argument rather than an implied
/// default: one argv must never serve both. Passing a headless argv to the
/// interactive path spawns a print-and-exit process into a pane the operator is
/// meant to attach to and drive (claude's `-p/--print` is literally "Print
/// response and exit"; copilot's `-p` is "exits after completion").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A captured subprocess with a null stdin: the provider must do the work
    /// unattended and exit. Nobody can answer a prompt or drive a REPL.
    Headless,
    /// A REAL, attachable tmux terminal (ccc / D6): the provider must start a
    /// live session, seeded with the brief, that the operator can take over.
    Interactive,
}

/// What to invoke a provider with for ONE run: the task's prompt plus the
/// per-agent config (e38.16, from the agent row's migration-0015 columns).
///
/// `prompt` / `model` / `cli_args` flow onto the provider's command line; the
/// agent's `agent_env` is threaded separately (it goes into the child env, not
/// the argv) via the `extra_env` argument of [`Runner::run_codex_with_env`].
///
/// # No `Default`
///
/// Deliberately NOT [`Default`]: a defaulted invocation is a promptless one, and
/// a promptless provider does not run — it exits non-zero at once (verified:
/// bare `claude` with a null stdin exits 1, "Input must be provided either
/// through stdin or as a prompt argument when using --print"). A `..default()`
/// fault path therefore does not degrade gracefully, it just fails later and
/// less legibly. Callers must state the prompt, even if it is only a fallback
/// (see `run_loop`'s `ResolvedDispatch::fallback`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInvocation {
    /// The task brief. Reaches the provider as a positional after
    /// [`ARG_SEPARATOR`] (claude / codex) or as the value of a prompt flag
    /// (copilot); [`Mode`] decides the surrounding shape.
    ///
    /// MUST be non-empty — see the type-level "No `Default`" note. The specs do
    /// not guard against an empty prompt: a caller that manufactures one gets a
    /// loud provider-level failure rather than a silent interactive hang.
    ///
    /// # Visible in `ps`
    ///
    /// This lands in the child's argv, so it is world-readable via `ps` on the
    /// host. Accepted deliberately: the brief is the operator's own issue text,
    /// and `model` / `cli_args` already ride the same argv. It is a real (if
    /// small) widening of what a local user can see versus keeping the brief on
    /// stdin — noted here so the next reader knows it was a decision, not an
    /// oversight. The repo's threat model does care about other local users (see
    /// the 0o700 wrapper rationale in `interactive.rs`), so if a brief ever
    /// carries something more sensitive than an issue title, stdin is the seam
    /// to move it to.
    pub prompt: String,
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
/// (see [`Runner::claude_spec`] / [`Runner::codex_spec`] / [`Runner::copilot_spec`])
/// rather than a new copy of the run loop.
struct ProviderSpec {
    /// Which provider this is — its wire name for logs/tracing
    /// ([`Backend::name`]).
    backend: Backend,
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

    /// The hard wall-clock deadline each run is bounded by (the interactive tmux
    /// path — [`crate::interactive`] — reuses the same budget the headless path
    /// enforces).
    #[must_use]
    pub const fn max_runtime(&self) -> Duration {
        self.cfg.max_runtime
    }

    /// Build the (tokio) spawn command for `program`, wrapped in the OS-level FS
    /// sandbox when [`RunnerConfig::sandbox`] is on.
    ///
    /// `program` is the provider binary (claude / codex / copilot): every provider spawns
    /// through this one wrapper, so the codex exec path gets exactly the same
    /// confinement as claude (e38.16). The confinement policy is derived from the
    /// task's [`ExecEnv`]: writes are confined to the task root
    /// (`workdir`/`output`/`logs` all live under it) + the process temp dir;
    /// reads are confined to the system roots a real agent needs + the task root;
    /// network egress to the model API stays allowed. With the sandbox off, or on
    /// an unsupported platform, the command is the bare provider binary (the env
    /// allowlist + process-group kill still apply).
    ///
    /// # No credential grant
    ///
    /// The confined child is granted NOTHING under the operator's `$HOME` —
    /// including the provider's own credential store. That is deliberate, and it
    /// means a provider whose token lives in the macOS Keychain cannot
    /// authenticate here: a real headless run fails "Not logged in · Please run
    /// /login" and needs `HANGAR_DAEMON_DISABLE_SANDBOX=1` to complete. An
    /// earlier attempt to grant the Keychain instead handed the child every
    /// Chrome-saved password — see the rationale on
    /// [`ainb_hangar_sandbox::SandboxPolicy`]. The fix is to inject the
    /// credential as an env var from the UNSANDBOXED parent daemon
    /// (`claude setup-token` -> `CLAUDE_CODE_OAUTH_TOKEN`), which needs no grant
    /// into `$HOME` at all; until that lands, sandboxed headless runs are
    /// unauthenticated by design.
    ///
    /// # Errors
    ///
    /// Returns an [`std::io::Error`] only if a *supported* sandbox primitive is
    /// expected but unavailable, or a sandbox setup IO fault occurs. An
    /// unsupported platform is NOT an error — it degrades to a passthrough.
    fn build_command(
        &self,
        program: &std::path::Path,
        env: &ExecEnv,
        extra_root: Option<&Path>,
    ) -> std::io::Result<Command> {
        if !self.cfg.sandbox {
            let cmd = ainb_hangar_sandbox::SandboxedCommand::passthrough(program).into_inner();
            return Ok(Command::from(cmd));
        }

        // F5: when the run executes in a provisioned worktree / scratch repo that
        // lives outside the task tree, widen the confinement to read+write it —
        // else the sandbox would block the agent from touching its own checkout.
        let mut policy = ainb_hangar_sandbox::SandboxPolicy::confined_to(env.root());
        if let Some(root) = extra_root {
            policy = policy.allow_read(root).allow_write(root);
        }
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
    pub async fn run_claude<I>(
        &self,
        env: &ExecEnv,
        source_env: I,
        invocation: &ProviderInvocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        self.run_claude_in(env, source_env, invocation, &RunLocation::in_task_tree(env))
            .await
    }

    /// [`Self::run_claude`], but executing in an explicit [`RunLocation`] (F5) —
    /// a provisioned worktree / scratch repo rather than the in-tree workdir. The
    /// sandbox is widened to the location's extra root so the agent can write its
    /// checkout.
    ///
    /// # Errors
    ///
    /// As [`Self::run_claude`].
    pub async fn run_claude_in<I>(
        &self,
        env: &ExecEnv,
        source_env: I,
        invocation: &ProviderInvocation,
        location: &RunLocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        self.run_claude_in_with_env(env, source_env, std::iter::empty(), invocation, location)
            .await
    }

    /// [`Self::run_claude_in`], plus a set of `extra_env` pairs layered onto the
    /// child env **after** the allowlist filter (the codex/copilot counterpart is
    /// [`Self::run_codex_in`]'s `extra_env`).
    ///
    /// This is the injection path for the daemon-resolved claude credential
    /// (`crate::claude_cred`): a confined child can reach neither the Keychain nor
    /// the operator's `~/.claude`, so the daemon supplies `CLAUDE_CODE_OAUTH_TOKEN`
    /// here. It rides `extra_env` — NOT `source_env` — precisely so it does NOT go
    /// through (and does not have to widen) the deny-by-default allowlist: an
    /// *ambient* `CLAUDE_CODE_OAUTH_TOKEN` in the daemon's own env is still dropped
    /// for every provider, while the *resolved* value reaches this claude child
    /// only. The caller is responsible for backend-gating what it passes here.
    ///
    /// # Errors
    ///
    /// As [`Self::run_claude`].
    pub async fn run_claude_in_with_env<I, E>(
        &self,
        env: &ExecEnv,
        source_env: I,
        extra_env: E,
        invocation: &ProviderInvocation,
        location: &RunLocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
        E: IntoIterator<Item = (String, String)>,
    {
        // The task brief reaches claude as the trailing positional; the workdir +
        // materialised home carry the CONTEXT (CLAUDE.md, skills) but never the ask
        // itself. `run_provider` captures stdout against a null stdin, so this is
        // unambiguously the headless contract.
        let spec = Self::claude_spec(invocation, Mode::Headless);
        self.run_provider(
            &self.cfg.claude_path,
            env,
            source_env,
            extra_env,
            spec,
            location,
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
        self.run_codex_in(
            env,
            source_env,
            extra_env,
            invocation,
            &RunLocation::in_task_tree(env),
        )
        .await
    }

    /// [`Self::run_codex_with_env`], but executing in an explicit [`RunLocation`]
    /// (F5) — the codex counterpart of [`Self::run_claude_in`].
    ///
    /// # Errors
    ///
    /// As [`Self::run_claude`].
    pub async fn run_codex_in<I, E>(
        &self,
        env: &ExecEnv,
        source_env: I,
        extra_env: E,
        invocation: &ProviderInvocation,
        location: &RunLocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
        E: IntoIterator<Item = (String, String)>,
    {
        let spec = Self::codex_spec(invocation, Mode::Headless);
        self.run_provider(
            &self.cfg.codex_path,
            env,
            source_env,
            extra_env,
            spec,
            location,
        )
        .await
    }

    /// Run the `copilot` provider (GitHub Copilot CLI) for one task.
    ///
    /// The copilot counterpart of [`Self::run_codex`]: same orchestration
    /// ([`Self::run_provider`]), same env allowlist + sandbox confinement, its own
    /// program path, argv, and log file.
    ///
    /// # Errors
    ///
    /// As [`Self::run_claude`].
    pub async fn run_copilot<I>(
        &self,
        env: &ExecEnv,
        source_env: I,
        invocation: &ProviderInvocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        self.run_copilot_with_env(env, source_env, std::iter::empty(), invocation).await
    }

    /// [`Self::run_copilot`], plus per-agent `extra_env` overrides layered onto the
    /// child env *after* the allowlist filter — the copilot counterpart of
    /// [`Self::run_codex_with_env`].
    ///
    /// # Errors
    ///
    /// As [`Self::run_claude`].
    pub async fn run_copilot_with_env<I, E>(
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
        self.run_copilot_in(
            env,
            source_env,
            extra_env,
            invocation,
            &RunLocation::in_task_tree(env),
        )
        .await
    }

    /// [`Self::run_copilot_with_env`], but executing in an explicit [`RunLocation`]
    /// (F5) — the copilot counterpart of [`Self::run_codex_in`].
    ///
    /// # Errors
    ///
    /// As [`Self::run_claude`].
    pub async fn run_copilot_in<I, E>(
        &self,
        env: &ExecEnv,
        source_env: I,
        extra_env: E,
        invocation: &ProviderInvocation,
        location: &RunLocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
        E: IntoIterator<Item = (String, String)>,
    {
        let spec = Self::copilot_spec(invocation, Mode::Headless);
        self.run_provider(
            &self.cfg.copilot_path,
            env,
            source_env,
            extra_env,
            spec,
            location,
        )
        .await
    }

    /// The program path + argv for a provider run in `mode`, WITHOUT spawning it
    /// (ccc / D6).
    ///
    /// The interactive tmux path ([`crate::interactive`]) needs the exact program
    /// and arguments to exec inside a tmux session rather than a captured
    /// subprocess. Deriving them here keeps each provider's argv shape in one
    /// place — but the shape DIFFERS by [`Mode`], so the caller must say which
    /// contract it is spawning for; the headless argv is print-and-exit and would
    /// hand the operator a dead pane.
    #[must_use]
    pub fn provider_command(
        &self,
        backend: Backend,
        invocation: &ProviderInvocation,
        mode: Mode,
    ) -> (PathBuf, Vec<String>) {
        match backend {
            Backend::Claude => (
                self.cfg.claude_path.clone(),
                Self::claude_spec(invocation, mode).argv,
            ),
            Backend::Codex => (
                self.cfg.codex_path.clone(),
                Self::codex_spec(invocation, mode).argv,
            ),
            Backend::Copilot => (
                self.cfg.copilot_path.clone(),
                Self::copilot_spec(invocation, mode).argv,
            ),
        }
    }

    /// The `claude` provider spec: claude log file + `[-p]
    /// --dangerously-skip-permissions [--model <model>] [<cli_args>…] -- <prompt>`.
    ///
    /// Verified against Claude Code 2.1.210, whose usage is
    /// `claude [options] [command] [prompt]`:
    ///
    /// * [`Mode::Headless`] adds `-p/--print` ("Print response and exit"). Without
    ///   it the daemon's spawn (null stdin, captured stdout) exits 1 having done
    ///   nothing.
    /// * [`Mode::Interactive`] OMITS it: the brief is still delivered (as the same
    ///   trailing positional), but claude starts the real session the operator
    ///   attaches to. `-p` here would print and exit into an empty pane.
    ///
    /// The prompt is a POSITIONAL either way — `-p` is a boolean and never takes
    /// it — and rides last, after [`ARG_SEPARATOR`], so a `-`-leading brief cannot
    /// be read as a flag.
    ///
    /// # Permission policy: blanket tool autonomy, deliberately
    ///
    /// `--dangerously-skip-permissions` (both modes) auto-approves **EVERY** tool
    /// call — including `Bash`, i.e. arbitrary shell commands — in an unattended
    /// background subprocess. This is an explicit operator decision, not a
    /// default that drifted in, and it is stated plainly rather than softened:
    ///
    /// * A headless claude MUST carry some permission flag or its gating falls
    ///   through to the operator's personal `~/.claude/settings.json` — which the
    ///   sandbox makes unreadable, so a confined run denies every tool, exits 0,
    ///   and is marked `done` over an untouched workdir (see
    ///   [`CLAUDE_SKIP_PERMISSIONS_FLAG`] for the measured matrix).
    /// * `--permission-mode acceptEdits` is narrower and closes the *write* case,
    ///   but MEASURABLY denies `Bash` (verified with a clean `$HOME` and an
    ///   env-nonce only a real shell could resolve). A task that must run a build,
    ///   a test, or any command would therefore be denied — and, by the very same
    ///   exit-0 mechanism, still report `done` having half-done the work. Closing
    ///   the write no-op while leaving the shell no-op open was judged worse than
    ///   granting the wider mode knowingly.
    /// * `--allowedTools <list>` cannot be fixed ahead of time: the daemon does not
    ///   know a brief's tool needs, so a static list breaks arbitrary tasks.
    ///
    /// ## What actually confines this
    ///
    /// The blast radius is bounded by the FS sandbox (Seatbelt/Landlock) + the
    /// deny-by-default env allowlist — NOT by claude's own permission prompt,
    /// which is now fully bypassed. **The sandbox is currently OFF for headless
    /// runs** (bead `ai-coder-rules-48b`: a confined child cannot reach the
    /// Keychain credential), so until that lands, a brief built from untrusted
    /// text (e.g. a board issue) can drive arbitrary shell as the daemon user.
    /// That exposure is accepted knowingly and is why 48b matters.
    ///
    /// A task can still override via `cli_args`, appended after this flag.
    ///
    /// Claude joins the other providers in carrying blanket tool autonomy: copilot
    /// `--allow-all-tools` (mandatory for non-interactive mode); codex runs
    /// `--full-auto` only when the agent passes it via `cli_args` (its spec adds
    /// no permission flag of its own).
    fn claude_spec(invocation: &ProviderInvocation, mode: Mode) -> ProviderSpec {
        let mut argv = Vec::new();
        if mode == Mode::Headless {
            argv.push(CLAUDE_PRINT_FLAG.to_string());
        }
        argv.push(CLAUDE_SKIP_PERMISSIONS_FLAG.to_string());
        if let Some(model) = &invocation.model {
            argv.push(CLAUDE_MODEL_FLAG.to_string());
            argv.push(model.clone());
        }
        argv.extend(invocation.cli_args.iter().cloned());
        argv.push(ARG_SEPARATOR.to_string());
        argv.push(invocation.prompt.clone());
        ProviderSpec {
            backend: Backend::Claude,
            log_file: CLAUDE_LOG_FILE,
            argv,
        }
    }

    /// The `codex` provider spec: codex log file + `[exec --skip-git-repo-check
    /// -s danger-full-access] [-m <model>] [<cli_args>…] -- <prompt>` (e38.16).
    ///
    /// Verified against codex-cli 0.144.0, whose usage is both
    /// `codex [OPTIONS] [PROMPT]` (interactive TUI) and `codex exec …`
    /// ("Run Codex non-interactively"):
    ///
    /// * [`Mode::Headless`] leads with the `exec` subcommand, plus
    ///   [`CODEX_SKIP_GIT_CHECK_FLAG`] and the [`CODEX_SANDBOX_FLAG`]
    ///   `danger-full-access` policy — without the latter, codex's default
    ///   read-only `exec` sandbox drops every write and the agent produces
    ///   nothing (see [`CODEX_SANDBOX_HEADLESS`]).
    /// * [`Mode::Interactive`] omits ALL THREE, so the top-level TUI starts with the
    ///   brief as its opening prompt. `codex exec` in a tmux pane would stream and
    ///   exit rather than give the operator a session.
    ///
    /// # Why the git-repo check is skipped headlessly but not interactively
    ///
    /// This is not a judgement call — the CLI settles it. `--skip-git-repo-check`
    /// is an `exec`-only flag: `codex --skip-git-repo-check …` at the top level is
    /// a hard parse error ("unexpected argument '--skip-git-repo-check' found",
    /// exit 2, verified), so an interactive session CANNOT carry it and would
    /// refuse to start if it did.
    ///
    /// That happens to match the security reasoning. The flag's justification is
    /// that the daemon supplies the confinement codex's check proxies for
    /// (per-task isolated dir + FS sandbox + teardown) — but the interactive path
    /// DELIBERATELY has no FS sandbox (see `run_loop::run_interactive`), so that
    /// justification does not hold there. It does not need to: a human is attached
    /// to the session and can answer codex's trust prompt themselves, which is
    /// exactly the review the check exists to secure.
    ///
    /// The prompt is a trailing POSITIONAL in both shapes, after every option and
    /// after [`ARG_SEPARATOR`] (which also stops a value-taking flag in `cli_args`
    /// from swallowing it, and stops a brief like `review` from hijacking
    /// `codex exec`'s `review` subcommand). Verified to compose:
    /// `codex exec --skip-git-repo-check -- "-fix the login bug"` parses and runs.
    fn codex_spec(invocation: &ProviderInvocation, mode: Mode) -> ProviderSpec {
        let mut argv = Vec::new();
        if mode == Mode::Headless {
            argv.push(CODEX_EXEC_SUBCOMMAND.to_string());
            argv.push(CODEX_SKIP_GIT_CHECK_FLAG.to_string());
            argv.push(CODEX_SANDBOX_FLAG.to_string());
            argv.push(CODEX_SANDBOX_HEADLESS.to_string());
        }
        if let Some(model) = &invocation.model {
            argv.push(CODEX_MODEL_FLAG.to_string());
            argv.push(model.clone());
        }
        argv.extend(invocation.cli_args.iter().cloned());
        argv.push(ARG_SEPARATOR.to_string());
        argv.push(invocation.prompt.clone());
        ProviderSpec {
            backend: Backend::Codex,
            log_file: CODEX_LOG_FILE,
            argv,
        }
    }

    /// The `copilot` provider spec: copilot log file + `--allow-all-tools`
    /// [+ `--model <model>`] [+ the agent's `cli_args`].
    ///
    /// Flags verified against GitHub Copilot CLI 1.0.68 (`copilot --help`):
    /// `--allow-all-tools` is "required for non-interactive mode", and `--model`
    /// is a real flag (`$ copilot --model gpt-5.4`) — so, unlike the interactive
    /// session launcher's stale "no model flag for these providers" rule, the
    /// agent's configured `model` IS threaded here. No subcommand is invented
    /// (copilot has none).
    ///
    /// # Permission policy: copilot is granted blanket tool autonomy, claude is not
    ///
    /// `--allow-all-tools` auto-approves EVERY tool call in an unattended
    /// background subprocess. It is no longer the only provider argv that does so:
    /// claude carries `--dangerously-skip-permissions` and codex `--full-auto`, so
    /// all three now grant blanket tool autonomy (see [`Self::claude_spec`] for the
    /// operator decision behind claude's). The rationale below is why copilot's is
    /// not merely acceptable but unavoidable:
    ///
    /// * It is **mandatory**, not discretionary — Copilot CLI 1.0.68 documents
    ///   `--allow-all-tools` as "required for non-interactive mode", so a copilot
    ///   agent without it stalls on a permission prompt it can never answer (stdin
    ///   is null) and dies. Gating it behind agent config would ship a provider
    ///   that is broken by default — the "recorded but doesn't actually work"
    ///   footgun this surface already rejected once. A required flag is not a
    ///   policy knob.
    /// * The blast radius is bounded by the FS sandbox (Seatbelt/Landlock) and the
    ///   deny-by-default env allowlist, but NOT eliminated: within the sandbox the
    ///   agent may still execute arbitrary commands and reach the network. This is
    ///   the same trust posture the daemon already warns about at dispatch
    ///   (`warnings::danger-full-access`), so copilot is not a new exposure class
    ///   — it is the existing one made explicit in argv.
    /// * Claude's permission policy is now resolved too, and it was NOT the same
    ///   route: an earlier note here assumed claude "reaches the same place by a
    ///   different route" under `--print`. Measured against 2.1.210, it did not —
    ///   a flagless `-p` run DENIED the tool, wrote nothing, and exited 0, which
    ///   the daemon scored as success. Claude now carries
    ///   `--dangerously-skip-permissions` by operator decision, so both providers
    ///   are explicit in argv and equally wide.
    ///
    /// # The brief rides a value-taking flag, chosen by [`Mode`]
    ///
    /// Copilot has NO positional prompt (`copilot [options] [command]`; a bare
    /// positional is rejected with "Invalid command format"), so — unlike claude
    /// and codex — the brief is the VALUE of a flag, and which flag is the whole
    /// interactive/headless distinction (verified against Copilot CLI 1.0.68):
    ///
    /// * [`Mode::Headless`] → `-p <prompt>`: "Execute a prompt in non-interactive
    ///   mode (exits after completion)".
    /// * [`Mode::Interactive`] → `-i <prompt>`: "Start interactive mode and
    ///   automatically execute this prompt" — a real session, seeded with the
    ///   brief, which is what an attachable tmux pane needs.
    ///
    /// Because the brief is a flag VALUE, it needs no [`ARG_SEPARATOR`]: the
    /// parser consumes a `-`-leading value verbatim (verified: `copilot -p
    /// "-fix the login bug"` parses). Adding `--` would BREAK it — `--` becomes
    /// the prompt value and the brief is then rejected as an unknown option.
    fn copilot_spec(invocation: &ProviderInvocation, mode: Mode) -> ProviderSpec {
        let prompt_flag = match mode {
            Mode::Headless => COPILOT_HEADLESS_PROMPT_FLAG,
            Mode::Interactive => COPILOT_INTERACTIVE_PROMPT_FLAG,
        };
        let mut argv = vec![prompt_flag.to_string(), invocation.prompt.clone()];
        argv.push(COPILOT_ALLOW_ALL_TOOLS_FLAG.to_string());
        if let Some(model) = &invocation.model {
            argv.push(COPILOT_MODEL_FLAG.to_string());
            argv.push(model.clone());
        }
        argv.extend(invocation.cli_args.iter().cloned());
        ProviderSpec {
            backend: Backend::Copilot,
            log_file: COPILOT_LOG_FILE,
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
        location: &RunLocation,
    ) -> std::io::Result<RunOutcome>
    where
        I: IntoIterator<Item = (String, String)>,
        E: IntoIterator<Item = (String, String)>,
    {
        let child_env = compose_child_env(source_env, extra_env);

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
        let mut command = self.build_command(program, env, location.extra_root.as_deref())?;
        let mut child = command
            .args(&spec.argv)
            .current_dir(&location.cwd)
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
            // a54 shutdown: SIGKILL the provider if its owning future is dropped
            // (the daemon's `runs` JoinSet aborts every in-flight run on Ctrl-C).
            // WITHOUT this, dropping the aborted future leaves the child alive:
            // it is its own process-group leader (never saw the terminal SIGINT)
            // and would be reparented to init, mutating the workspace unsupervised
            // while its DB row is stuck `running` until the next boot's
            // crash-recovery reclaim. `kill_on_drop` sends SIGKILL to the immediate
            // provider pid synchronously in `Child::drop`, so the run stops even
            // when the drop happens during runtime teardown. It does NOT reach a
            // shelled-out grandchild in the same group (the group leader dying does
            // not kill members) — that residual is backstopped by the workspace GC
            // sweeper, same as any orphaned dir.
            .kill_on_drop(true)
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

        let (session_id, usage, stdout_tail) = stdout_task
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
            usage,
            stdout_tail,
            stderr_tail,
        };

        let outcome = if timed_out {
            tracing::warn!(
                provider = spec.backend.name(),
                reason = "timeout",
                "runner_failed"
            );
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
                provider = spec.backend.name(),
                reason = "runtime_offline",
                "runner_failed"
            );
            RunOutcome::Failed {
                reason: FailureReason::RuntimeOffline,
                result,
            }
        } else {
            tracing::warn!(provider = spec.backend.name(), reason = "agent_error", exit_code = ?exit_code, "runner_failed");
            RunOutcome::Failed {
                reason: FailureReason::AgentError,
                result,
            }
        };
        Ok(outcome)
    }
}

/// Compose a provider subprocess's child environment: the deny-by-default
/// [`ENV_ALLOWLIST`] filter over the ambient `source_env`, with the agent's
/// explicit `extra_env` overrides layered on top.
///
/// Shared by the headless [`Runner::run_provider`] and the interactive tmux path
/// ([`crate::interactive`]) so both apply the identical secret-leak boundary: a
/// per-agent value wins over an allowlisted ambient one of the same name, and
/// arbitrary agent keys still reach the child.
pub(crate) fn compose_child_env<I, E>(source_env: I, extra_env: E) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
    E: IntoIterator<Item = (String, String)>,
{
    let allow: std::collections::HashSet<&str> = ENV_ALLOWLIST.iter().copied().collect();
    let mut child_env: Vec<(String, String)> =
        source_env.into_iter().filter(|(k, _)| allow.contains(k.as_str())).collect();
    // ccc / D11: the daemon's AINB_PARENT_SESSION stamp (an allowlisted
    // `source_env` value) is AUTHORITATIVE fleet-membership config. A per-agent
    // `agent_env` is layered on top and wins over ambient values by name — so
    // WITHOUT this filter an agent config carrying AINB_PARENT_SESSION (or a blank
    // one) would shadow the daemon's stamp, dropping the hook's membership
    // resolution or misrouting the session's Stop completion. Only the daemon sets
    // this key, so drop any the agent env carries before layering.
    child_env.extend(
        extra_env
            .into_iter()
            .filter(|(k, _)| k.as_str() != ainb_fleet_core::session_registry::PARENT_ENV),
    );
    child_env
}

/// Read the child's stdout line-by-line, appending each line to `log_file`,
/// pinning the first `system` line's `session_id` and the last `result` line's
/// token/cost `usage` (e38.35), and retaining a bounded tail.
///
/// Returns `(first_session_id, last_usage, stdout_tail)`. The usage is taken from
/// the LAST `result` line (a multi-result stream's final tally wins), mirroring
/// how `session_id` takes the FIRST `system` line.
async fn stream_stdout(
    stdout: tokio::process::ChildStdout,
    mut log_file: std::fs::File,
    tail_lines: usize,
) -> std::io::Result<(Option<String>, Option<ProviderUsage>, String)> {
    use std::io::Write;

    let mut reader = BufReader::new(stdout).lines();
    let mut session_id: Option<String> = None;
    let mut usage: Option<ProviderUsage> = None;
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
        // Pin token/cost usage from a `result` line. The last result line wins so
        // a stream that ends with a corrected tally reflects the final figure. A
        // result line that reports neither tokens nor cost (e.g. a bare
        // `{"type":"result","content":"ok"}`) carries nothing to record, so it
        // leaves `usage` as `None`.
        if let Ok(parsed) = serde_json::from_str::<ResultLine>(&line) {
            if parsed.kind == "result" {
                let block = parsed.usage.unwrap_or_default();
                let reported = block.input_tokens != 0
                    || block.output_tokens != 0
                    || parsed.total_cost_usd != 0.0;
                if reported {
                    usage = Some(ProviderUsage {
                        input_tokens: block.input_tokens,
                        output_tokens: block.output_tokens,
                        cost_usd: parsed.total_cost_usd,
                    });
                }
            }
        }
        push_tail(&mut tail, line, tail_lines);
    }
    log_file.flush()?;
    Ok((session_id, usage, join_tail(tail)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    /// The Seatbelt profile the daemon actually generates for a confined task
    /// must grant NOTHING under the operator's `$HOME` and no mach service.
    ///
    /// This reads the profile back off the real `sandbox-exec -p <profile>` argv
    /// [`Runner::build_command`] builds, rather than re-deriving it — so it
    /// asserts what the provider is actually spawned under.
    ///
    /// A per-provider "credential grant" (a securityd `mach-lookup` +
    /// `~/Library/Keychains/login.keychain-db`) was briefly shipped here to get a
    /// confined claude authenticated. It handed the child every Chrome-saved
    /// password: under this exact profile,
    /// `security find-generic-password -s 'Chrome Safe Storage' -w` returned the
    /// secret silently. `process-exec*` is allowed, so a confined agent reaches
    /// `/usr/bin/security` on its own, and securityd does NOT arbitrate per-item
    /// for ACL-permissive items. It is gone; this pins that it stays gone.
    #[cfg(target_os = "macos")]
    #[test]
    fn confined_task_profile_grants_no_home_path_and_no_mach_service() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("task");
        let env = ExecEnv {
            workdir: root.join("workdir"),
            output: root.join("output"),
            logs: root.join("logs"),
            gc_meta: root.join(".gc_meta.json"),
        };
        let runner = Runner::new(RunnerConfig {
            claude_path: "claude".into(),
            codex_path: "codex".into(),
            copilot_path: "copilot".into(),
            max_runtime: Duration::from_secs(1),
            tail_lines: 1,
            sandbox: true,
        });
        let cmd = runner.build_command(Path::new("/bin/sh"), &env, None).unwrap();

        // `sandbox-exec -p <profile> -- <program>`: the profile is the arg after `-p`.
        let args: Vec<String> =
            cmd.as_std().get_args().map(|a| a.to_string_lossy().to_string()).collect();
        let profile = args
            .iter()
            .position(|a| a == "-p")
            .and_then(|i| args.get(i + 1))
            .expect("the confined command must carry an inline -p profile");

        assert!(
            !profile.contains("mach-lookup"),
            "no mach service may be granted — securityd least of all:\n{profile}"
        );
        assert!(
            !profile.contains("SecurityServer"),
            "the Keychain grant must stay gone:\n{profile}"
        );
        assert!(
            !profile.contains("keychain"),
            "no keychain file may be granted:\n{profile}"
        );
        assert!(
            !profile.contains(".credentials.json"),
            "no credential file may be granted:\n{profile}"
        );

        // Nothing under the operator's real home is reachable. The task root is
        // a tempdir, so every granted path is a system root or the task itself.
        if let Some(home) = dirs::home_dir() {
            let home = home.to_string_lossy().to_string();
            assert!(
                !profile.contains(&home),
                "no path under the operator's $HOME ({home}) may be granted:\n{profile}"
            );
        }
    }

    #[test]
    fn compose_child_env_filters_source_to_the_allowlist() {
        // A non-allowlisted ambient var (a leaked secret) is dropped; HOME survives.
        let child = compose_child_env(
            vec![pair("HOME", "/h"), pair("SECRET_KEY", "leak")],
            std::iter::empty(),
        );
        assert!(child.contains(&pair("HOME", "/h")));
        assert!(
            child.iter().all(|(k, _)| k != "SECRET_KEY"),
            "secret filtered"
        );
    }

    #[test]
    fn ambient_claude_token_never_reaches_a_child_but_resolved_injection_does() {
        use ainb_hangar_secrets::{InMemoryBackend, Scope, SecretBackend as _};

        // The daemon's OWN env carries an ambient CLAUDE_CODE_OAUTH_TOKEN (a
        // leaked operator secret). The store holds the resolved credential.
        let mut daemon_env = std::collections::HashMap::new();
        daemon_env.insert("HOME".to_string(), "/h".to_string());
        daemon_env.insert(
            crate::claude_cred::CHILD_ENV_VAR.to_string(),
            "AMBIENT-LEAK".to_string(),
        );
        let store = InMemoryBackend::new();
        store.put(&Scope::Global, crate::claude_cred::SECRET_KEY, b"RESOLVED").unwrap();

        // The child's source_env is the ambient daemon env (as `build_task_env`
        // would filter it). CLAUDE_CODE_OAUTH_TOKEN is NOT on ENV_ALLOWLIST, so
        // `compose_child_env` drops it for EVERY backend.
        let source: Vec<(String, String)> =
            daemon_env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        // Codex / copilot resolve NO credential (backend gate) — their extra_env
        // carries no token, and the ambient one is filtered out. A claude token
        // must never appear in their child env.
        for backend in [Backend::Codex, Backend::Copilot] {
            let extra = crate::claude_cred::keys_for_backend(backend, &store, &daemon_env);
            let child = compose_child_env(source.clone(), extra);
            assert!(
                child.iter().all(|(k, _)| k != crate::claude_cred::CHILD_ENV_VAR),
                "{backend:?} child must carry NO CLAUDE_CODE_OAUTH_TOKEN, got {child:?}"
            );
        }

        // Claude resolves the stored credential and injects it via extra_env
        // (appended after the filter). The child sees the RESOLVED value — never
        // the ambient leak.
        let extra = crate::claude_cred::keys_for_backend(Backend::Claude, &store, &daemon_env);
        let child = compose_child_env(source, extra);
        let tok = child
            .iter()
            .find(|(k, _)| k == crate::claude_cred::CHILD_ENV_VAR)
            .map(|(_, v)| v.as_str());
        assert_eq!(
            tok,
            Some("RESOLVED"),
            "claude child must carry the RESOLVED token, not the ambient leak"
        );
    }

    #[test]
    fn compose_child_env_layers_agent_env_over_ambient() {
        // A per-agent value overrides an allowlisted ambient one of the same name.
        let child = compose_child_env(vec![pair("LANG", "C")], vec![pair("LANG", "en_US.UTF-8")]);
        // The daemon spawn passes the composed Vec to `Command::envs` / `env -i`,
        // both last-wins — so the agent value is the effective one.
        assert_eq!(child.last(), Some(&pair("LANG", "en_US.UTF-8")));
    }

    #[test]
    fn compose_child_env_agent_env_cannot_shadow_the_daemon_parent_stamp() {
        // ccc / D11 regression guard: the daemon stamps AINB_PARENT_SESSION into the
        // allowlisted source env; a per-agent `agent_env` carrying its OWN value must
        // NOT override it (that would drop the hook's fleet-membership resolution).
        let child = compose_child_env(
            vec![pair(
                ainb_fleet_core::session_registry::PARENT_ENV,
                "hangar-daemon",
            )],
            vec![pair(
                ainb_fleet_core::session_registry::PARENT_ENV,
                "agent-hijack",
            )],
        );
        let parent: Vec<&String> = child
            .iter()
            .filter(|(k, _)| k == ainb_fleet_core::session_registry::PARENT_ENV)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(
            parent,
            vec!["hangar-daemon"],
            "the daemon parent stamp must be the ONLY AINB_PARENT_SESSION in the child env"
        );
    }
}
