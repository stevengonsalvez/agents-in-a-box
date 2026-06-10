//! The `ainb hangar <verb>` command namespace.
//!
//! Wires the Hangar managed-agents control plane onto the `ainb` binary's clap
//! tree. Every verb here dispatches to the `ainb-hangar-{store,daemon}` library
//! APIs; this module owns only the **parse + render + bootstrap** glue. Per
//! `reference_rust_bin_lib_split`, the dispatch functions live in this library
//! module (reachable as `ainb::cli::hangar::dispatch`) rather than in
//! `main.rs`, so the binary entry point stays a thin shell and the surface is
//! unit-testable.
//!
//! # Wired verbs (P0–P3 backing impl exists)
//!
//! | path                          | backing impl                                            |
//! |-------------------------------|---------------------------------------------------------|
//! | `hangar issue create`         | [`ainb_hangar_store::repo::issue::IssueRepo::insert`]    |
//! | `hangar issue list`           | `IssueRepo::list_by_workspace_state`                     |
//! | `hangar issue show`           | `IssueRepo::get_by_id`                                   |
//! | `hangar task list`            | `ainb_hangar_store::repo::task::TaskRepo`                |
//! | `hangar task cancel`          | `ainb_hangar_store::service::cancel::CancelTaskService`  |
//! | `hangar task retry`           | `ainb_hangar_store::service::retry::RetryService`        |
//! | `hangar beads reconcile`      | `ainb_hangar_daemon::beads_sync::reconcile`              |
//! | `hangar daemon status`        | [`ainb_hangar_store::Store::open_default`] reachability  |
//!
//! # Deliberately NOT wired
//!
//! `skill`, `autopilot`, `config`, `init`, `tui`, and `daemon start|stop` land
//! in later phases. The [`HangarCommand`](crate::cli::registry) subtree is built
//! with derive `Subcommand`s, so a later phase slots a new verb in by adding a
//! variant — no stubs are shipped for unimplemented verbs today.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::SystemClock;
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::Autopilot;
use ainb_hangar_store::repo::issue::{Issue, IssueRepo, NewIssue};
use ainb_hangar_store::repo::task::{Task, TaskRepo};
use ainb_hangar_store::repo::token::{PatRecord, PatRepo, mint_daemon_token, mint_pat};
use ainb_hangar_store::service::cancel::CancelTaskService;
use ainb_hangar_store::service::retry::{RetryDecision, RetryService};

use crate::cli::OutputFormat;

/// Default workspace bootstrapped when the Hangar database has no workspace yet.
///
/// The CLI is usable standalone (no daemon / TUI onboarding required), so
/// `issue create` lazily lays down a single workspace + owner the first time it
/// runs. Mirrors the single-workspace-at-v1 reality (migration 0001 docs).
const DEFAULT_WORKSPACE_SLUG: &str = "default";
/// Human name of the bootstrapped default workspace.
const DEFAULT_WORKSPACE_NAME: &str = "Default Workspace";
/// Email of the bootstrapped owner user.
const DEFAULT_OWNER_EMAIL: &str = "stevie@local";
/// Member id used as the default issue creator (`member:stevie`).
const DEFAULT_CREATOR_ID: &str = "stevie";
/// Lifecycle state a freshly-created issue lands in.
const DEFAULT_ISSUE_STATE: &str = "open";

/// The `hangar` subcommand tree.
///
/// The single derive enum the registry's [`HangarCommand`](crate::cli::registry)
/// augments onto the root `ainb` command. Each variant is a noun group whose
/// inner enum carries the verbs.
#[derive(Subcommand, Debug)]
pub enum HangarCommand {
    /// Manage Hangar issues.
    #[command(subcommand)]
    Issue(IssueCommand),
    /// Inspect and control Hangar tasks.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Sync Hangar issues with the beads (`bd`) tracker.
    #[command(subcommand)]
    Beads(BeadsCommand),
    /// Inspect the Hangar control-plane daemon.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Manage Hangar auth tokens (PATs + daemon tokens).
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Configure Hangar (env allowlist, …).
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Import + list workspace-scoped skills.
    #[command(subcommand)]
    Skills(SkillsCommand),
    /// List, inspect, and apply curated agent templates.
    #[command(subcommand)]
    Templates(TemplatesCommand),
    /// Edit, archive, and list workspace agents.
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Create and control cron-scheduled autopilots.
    #[command(subcommand)]
    Autopilot(AutopilotCommand),
    /// Read the daemon's structured logs.
    #[command(subcommand)]
    Logs(LogsCommand),
}

/// `hangar logs <verb>`.
///
/// Surfaces the daemon's P8.1 structured-log file
/// (`<hangar_home>/hangar/logs/daemon.<date>`, daily-rotated — never a literal
/// `daemon.jsonl`). `tail` pretty-prints the newest file's recent events,
/// optionally following live or bounded to the last N.
#[derive(Subcommand, Debug)]
pub enum LogsCommand {
    /// Pretty-print recent log events; `--follow` streams live.
    Tail(LogsTailArgs),
}

/// Arguments for `hangar logs tail`.
#[derive(Args, Debug)]
pub struct LogsTailArgs {
    /// Stream new events live as the daemon writes them (poll-append loop).
    #[arg(long, short = 'f')]
    pub follow: bool,
    /// Print the last N events and exit (the bounded tail window).
    #[arg(long, default_value_t = 200)]
    pub lines: usize,
    /// Only show events at or above this level
    /// (`trace`/`debug`/`info`/`warn`/`error`).
    #[arg(long)]
    pub level: Option<String>,
    /// Print + exit even when `--follow` is set (bounded mode for tests/CI).
    #[arg(long)]
    pub no_follow: bool,
}

/// `hangar autopilot <verb>`.
///
/// A cron-scheduled autopilot fires its bound agent on a recurring schedule.
/// `create` validates the cron expression (P7.1) and **rejects it before any
/// row is written** when malformed; `list` shows each autopilot's cron + an
/// enabled/disabled badge + its last run; `disable`/`enable` toggle scheduling
/// (enable recomputes the next tick from now, never replaying missed slots);
/// `run` fires one tick immediately via the P7.4 enqueue path, bypassing the
/// schedule. Every verb is workspace-scoped (the bootstrapped `default`
/// workspace unless `--workspace` is given).
#[derive(Subcommand, Debug)]
pub enum AutopilotCommand {
    /// Create a cron-scheduled autopilot (rejects an invalid cron expression).
    Create(AutopilotCreateArgs),
    /// List the workspace's autopilots (cron, next tick, last run, enabled).
    List(AutopilotListArgs),
    /// Disable an autopilot so the scheduler stops firing it.
    Disable(AutopilotIdArgs),
    /// Re-enable an autopilot, recomputing its next tick from now.
    Enable(AutopilotIdArgs),
    /// Fire one tick immediately (manual run), bypassing the schedule.
    Run(AutopilotIdArgs),
}

/// Arguments for `hangar autopilot create`.
#[derive(Args, Debug)]
pub struct AutopilotCreateArgs {
    /// Name, unique within the workspace.
    #[arg(long)]
    pub name: String,
    /// Cron expression (UTC, 5-field) — validated before insert.
    #[arg(long)]
    pub cron: String,
    /// Agent id to dispatch to at each tick (`agent.id`).
    #[arg(long)]
    pub agent: String,
    /// Optional instructions handed to the agent on every tick.
    #[arg(long)]
    pub instructions: Option<String>,
    /// Maximum simultaneous in-flight runs before a tick is skipped.
    #[arg(long = "max-concurrent-runs", default_value_t = 1)]
    pub max_concurrent_runs: i64,
    /// Workspace slug to create in. Defaults to the bootstrapped `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar autopilot list`.
#[derive(Args, Debug)]
pub struct AutopilotListArgs {
    /// Workspace slug to list. Defaults to the bootstrapped `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for the id-only autopilot verbs (`disable`, `enable`, `run`).
#[derive(Args, Debug)]
pub struct AutopilotIdArgs {
    /// The autopilot id (`autopilot.id`).
    pub id: String,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar agent <verb>`.
///
/// The general agent edit/archive surface (e38.15): `templates use` is the only
/// way to *create* an agent, but this group lets the operator EDIT one's config
/// knobs (model / CLI args / MCP / thinking / per-agent env), ARCHIVE it (hide it
/// from the active picker without a hard delete), un-archive it, and LIST the
/// workspace's agents (active by default, `--all` includes archived). Every verb
/// is workspace-scoped (`--workspace`, else the bootstrapped `default`).
///
/// Persists + exposes the config only; the provider EXEC consumption of
/// `model`/`args` is a separate concern (e38.16).
#[derive(Subcommand, Debug)]
pub enum AgentCommand {
    /// List the workspace's agents (active by default; `--all` includes archived).
    List(AgentListArgs),
    /// Edit an agent's config knobs (model / args / MCP / thinking / env / name).
    Edit(AgentEditArgs),
    /// Archive an agent (hide it from the active picker).
    Archive(AgentArchiveArgs),
    /// Un-archive an agent (restore it to the active picker).
    Unarchive(AgentArchiveArgs),
}

/// Arguments for `hangar agent list`.
#[derive(Args, Debug)]
pub struct AgentListArgs {
    /// Include archived agents in the listing (default: active only).
    #[arg(long)]
    pub all: bool,
    /// Workspace slug to list. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar agent edit`.
///
/// Edits a subset of one agent's mutable config; every field is optional and an
/// omitted field is left unchanged. The four nullable text fields each have an
/// explicit "clear" flag (`--clear-model`, `--clear-mcp`, `--clear-thinking`,
/// `--clear-instructions`) so the caller can distinguish "leave as-is" from "set
/// back to none". `--arg` and `--env` are repeatable and REPLACE the whole list
/// when any is given (they are not append-to-existing). The edit is
/// workspace-scoped: an agent id outside `--workspace` touches no row.
// The four `--clear-*` flags are the deliberate CLI shape (one per nullable
// field), not an over-boolean design smell.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args, Debug)]
pub struct AgentEditArgs {
    /// Agent id (ULID) to edit.
    pub id: String,
    /// Rename the agent; omitted leaves the name.
    #[arg(long)]
    pub name: Option<String>,
    /// New instructions; omitted leaves them. Mutually exclusive with
    /// `--clear-instructions`.
    #[arg(long, conflicts_with = "clear_instructions")]
    pub instructions: Option<String>,
    /// Clear the instructions; omitted leaves them.
    #[arg(long = "clear-instructions")]
    pub clear_instructions: bool,
    /// New model override (e.g. `claude-opus-4`); omitted leaves it. Mutually
    /// exclusive with `--clear-model`.
    #[arg(long, conflicts_with = "clear_model")]
    pub model: Option<String>,
    /// Clear the model override (back to the provider default); omitted leaves it.
    #[arg(long = "clear-model")]
    pub clear_model: bool,
    /// A CLI arg to pass the provider (repeatable: `--arg --verbose --arg -x`).
    /// When ANY `--arg` is given the whole arg list is REPLACED with the values.
    #[arg(long = "arg", action = clap::ArgAction::Append)]
    pub args: Vec<String>,
    /// New MCP config as a raw JSON-object string; omitted leaves it. Mutually
    /// exclusive with `--clear-mcp`.
    #[arg(long = "mcp", conflicts_with = "clear_mcp")]
    pub mcp: Option<String>,
    /// Clear the MCP config; omitted leaves it.
    #[arg(long = "clear-mcp")]
    pub clear_mcp: bool,
    /// New thinking level (e.g. `low`/`medium`/`high`); omitted leaves it.
    /// Mutually exclusive with `--clear-thinking`.
    #[arg(long, conflicts_with = "clear_thinking")]
    pub thinking: Option<String>,
    /// Clear the thinking level; omitted leaves it.
    #[arg(long = "clear-thinking")]
    pub clear_thinking: bool,
    /// A `KEY=VALUE` env var for the agent (repeatable). When ANY `--env` is
    /// given the whole env map is REPLACED with the values.
    #[arg(long = "env", value_parser = parse_env_kv, action = clap::ArgAction::Append)]
    pub env: Vec<(String, String)>,
    /// Workspace slug the agent belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar agent archive` / `hangar agent unarchive`.
#[derive(Args, Debug)]
pub struct AgentArchiveArgs {
    /// Agent id (ULID) to (un)archive.
    pub id: String,
    /// Workspace slug the agent belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Parse a `--env KEY=VALUE` argument into a `(key, value)` pair.
///
/// # Errors
///
/// Returns a human-readable message if the input has no `=` or an empty key.
fn parse_env_kv(raw: &str) -> Result<(String, String), String> {
    let (key, value) =
        raw.split_once('=').ok_or_else(|| format!("expected KEY=VALUE, got {raw:?}"))?;
    if key.is_empty() {
        return Err(format!("env var name must not be empty in {raw:?}"));
    }
    Ok((key.to_string(), value.to_string()))
}

/// `hangar templates <verb>`.
///
/// `list` and `show` read the curated templates baked into the binary (no
/// database). `use` materialises a template into a live agent + its skill
/// attachments in the target workspace (the skills must already be imported via
/// `hangar skills sync`).
#[derive(Subcommand, Debug)]
pub enum TemplatesCommand {
    /// List every embedded curated template.
    List,
    /// Show one template in full (instructions + skill list).
    Show(TemplatesShowArgs),
    /// Create an agent from a template, attaching its bundled skills.
    Use(TemplatesUseArgs),
}

/// Arguments for `hangar templates show`.
#[derive(Args, Debug)]
pub struct TemplatesShowArgs {
    /// Template name (e.g. `code-reviewer`).
    pub name: String,
}

/// Arguments for `hangar templates use`.
#[derive(Args, Debug)]
pub struct TemplatesUseArgs {
    /// Template name to apply (e.g. `code-reviewer`).
    pub name: String,
    /// Workspace slug to create the agent in. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Name the created agent something other than the template name.
    #[arg(long = "agent-name")]
    pub agent_name: Option<String>,
}

/// `hangar skills <verb>`.
///
/// `sync` imports a `toolkit/packages/skills/`-shaped directory into the default
/// workspace (idempotent on `(workspace, name)`); `list` shows the imported
/// skills. Both are workspace-scoped via `--workspace`.
#[derive(Subcommand, Debug)]
pub enum SkillsCommand {
    /// Import skills from a toolkit directory into a workspace (idempotent).
    Sync(SkillsSyncArgs),
    /// List the skills imported into a workspace.
    List(SkillsListArgs),
}

/// Arguments for `hangar skills sync`.
#[derive(Args, Debug)]
pub struct SkillsSyncArgs {
    /// Workspace slug to import into. Defaults to the bootstrapped `default`
    /// workspace.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Source directory holding `<name>/SKILL.md` skill dirs. Defaults to
    /// `$AINB_TOOLKIT_SKILLS_DIR`, else a walk up to `toolkit/packages/skills`.
    #[arg(long)]
    pub source: Option<std::path::PathBuf>,
    /// Print the skills that would be imported without writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// Arguments for `hangar skills list`.
#[derive(Args, Debug)]
pub struct SkillsListArgs {
    /// Workspace slug to list. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar config <noun>`.
#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Manage the provider-subprocess env allowlist.
    #[command(name = "env.allow", subcommand)]
    EnvAllow(EnvAllowCommand),
    /// Manage danger-full-access warning acknowledgements.
    #[command(subcommand)]
    Warnings(WarningsCommand),
}

/// `hangar config warnings <verb>`.
///
/// The danger-full-access warnings are shown once on first run and once per
/// provider per session. `reset` wipes the recorded acknowledgements so the
/// warning is shown again — useful after handing a machine to a new user, or to
/// re-confirm a specific provider.
#[derive(Subcommand, Debug)]
pub enum WarningsCommand {
    /// Clear recorded warning acks so they show again.
    ///
    /// With no flag, wipes every ack (first-run + all per-provider sessions).
    /// With `--provider <name>`, wipes only that provider's per-session acks so
    /// its next dispatch re-warns, leaving first-run + other providers intact.
    Reset(WarningsResetArgs),
}

/// Arguments for `hangar config warnings reset`.
#[derive(Args, Debug)]
pub struct WarningsResetArgs {
    /// Reset only this provider's per-session acks (e.g. `claude`). Omit to
    /// reset every warning ack.
    #[arg(long)]
    pub provider: Option<String>,
}

/// `hangar config env.allow <verb>`.
///
/// The allowlist governs which *ambient* env vars pass through to a provider
/// subprocess. A hardcoded code-injection deny family (`LD_PRELOAD`,
/// `DYLD_INSERT_LIBRARIES`, …) always overrides — `add`ing one of those is
/// rejected before it ever reaches the file.
#[derive(Subcommand, Debug)]
pub enum EnvAllowCommand {
    /// Show the merged effective allowlist (`[deny-locked]` marks deny entries).
    List,
    /// Add an env-var name (or `*`-suffix glob) to the allowlist.
    Add(EnvAllowKeyArgs),
    /// Remove an env-var name from the allowlist.
    Remove(EnvAllowKeyArgs),
}

/// Arguments for `hangar config env.allow add|remove`.
#[derive(Args, Debug)]
pub struct EnvAllowKeyArgs {
    /// The env-var name (e.g. `FOO_BAR`) or `*`-suffix glob (e.g. `MY_*`).
    pub key: String,
}

/// `hangar auth <verb>`.
#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Manage personal access tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// Manage daemon-to-daemon tokens (advanced).
    #[command(subcommand, hide = true)]
    DaemonToken(DaemonTokenCommand),
}

/// `hangar auth token <verb>`.
#[derive(Subcommand, Debug)]
pub enum TokenCommand {
    /// Mint a new PAT. Prints the plaintext **once** — it is never recoverable.
    Create(TokenCreateArgs),
    /// List this user's PATs (id, scope, timestamps — never the plaintext).
    List,
    /// Revoke a PAT by id.
    Revoke(TokenRevokeArgs),
}

/// Permitted scopes for a freshly minted PAT.
///
/// Stored as the opaque `pat.scope` column; the store treats it as a plain
/// string, the CLI constrains it to this closed set.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenScope {
    /// Read-only access.
    Read,
    /// Read + write access.
    Write,
    /// Full administrative access.
    Admin,
}

impl TokenScope {
    /// The string persisted into `pat.scope`.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

/// Arguments for `hangar auth token create`.
#[derive(Args, Debug)]
pub struct TokenCreateArgs {
    /// Access scope to grant the token.
    #[arg(long, value_enum, default_value_t = TokenScope::Read)]
    pub scope: TokenScope,
    /// Human-readable label (recorded as the token's scope context; advisory).
    #[arg(long)]
    pub name: Option<String>,
}

/// Arguments for `hangar auth token revoke`.
#[derive(Args, Debug)]
pub struct TokenRevokeArgs {
    /// PAT id to revoke.
    pub id: String,
}

/// `hangar auth daemon-token <verb>`.
///
/// Hidden from `--help` (the daemon-to-daemon path is advanced/future); the
/// verbs still parse and run.
#[derive(Subcommand, Debug)]
pub enum DaemonTokenCommand {
    /// Mint a daemon token bound to a runtime. Prints the plaintext once.
    Create(DaemonTokenCreateArgs),
}

/// Arguments for `hangar auth daemon-token create`.
#[derive(Args, Debug)]
pub struct DaemonTokenCreateArgs {
    /// Runtime id (`agent_runtime.id`) the token is bound to.
    #[arg(long = "runtime-id")]
    pub runtime_id: String,
}

/// `hangar issue <verb>`.
#[derive(Subcommand, Debug)]
pub enum IssueCommand {
    /// Create a new issue (bootstraps a default workspace on first use).
    Create(IssueCreateArgs),
    /// List issues in the default workspace.
    List(IssueListArgs),
    /// Show one issue by id.
    Show(IssueShowArgs),
    /// Edit an existing issue's state, assignee, priority, or due date.
    Update(IssueUpdateArgs),
}

/// Arguments for `hangar issue update`.
///
/// Edits a subset of one issue's mutable fields; every field is optional and an
/// omitted field is left unchanged. The two nullable fields have an explicit
/// "clear" flag (`--unassign`, `--clear-due`) so the caller can distinguish
/// "leave as-is" from "set back to none". The edit is workspace-scoped: a
/// `--workspace` selects the tenant (default: the bootstrapped `default`), and
/// an issue id outside it touches no row.
#[derive(Args, Debug)]
pub struct IssueUpdateArgs {
    /// Issue id (ULID) to edit.
    pub id: String,
    /// New lifecycle state (e.g. `in_progress`, `done`); omitted leaves it.
    #[arg(long)]
    pub state: Option<String>,
    /// Reassign the issue to an agent (`agent.id`); omitted leaves the assignee.
    ///
    /// Mutually exclusive with `--unassign`.
    #[arg(long, conflicts_with = "unassign")]
    pub assign: Option<String>,
    /// Clear the assignee (unassign the issue); omitted leaves it.
    #[arg(long)]
    pub unassign: bool,
    /// New urgency 0..3 (P3..P0, HIGHER = MORE URGENT); omitted leaves it.
    #[arg(long, value_parser = clap::value_parser!(i64).range(0..=3))]
    pub priority: Option<i64>,
    /// New due date as `YYYY-MM-DD` (UTC midnight); omitted leaves it.
    ///
    /// Mutually exclusive with `--clear-due`.
    #[arg(long, value_parser = parse_due_date, conflicts_with = "clear_due")]
    pub due: Option<i64>,
    /// Clear the due date (remove the deadline); omitted leaves it.
    #[arg(long = "clear-due")]
    pub clear_due: bool,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue create`.
#[derive(Args, Debug)]
pub struct IssueCreateArgs {
    /// Issue title.
    #[arg(long)]
    pub title: String,
    /// Free-form description.
    #[arg(long)]
    pub description: Option<String>,
    /// Initial lifecycle state.
    #[arg(long, default_value = DEFAULT_ISSUE_STATE)]
    pub state: String,
    /// Assign the issue to an agent (`agent.id`) and enqueue a task for it.
    ///
    /// When set, the issue's assignee is the agent and a `queued` task is
    /// enqueued for the agent's runtime, so the daemon's claim loop picks it up,
    /// materialises the agent's attached skills (P6.4), and dispatches the
    /// provider. The created task id is printed alongside the issue id.
    #[arg(long)]
    pub assign: Option<String>,
    /// Urgency: 0..3 mapping P3..P0 — HIGHER = MORE URGENT (default 0).
    ///
    /// Stamped onto BOTH the created issue and (when `--assign` enqueues one) the
    /// task: the daemon's claim loop drains `priority DESC, created_at, id`
    /// (Multica ordering parity), so a higher value jumps the queue while equal
    /// priorities stay FIFO.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(i64).range(0..=3))]
    pub priority: i64,
    /// Optional due date as `YYYY-MM-DD` (interpreted at UTC midnight).
    ///
    /// Persisted onto the issue as an epoch-millisecond deadline; omitted leaves
    /// the issue with no due date.
    #[arg(long, value_parser = parse_due_date)]
    pub due: Option<i64>,
    /// A label to attach to the issue (repeatable: `--label bug --label p0`).
    ///
    /// Persisted as the issue's label list. The full labels table + attach/detach
    /// is a separate concern; create just records the labels it is handed.
    #[arg(long = "label", action = clap::ArgAction::Append)]
    pub labels: Vec<String>,
}

/// Parse a `--due` value (`YYYY-MM-DD`) into an epoch-millisecond timestamp at
/// UTC midnight.
///
/// # Errors
///
/// Returns a human-readable message if the input is not a valid `YYYY-MM-DD`
/// date (surfaced by clap as the flag's value error).
fn parse_due_date(raw: &str) -> Result<i64, String> {
    let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| format!("expected a YYYY-MM-DD date, got {raw:?}"))?;
    let dt = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| format!("invalid time-of-day for date {raw:?}"))?
        .and_utc();
    Ok(dt.timestamp_millis())
}

/// Arguments for `hangar issue list`.
#[derive(Args, Debug)]
pub struct IssueListArgs {
    /// Restrict to issues in this lifecycle state.
    #[arg(long, default_value = DEFAULT_ISSUE_STATE)]
    pub state: String,
}

/// Arguments for `hangar issue show`.
#[derive(Args, Debug)]
pub struct IssueShowArgs {
    /// Issue id (ULID).
    pub id: String,
}

/// `hangar task <verb>`.
#[derive(Subcommand, Debug)]
pub enum TaskCommand {
    /// List pending (queued / dispatched) tasks.
    List(TaskListArgs),
    /// Cancel a task (`{queued|dispatched|running} -> cancelled`).
    Cancel(TaskIdArgs),
    /// Spawn a retry child for a retryable failed task.
    Retry(TaskIdArgs),
}

/// Arguments for `hangar task list`.
#[derive(Args, Debug)]
pub struct TaskListArgs {
    /// Restrict to a single runtime id. When omitted, every runtime in the
    /// default workspace is scanned.
    #[arg(long)]
    pub runtime: Option<String>,
}

/// Arguments for the task verbs that take a single id (`cancel`, `retry`).
#[derive(Args, Debug)]
pub struct TaskIdArgs {
    /// Task id (ULID).
    pub id: String,
}

/// `hangar beads <verb>`.
#[derive(Subcommand, Debug)]
pub enum BeadsCommand {
    /// Walk the mapping table and repair Hangar <-> bd drift.
    Reconcile(BeadsReconcileArgs),
}

/// Arguments for `hangar beads reconcile`.
///
/// Mirrors `ainb_hangar_daemon::beads_sync::reconcile::cli::ReconcileArgs`
/// 1:1; we re-declare them here (rather than re-using the daemon's clap type)
/// because the daemon's `format` enum is its own `OutputFormat`, and host-side
/// the verb plugs into the shared dispatch path. The fields round-trip into the
/// daemon's args inside [`run_beads_reconcile`].
#[derive(Args, Debug, Clone)]
pub struct BeadsReconcileArgs {
    /// Diff only — report drift without writing either side.
    #[arg(long)]
    pub dry_run: bool,
    /// Restrict to bd issues carrying this label (repeatable).
    #[arg(long = "label")]
    pub label: Vec<String>,
    /// Emit the reconcile report as JSON instead of a summary line.
    #[arg(long)]
    pub json: bool,
}

/// `hangar daemon <verb>`.
#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Report whether the Hangar database is reachable and migrated.
    Status,
}

/// Dispatch a parsed [`HangarCommand`] to its backing service.
///
/// This is the single entry point `main.rs` (via the registry) calls. The
/// `format` is the global `ainb --format` selection; only the structured verbs
/// (`issue list`, `issue show`, `task list`) honour `Json` today — the rest
/// print a human line.
///
/// # Errors
///
/// Propagates any backing-store, service, or IO error. Bootstrapping the
/// default workspace, opening the store, and every repo / service call can fail.
pub async fn dispatch(cmd: HangarCommand, format: OutputFormat) -> Result<()> {
    match cmd {
        HangarCommand::Issue(c) => dispatch_issue(c, format).await,
        HangarCommand::Task(c) => dispatch_task(c, format).await,
        HangarCommand::Beads(BeadsCommand::Reconcile(args)) => run_beads_reconcile(args).await,
        HangarCommand::Daemon(DaemonCommand::Status) => run_daemon_status().await,
        HangarCommand::Auth(c) => dispatch_auth(c, format).await,
        HangarCommand::Config(c) => dispatch_config(c, format),
        HangarCommand::Skills(c) => dispatch_skills(c, format).await,
        HangarCommand::Templates(c) => dispatch_templates(c, format).await,
        HangarCommand::Agent(c) => dispatch_agent(c, format).await,
        HangarCommand::Autopilot(c) => dispatch_autopilot(c, format).await,
        HangarCommand::Logs(LogsCommand::Tail(args)) => run_logs_tail(args).await,
    }
}

/// `hangar logs tail`: pretty-print the daemon's structured-log events.
///
/// Resolves the log dir the daemon writes (`<hangar_home>/hangar/logs`) via the
/// shared [`ainb_hangar_core::logs::default_log_dir`], reads the newest
/// `daemon.<date>` file's last `--lines` events (the file is daily-rotated —
/// never a literal `daemon.jsonl`), applies the optional `--level` floor, and
/// prints each as `{ts} {LVL} {target} {msg} {k=v}` coloured by level. A
/// missing log dir / file prints nothing and exits 0 (a fresh install with no
/// daemon run yet is not an error).
///
/// With `--follow`/`-f` (and not `--no-follow`) it then polls the file for
/// appended lines and prints them as they arrive, exiting cleanly on Ctrl-C.
async fn run_logs_tail(args: LogsTailArgs) -> Result<()> {
    use ainb_hangar_core::logs::{self, LogLevel};

    let min_level = match &args.level {
        Some(s) => Some(LogLevel::parse(s).with_context(|| {
            format!("unknown log level `{s}` (use trace/debug/info/warn/error)")
        })?),
        None => None,
    };

    let dir = logs::default_log_dir().context("resolve hangar log dir")?;

    // Bounded pass: print the last `--lines` events (chronological).
    let tail = logs::read_tail(&dir, args.lines, min_level);
    for line in &tail {
        println!("{}", logs_pretty_line(line));
    }

    // Follow loop: poll the newest file for appended lines and print them as
    // they arrive. `--no-follow` short-circuits (the bounded tail above is the
    // whole output) so tests + CI never hang on the loop.
    if args.follow && !args.no_follow {
        follow_logs(&dir, min_level).await?;
    }
    Ok(())
}

/// Poll the newest `daemon.*` file for appended lines, printing each new event.
///
/// Tracks how many lines have already been emitted and re-reads on a fixed
/// interval, printing only the tail past the watermark. A daily rotation that
/// swaps in a newer file resets the watermark (the new file starts fresh). Runs
/// until the process is interrupted (Ctrl-C), which Tokio delivers as a
/// `ctrl_c` signal that exits the loop cleanly.
async fn follow_logs(
    dir: &std::path::Path,
    min_level: Option<ainb_hangar_core::logs::LogLevel>,
) -> Result<()> {
    use ainb_hangar_core::logs;
    use std::time::Duration;

    /// Poll cadence for the follow loop.
    const POLL: Duration = Duration::from_millis(500);

    let mut current = logs::log_files_newest_first(dir).into_iter().next();
    let mut emitted = current.as_deref().map_or(0, |p| {
        std::fs::read_to_string(p).map(|c| c.lines().count()).unwrap_or(0)
    });

    loop {
        tokio::select! {
            // Clean exit on Ctrl-C.
            res = tokio::signal::ctrl_c() => {
                res.context("listen for ctrl-c")?;
                return Ok(());
            }
            () = tokio::time::sleep(POLL) => {
                let newest = logs::log_files_newest_first(dir).into_iter().next();
                // A daily rotation swapped in a newer file: reset the watermark.
                if newest != current {
                    current = newest;
                    emitted = 0;
                }
                let Some(path) = current.as_deref() else {
                    continue;
                };
                let Ok(contents) = std::fs::read_to_string(path) else {
                    continue;
                };
                let all: Vec<&str> = contents.lines().collect();
                if all.len() > emitted {
                    for raw in &all[emitted..] {
                        if let Some(line) = logs::LogLine::parse(raw) {
                            if line.passes_level(min_level) {
                                println!("{}", logs_pretty_line(&line));
                            }
                        }
                    }
                    emitted = all.len();
                }
            }
        }
    }
}

/// Pretty-print one log event as `{ts} {LVL} {target} {msg} {k=v…}`, with the
/// level token coloured by severity via crossterm (a workspace dep; ANSI is
/// auto-suppressed when stdout is not a TTY). Missing fields are simply skipped
/// — the printer never panics on a partial event (the P8.6 acceptance criterion).
fn logs_pretty_line(line: &ainb_hangar_core::logs::LogLine) -> String {
    use ainb_hangar_core::logs::LogLevel;
    use crossterm::style::Stylize;

    let mut parts: Vec<String> = Vec::new();
    if !line.timestamp.is_empty() {
        parts.push(line.timestamp.clone());
    }
    if !line.level.is_empty() {
        // Colour the level token by severity; non-TTY stdout drops the codes.
        let colored = match line.level_enum() {
            Some(LogLevel::Info) => line.level.clone().blue().to_string(),
            Some(LogLevel::Warn) => line.level.clone().yellow().to_string(),
            Some(LogLevel::Error) => line.level.clone().red().to_string(),
            Some(LogLevel::Debug | LogLevel::Trace) => line.level.clone().dark_grey().to_string(),
            None => line.level.clone(),
        };
        parts.push(colored);
    }
    if !line.target.is_empty() {
        parts.push(line.target.clone());
    }
    if !line.message.is_empty() {
        parts.push(line.message.clone());
    }
    for (k, v) in &line.fields {
        parts.push(format!("{k}={v}"));
    }
    parts.join(" ")
}

/// Dispatch the `hangar autopilot` verbs.
///
/// Opens the store, resolves the workspace the same way the skills/templates
/// verbs do, and drives the workspace-scoped [`AutopilotRepo`]. `create` rejects
/// an invalid cron expression *before* any insert (the repo validates first);
/// `run` fires one tick immediately through the P7.4 enqueue path.
async fn dispatch_autopilot(cmd: AutopilotCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        AutopilotCommand::Create(args) => run_autopilot_create(&store, args).await,
        AutopilotCommand::List(args) => run_autopilot_list(&store, args, format).await,
        AutopilotCommand::Disable(args) => run_autopilot_set_enabled(&store, args, false).await,
        AutopilotCommand::Enable(args) => run_autopilot_set_enabled(&store, args, true).await,
        AutopilotCommand::Run(args) => run_autopilot_run_now(&store, args).await,
    }
}

/// `hangar autopilot create`: validate the cron (rejecting before insert) then
/// persist a workspace-scoped autopilot.
async fn run_autopilot_create(store: &Store, args: AutopilotCreateArgs) -> Result<()> {
    use ainb_hangar_core::ids::{AgentId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let agent = AgentId::from_str(args.agent).context("agent id was empty")?;

    let req = NewAutopilot {
        workspace_id: ws,
        agent_id: agent,
        name: args.name.clone(),
        instructions: args.instructions,
        cron_expr: args.cron.clone(),
        max_concurrent_runs: args.max_concurrent_runs,
    };

    let id = AutopilotRepo::create(store.pool(), &SystemClock, &req)
        .await
        .with_context(|| format!("create autopilot `{}` (cron `{}`)", args.name, args.cron))?;
    println!(
        "created autopilot {id} `{}` (cron `{}`)",
        args.name, args.cron
    );
    Ok(())
}

/// `hangar autopilot list`: list the workspace's autopilots with their last run.
async fn run_autopilot_list(
    store: &Store,
    args: AutopilotListArgs,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::AutopilotRepo;

    // A missing/empty workspace lists as no autopilots, not an error (mirrors
    // skills list).
    let workspace_id = match args.workspace.as_deref() {
        Some(slug) => {
            let id: Option<String> = sqlx::query_scalar("SELECT id FROM workspace WHERE slug = ?")
                .bind(slug)
                .fetch_optional(store.pool())
                .await
                .context("look up workspace by slug")?;
            id
        }
        None => find_default_workspace(store).await?,
    };
    let Some(workspace_id) = workspace_id else {
        render_autopilot_list(&[], format);
        return Ok(());
    };
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let autopilots = AutopilotRepo::list(store.pool(), &ws).await.context("list autopilots")?;

    // Pair each autopilot with its most-recent run's status (the "last run"
    // column), looked up workspace-scoped through the repo's run history.
    let mut rows = Vec::with_capacity(autopilots.len());
    for ap in autopilots {
        let last_run = {
            let id = AutopilotId::from_str(ap.id.clone()).context("autopilot id was empty")?;
            AutopilotRepo::list_runs(store.pool(), &ws, &id, 1)
                .await
                .context("list autopilot runs")?
                .into_iter()
                .next()
                .map(|r| r.status)
        };
        rows.push((ap, last_run));
    }
    render_autopilot_list(&rows, format);
    Ok(())
}

/// `hangar autopilot disable|enable`: toggle scheduling, workspace-scoped.
async fn run_autopilot_set_enabled(
    store: &Store,
    args: AutopilotIdArgs,
    enabled: bool,
) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::AutopilotRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;

    if enabled {
        AutopilotRepo::enable(store.pool(), &SystemClock, &ws, &id)
            .await
            .with_context(|| format!("enable autopilot `{}`", args.id))?;
        println!("enabled autopilot {}", args.id);
    } else {
        AutopilotRepo::disable(store.pool(), &ws, &id)
            .await
            .with_context(|| format!("disable autopilot `{}`", args.id))?;
        println!("disabled autopilot {}", args.id);
    }
    Ok(())
}

/// `hangar autopilot run <id>`: fire one tick immediately via the P7.4 enqueue
/// path, bypassing the schedule. Workspace-scoped: a foreign id is rejected.
async fn run_autopilot_run_now(store: &Store, args: AutopilotIdArgs) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::AutopilotRepo;
    use ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;

    let autopilot = AutopilotRepo::get(store.pool(), &ws, &id)
        .await
        .context("look up autopilot")?
        .with_context(|| format!("no autopilot `{}` in this workspace", args.id))?;

    let (run_id, task_id) = fire_autopilot_tick(store.pool(), &SystemClock, &autopilot)
        .await
        .with_context(|| format!("fire autopilot `{}`", args.id))?;
    println!("fired autopilot {} → run {run_id} task {task_id}", args.id);
    Ok(())
}

/// Dispatch the `hangar templates` verbs.
///
/// `list` and `show` are pure reads of the embedded registry (no database).
/// `use` opens the store, resolves the workspace, and materialises the template
/// transactionally via [`ainb_hangar_daemon::templates::templates_use`].
async fn dispatch_templates(cmd: TemplatesCommand, format: OutputFormat) -> Result<()> {
    match cmd {
        TemplatesCommand::List => {
            run_templates_list(format);
            Ok(())
        }
        TemplatesCommand::Show(args) => run_templates_show(args, format),
        TemplatesCommand::Use(args) => {
            let store = Store::open_default().await.context("open hangar database")?;
            run_templates_use(&store, args).await
        }
    }
}

/// `hangar templates list`: print every embedded curated template.
fn run_templates_list(format: OutputFormat) {
    let templates = ainb_hangar_core::template::TemplateRegistry::list();
    render_template_list(&templates, format);
}

/// `hangar templates show <name>`: dump one template in full.
fn run_templates_show(args: TemplatesShowArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::template::TemplateRegistry;
    let template = TemplateRegistry::get(&args.name)
        .with_context(|| format!("no curated template named `{}`", args.name))?;
    match format {
        OutputFormat::Json => println!("{}", template_to_json(&template)),
        OutputFormat::Csv => {
            println!("{}", template_csv_header());
            println!("{}", template_csv_row(&template));
        }
        OutputFormat::Markdown => {
            print!("{}", template_md_header());
            println!("{}", template_md_row(&template));
        }
        OutputFormat::Text => print!("{}", template_detail(&template)),
    }
    Ok(())
}

/// `hangar templates use <name>`: create an agent from a template + attach its
/// skills, transactionally. Resolves the workspace the same way the skills verbs
/// do (named slug, else the bootstrapped `default`).
async fn run_templates_use(store: &Store, args: TemplatesUseArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_daemon::templates::templates_use;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;

    let outcome = templates_use(store.pool(), &ws, &args.name, args.agent_name.as_deref())
        .await
        .with_context(|| format!("apply template `{}`", args.name))?;

    if outcome.created {
        println!(
            "created agent {} from template `{}` with {} skill(s)",
            outcome.agent_id,
            args.name,
            outcome.skill_ids.len()
        );
    } else {
        println!(
            "agent {} already exists for template `{}` (no change)",
            outcome.agent_id, args.name
        );
    }
    Ok(())
}

/// Dispatch the `hangar agent` verbs (e38.15).
///
/// Opens the store, resolves the workspace the same way the skills/templates
/// verbs do, and drives the workspace-scoped [`AgentRepo`]. `edit` maps the
/// present flags onto an [`AgentConfigUpdate`] (rejecting an empty edit);
/// `archive`/`unarchive` flip the archived flag; `list` shows the workspace's
/// agents (active by default, `--all` includes archived).
async fn dispatch_agent(cmd: AgentCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        AgentCommand::List(args) => run_agent_list(&store, args, format).await,
        AgentCommand::Edit(args) => run_agent_edit(&store, args).await,
        AgentCommand::Archive(args) => run_agent_set_archived(&store, args, true).await,
        AgentCommand::Unarchive(args) => run_agent_set_archived(&store, args, false).await,
    }
}

/// `hangar agent list`: list the workspace's agents (active, or all with `--all`).
async fn run_agent_list(store: &Store, args: AgentListArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_store::repo::agent::AgentRepo;

    // A missing/empty workspace lists as no agents, not an error (mirrors skills).
    let workspace_id = match args.workspace.as_deref() {
        Some(slug) => {
            let id: Option<String> = sqlx::query_scalar("SELECT id FROM workspace WHERE slug = ?")
                .bind(slug)
                .fetch_optional(store.pool())
                .await
                .context("look up workspace by slug")?;
            id
        }
        None => find_default_workspace(store).await?,
    };
    let Some(workspace_id) = workspace_id else {
        render_agent_list(&[], format);
        return Ok(());
    };
    let agents = if args.all {
        AgentRepo::list_by_workspace_including_archived(store.pool(), &workspace_id).await
    } else {
        AgentRepo::list_by_workspace(store.pool(), &workspace_id).await
    }
    .context("list agents")?;
    render_agent_list(&agents, format);
    Ok(())
}

/// `hangar agent edit`: map the present flags onto an [`AgentConfigUpdate`] and
/// drive the workspace-scoped edit. An empty edit (no field flag) is rejected;
/// an agent id outside the workspace is reported as a not-found error.
async fn run_agent_edit(store: &Store, args: AgentEditArgs) -> Result<()> {
    use ainb_hangar_store::repo::agent::{AgentConfigUpdate, AgentRepo};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;

    // Each nullable text field uses its clear-flag to distinguish "clear to none"
    // from "leave unchanged" (a clap conflict already bars setting both).
    let instructions = clear_or_set(args.clear_instructions, args.instructions);
    let model = clear_or_set(args.clear_model, args.model);
    let mcp_config = clear_or_set(args.clear_mcp, args.mcp);
    let thinking = clear_or_set(args.clear_thinking, args.thinking);
    // `--arg` / `--env` REPLACE the list when any value is given (an empty Vec
    // means "no flag passed" → leave unchanged).
    let cli_args = (!args.args.is_empty()).then_some(args.args);
    let agent_env = (!args.env.is_empty()).then_some(args.env);

    let update = AgentConfigUpdate {
        name: args.name,
        instructions,
        model,
        cli_args,
        mcp_config,
        thinking,
        agent_env,
    };

    if update.is_empty() {
        anyhow::bail!(
            "nothing to update: pass at least one of --name / --instructions / --clear-instructions \
             / --model / --clear-model / --arg / --mcp / --clear-mcp / --thinking / --clear-thinking \
             / --env"
        );
    }

    let touched = AgentRepo::update_config(store.pool(), &workspace_id, &args.id, &update)
        .await
        .with_context(|| format!("update agent {}", args.id))?;
    if touched {
        println!("updated agent {}", args.id);
    } else {
        anyhow::bail!("no agent with id {} in this workspace", args.id);
    }
    Ok(())
}

/// `hangar agent archive|unarchive`: flip the archived flag, workspace-scoped.
async fn run_agent_set_archived(
    store: &Store,
    args: AgentArchiveArgs,
    archived: bool,
) -> Result<()> {
    use ainb_hangar_store::repo::agent::AgentRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let touched = AgentRepo::set_archived(store.pool(), &workspace_id, &args.id, archived)
        .await
        .with_context(|| format!("archive agent {}", args.id))?;
    if touched {
        let verb = if archived { "archived" } else { "un-archived" };
        println!("{verb} agent {}", args.id);
    } else {
        anyhow::bail!("no agent with id {} in this workspace", args.id);
    }
    Ok(())
}

/// Collapse a `(clear_flag, optional_value)` pair into the store's nested-`Option`
/// three-state: the clear flag wins (`Some(None)`), else a present value sets
/// (`Some(Some(v))`), else leave unchanged (`None`). The clap `conflicts_with`
/// already bars both at once.
#[allow(clippy::option_option)] // the nested Option IS the store's 3-state encoding
fn clear_or_set(clear: bool, value: Option<String>) -> Option<Option<String>> {
    if clear { Some(None) } else { value.map(Some) }
}

/// Dispatch the `hangar skills` verbs.
async fn dispatch_skills(cmd: SkillsCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        SkillsCommand::Sync(args) => run_skills_sync(&store, args).await,
        SkillsCommand::List(args) => run_skills_list(&store, args, format).await,
    }
}

/// Resolve the target workspace id for a skills verb: the named workspace slug
/// when `--workspace` is given, else the bootstrapped `default` workspace
/// (created lazily so a standalone CLI is usable without prior onboarding).
async fn resolve_skills_workspace(store: &Store, slug: Option<&str>) -> Result<String> {
    match slug {
        Some(slug) => {
            let id: Option<String> = sqlx::query_scalar("SELECT id FROM workspace WHERE slug = ?")
                .bind(slug)
                .fetch_optional(store.pool())
                .await
                .context("look up workspace by slug")?;
            id.with_context(|| format!("no workspace with slug `{slug}`"))
        }
        None => ensure_default_workspace(store).await,
    }
}

/// `hangar skills sync`: import a toolkit skills directory into a workspace.
///
/// Resolves the source directory (`--source`, else `$AINB_TOOLKIT_SKILLS_DIR`,
/// else a walk up to `toolkit/packages/skills`), then either previews the
/// imports (`--dry-run`) or upserts them workspace-scoped via the daemon's
/// idempotent importer.
async fn run_skills_sync(store: &Store, args: SkillsSyncArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_daemon::skills_sync::{
        SkillImporter, ToolkitDirImporter, default_source_dir, skills_sync_from,
    };

    let source = match args.source {
        Some(p) => p,
        None => default_source_dir().context(
            "could not locate a skills source: set $AINB_TOOLKIT_SKILLS_DIR or pass --source PATH",
        )?,
    };

    if args.dry_run {
        // Walk + parse only; never touch the store.
        let parsed = ToolkitDirImporter::new(&source).collect().context("scan skills source")?;
        println!(
            "dry-run: {} skill(s) would be imported from {}",
            parsed.len(),
            source.display()
        );
        for skill in &parsed {
            println!("  {}  ({} file(s))", skill.name, skill.files.len());
        }
        return Ok(());
    }

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let report = skills_sync_from(store.pool(), &ws, &source).await.context("import skills")?;
    println!(
        "imported {} skill(s) from {}",
        report.imported.len(),
        source.display()
    );
    for (name, _id) in &report.imported {
        println!("  {name}");
    }
    Ok(())
}

/// `hangar skills list`: list the skills imported into a workspace.
async fn run_skills_list(store: &Store, args: SkillsListArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::skill::SkillRepo;

    // A missing/empty workspace lists as no skills, not an error.
    let workspace_id = match args.workspace.as_deref() {
        Some(slug) => {
            let id: Option<String> = sqlx::query_scalar("SELECT id FROM workspace WHERE slug = ?")
                .bind(slug)
                .fetch_optional(store.pool())
                .await
                .context("look up workspace by slug")?;
            id
        }
        None => find_default_workspace(store).await?,
    };
    let Some(workspace_id) = workspace_id else {
        render_skill_list(&[], format);
        return Ok(());
    };
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let skills = SkillRepo::list(store.pool(), &ws).await.context("list skills")?;
    render_skill_list(&skills, format);
    Ok(())
}

/// Dispatch the `hangar config` verbs (synchronous — pure file IO, no database).
fn dispatch_config(cmd: ConfigCommand, format: OutputFormat) -> Result<()> {
    match cmd {
        ConfigCommand::EnvAllow(EnvAllowCommand::List) => run_env_allow_list(format),
        ConfigCommand::EnvAllow(EnvAllowCommand::Add(args)) => run_env_allow_add(args),
        ConfigCommand::EnvAllow(EnvAllowCommand::Remove(args)) => run_env_allow_remove(args),
        ConfigCommand::Warnings(WarningsCommand::Reset(args)) => run_warnings_reset(args),
    }
}

/// `hangar config warnings reset [--provider <name>]`: wipe recorded
/// danger-full-access acks so they show again on the next launch / dispatch.
///
/// `--provider <name>` removes only `provider:<name>:session:*` acks (that
/// provider re-warns next dispatch); no flag removes every ack including
/// `first_run`. Preserves foreign `state.toml` sections.
fn run_warnings_reset(args: WarningsResetArgs) -> Result<()> {
    let path =
        ainb_hangar_daemon::warnings::default_state_path().context("resolve state.toml path")?;
    let removed = match &args.provider {
        Some(provider) => {
            let p = provider.clone();
            ainb_hangar_daemon::warnings::reset_at(&path, move |k| {
                ainb_hangar_core::warnings::is_provider_ack(k, &p)
            })
            .context("reset provider warning acks")?
        }
        None => ainb_hangar_daemon::warnings::reset_at(&path, |_| true)
            .context("reset all warning acks")?,
    };
    match &args.provider {
        Some(p) => println!(
            "reset {removed} {p} warning ack(s); next {p} dispatch will re-warn about danger-full-access"
        ),
        None => {
            println!("reset {removed} warning ack(s); danger-full-access warnings will show again")
        }
    }
    Ok(())
}

/// `hangar config env.allow list`: print the merged effective set.
///
/// Shows every allowlisted key, then every hardcoded-deny key with a
/// `[deny-locked]` suffix (deny always overrides allow, so it is part of the
/// effective policy the operator should see).
fn run_env_allow_list(format: OutputFormat) -> Result<()> {
    let path = ainb_hangar_daemon::dispatch::default_allow_path()
        .context("resolve env.allow.toml path")?;
    let cfg = ainb_hangar_daemon::dispatch::load_allow_at(&path).context("load env.allow.toml")?;

    match format {
        OutputFormat::Json => {
            let allow: Vec<&String> = cfg.allow.iter().collect();
            let deny: Vec<&&str> = ainb_hangar_core::env_policy::DENY.iter().collect();
            println!(
                "{{\"allow\":{},\"deny_locked\":{}}}",
                json_string_array(allow.iter().map(|s| s.as_str())),
                json_string_array(deny.iter().map(|s| **s)),
            );
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            for key in &cfg.allow {
                println!("{key}");
            }
            for denied in ainb_hangar_core::env_policy::DENY {
                println!("{denied} [deny-locked]");
            }
        }
    }
    Ok(())
}

/// `hangar config env.allow add <KEY>`: reject deny-family keys, else upsert +
/// atomically save (preserving foreign sections).
fn run_env_allow_add(args: EnvAllowKeyArgs) -> Result<()> {
    if ainb_hangar_core::env_policy::DENY.contains(&args.key.as_str()) {
        anyhow::bail!(
            "{} is in the hardcoded deny family and can never be allowlisted (code-injection risk); \
             refusing to write it",
            args.key
        );
    }
    let path = ainb_hangar_daemon::dispatch::default_allow_path()
        .context("resolve env.allow.toml path")?;
    let mut cfg =
        ainb_hangar_daemon::dispatch::load_allow_at(&path).context("load env.allow.toml")?;
    if cfg.allow.insert(args.key.clone()) {
        ainb_hangar_daemon::dispatch::save_allow_at(&path, &cfg).context("save env.allow.toml")?;
        println!("added {} to the env allowlist", args.key);
    } else {
        println!("{} is already on the env allowlist", args.key);
    }
    Ok(())
}

/// `hangar config env.allow remove <KEY>`: remove + atomically save.
fn run_env_allow_remove(args: EnvAllowKeyArgs) -> Result<()> {
    let path = ainb_hangar_daemon::dispatch::default_allow_path()
        .context("resolve env.allow.toml path")?;
    let mut cfg =
        ainb_hangar_daemon::dispatch::load_allow_at(&path).context("load env.allow.toml")?;
    if cfg.allow.remove(&args.key) {
        ainb_hangar_daemon::dispatch::save_allow_at(&path, &cfg).context("save env.allow.toml")?;
        println!("removed {} from the env allowlist", args.key);
    } else {
        println!(
            "{} was not on the env allowlist (nothing removed)",
            args.key
        );
    }
    Ok(())
}

/// Render a slice of strings as a compact JSON array, reusing the module's
/// JSON string escaper.
fn json_string_array<'a, I: Iterator<Item = &'a str>>(items: I) -> String {
    let body = items.map(json_string).collect::<Vec<_>>().join(",");
    format!("[{body}]")
}

/// Dispatch the `hangar auth` verbs.
async fn dispatch_auth(cmd: AuthCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        AuthCommand::Token(TokenCommand::Create(args)) => run_token_create(&store, args).await,
        AuthCommand::Token(TokenCommand::List) => run_token_list(&store, format).await,
        AuthCommand::Token(TokenCommand::Revoke(args)) => run_token_revoke(&store, args).await,
        AuthCommand::DaemonToken(DaemonTokenCommand::Create(args)) => {
            run_daemon_token_create(&store, args).await
        }
    }
}

/// `hangar auth token create`: bootstrap the default owner, mint a PAT, and
/// print the plaintext **once** with a no-second-chance warning.
async fn run_token_create(store: &Store, args: TokenCreateArgs) -> Result<()> {
    let pool = store.pool();
    // Reuse the issue path's bootstrap so a standalone CLI has an owning user.
    ensure_default_workspace(store).await?;
    let user_id = default_owner_id(store)
        .await?
        .context("default owner user missing after bootstrap")?;

    let clock = SystemClock;
    let idgen = SystemIdGen;
    let mut rng = rand::rngs::OsRng;
    let (record, minted) = mint_pat(
        pool,
        &user_id,
        Some(args.scope.as_str()),
        &clock,
        &idgen,
        &mut rng,
    )
    .await
    .context("mint personal access token")?;

    // The plaintext is shown here and nowhere else, ever.
    print!(
        "{}",
        token_create_output(&record.id, args.name.as_deref(), &minted.plaintext)
    );
    Ok(())
}

/// `hangar auth token list`: list the owner's PATs. Never prints a plaintext —
/// the store has none to print (only the digest is persisted).
async fn run_token_list(store: &Store, format: OutputFormat) -> Result<()> {
    let pool = store.pool();
    let Some(user_id) = default_owner_id(store).await? else {
        render_token_list(&[], format);
        return Ok(());
    };
    let tokens = PatRepo::list_by_user(pool, &user_id)
        .await
        .context("list personal access tokens")?;
    render_token_list(&tokens, format);
    Ok(())
}

/// `hangar auth token revoke`: revoke a PAT by id.
async fn run_token_revoke(store: &Store, args: TokenRevokeArgs) -> Result<()> {
    let removed = PatRepo::revoke(store.pool(), &args.id)
        .await
        .with_context(|| format!("revoke token {}", args.id))?;
    if removed {
        println!("revoked token {}", args.id);
    } else {
        println!("no token with id {} (nothing revoked)", args.id);
    }
    Ok(())
}

/// `hangar auth daemon-token create`: mint a daemon token, print plaintext once.
async fn run_daemon_token_create(store: &Store, args: DaemonTokenCreateArgs) -> Result<()> {
    let clock = SystemClock;
    let idgen = SystemIdGen;
    let mut rng = rand::rngs::OsRng;
    let (record, minted) =
        mint_daemon_token(store.pool(), &args.runtime_id, &clock, &idgen, &mut rng)
            .await
            .context("mint daemon token")?;
    print!(
        "{}",
        token_create_output(&record.id, None, &minted.plaintext)
    );
    Ok(())
}

/// Dispatch the `hangar issue` verbs.
async fn dispatch_issue(cmd: IssueCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        IssueCommand::Create(args) => run_issue_create(&store, args).await,
        IssueCommand::List(args) => run_issue_list(&store, args, format).await,
        IssueCommand::Show(args) => run_issue_show(&store, args, format).await,
        IssueCommand::Update(args) => run_issue_update(&store, args).await,
    }
}

/// `hangar issue update`: edit a subset of an issue's mutable fields,
/// workspace-scoped.
///
/// Resolves the workspace the same way the other verbs do (`--workspace`, else
/// the bootstrapped `default`), maps the present flags onto an
/// [`IssueFieldUpdate`], and drives the workspace-scoped store edit. An issue id
/// that resolves to no row in the workspace (an unknown id or a foreign tenant's
/// issue) is reported as an error — never a silent no-op. The mutually-exclusive
/// `--assign`/`--unassign` and `--due`/`--clear-due` pairs are enforced by clap.
async fn run_issue_update(store: &Store, args: IssueUpdateArgs) -> Result<()> {
    use ainb_hangar_store::repo::issue::IssueFieldUpdate;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;

    // Map the present flags onto the partial edit. The two nullable fields use
    // the clear-flag to distinguish "clear to none" from "leave unchanged".
    let assignee = if args.unassign {
        Some(None)
    } else {
        args.assign
            .as_deref()
            .map(|id| ActorRef::new(ActorKind::Agent, id).context("assignee agent id was empty"))
            .transpose()?
            .map(Some)
    };
    let due_date = if args.clear_due {
        Some(None)
    } else {
        args.due.map(Some)
    };
    let update = IssueFieldUpdate {
        state: args.state,
        assignee,
        priority: args.priority,
        due_date,
    };

    if update.is_empty() {
        anyhow::bail!(
            "nothing to update: pass at least one of --state / --assign / --unassign / \
             --priority / --due / --clear-due"
        );
    }

    let touched = IssueRepo::update_fields(store.pool(), &workspace_id, &args.id, &update)
        .await
        .with_context(|| format!("update issue {}", args.id))?;
    if touched {
        println!("updated issue {}", args.id);
    } else {
        anyhow::bail!("no issue with id {} in this workspace", args.id);
    }
    Ok(())
}

/// `hangar issue create`: bootstrap a workspace if absent, then insert.
///
/// With `--assign <agent_id>` the issue is created assigned to the agent AND a
/// `queued` task is enqueued for the agent's runtime, so the daemon's claim loop
/// dispatches it (and materialises the agent's skills, P6.4). The task id is
/// printed alongside the issue id.
async fn run_issue_create(store: &Store, args: IssueCreateArgs) -> Result<()> {
    let pool = store.pool();
    let workspace_id = ensure_default_workspace(store).await?;
    let idgen = SystemIdGen;
    let clock = SystemClock;
    let id = idgen.new_ulid();
    let creator = ActorRef::new(ActorKind::Member, DEFAULT_CREATOR_ID)
        .expect("default creator id is non-empty");

    // Resolve the assignee (if any): the agent must exist in the workspace; its
    // runtime is the queue the task lands on. Resolved BEFORE the issue insert so
    // a bad agent id fails before any write.
    let assignment = match args.assign.as_deref() {
        Some(agent_id) => Some(resolve_agent_runtime(pool, &workspace_id, agent_id).await?),
        None => None,
    };
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&clock);

    let new = NewIssue {
        id: id.clone(),
        workspace_id: workspace_id.clone(),
        title: args.title,
        description: args.description,
        state: args.state,
        assignee: assignment
            .as_ref()
            .map(|a| ActorRef::new(ActorKind::Agent, &a.agent_id).expect("agent id non-empty")),
        creator,
        created_at: now,
        priority: args.priority,
        due_date: args.due,
        labels: args.labels.clone(),
    };
    IssueRepo::insert(pool, &new).await.context("insert issue")?;

    // When assigned, enqueue a task for the agent's runtime so the daemon claims
    // + dispatches it (materialising the agent's skills first).
    if let Some(a) = assignment {
        let task_id = idgen.new_ulid();
        TaskRepo::insert(
            pool,
            &ainb_hangar_store::repo::task::NewTask {
                id: task_id.clone(),
                workspace_id,
                runtime_id: a.runtime_id,
                agent_id: a.agent_id,
                issue_id: Some(id.clone()),
                work_dir: None,
                priority: args.priority,
                created_at: now,
                autopilot_run_id: None,
            },
        )
        .await
        .context("enqueue task for assigned agent")?;
        println!("created issue {id}");
        println!("queued task {task_id}");
    } else {
        println!("created issue {id}");
    }
    Ok(())
}

/// `hangar issue list`: list issues in the default workspace + state.
async fn run_issue_list(store: &Store, args: IssueListArgs, format: OutputFormat) -> Result<()> {
    let pool = store.pool();
    let Some(workspace_id) = find_default_workspace(store).await? else {
        // No workspace yet -> no issues. Empty result, not an error.
        render_issue_list(&[], format);
        return Ok(());
    };
    let issues = IssueRepo::list_by_workspace_state(pool, &workspace_id, &args.state)
        .await
        .context("list issues")?;
    render_issue_list(&issues, format);
    Ok(())
}

/// `hangar issue show`: fetch one issue by id.
async fn run_issue_show(store: &Store, args: IssueShowArgs, format: OutputFormat) -> Result<()> {
    let pool = store.pool();
    let issue = IssueRepo::get_by_id(pool, &args.id)
        .await
        .context("fetch issue")?
        .with_context(|| format!("no issue with id {}", args.id))?;
    match format {
        OutputFormat::Json => println!("{}", issue_to_json(&issue)),
        OutputFormat::Csv => {
            println!("{}", issue_csv_header());
            println!("{}", issue_csv_row(&issue));
        }
        OutputFormat::Markdown => {
            print!("{}", issue_md_header());
            println!("{}", issue_md_row(&issue));
        }
        OutputFormat::Text => println!("{}", issue_line(&issue)),
    }
    Ok(())
}

/// Dispatch the `hangar task` verbs.
async fn dispatch_task(cmd: TaskCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        TaskCommand::List(args) => run_task_list(&store, args, format).await,
        TaskCommand::Cancel(args) => run_task_cancel(&store, args).await,
        TaskCommand::Retry(args) => run_task_retry(&store, args).await,
    }
}

/// `hangar task list`: list pending tasks for one runtime, or all runtimes in
/// the default workspace when `--runtime` is omitted.
async fn run_task_list(store: &Store, args: TaskListArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_store::repo::agent_runtime::AgentRuntimeRepo;

    let pool = store.pool();
    let mut tasks = Vec::new();
    match args.runtime {
        Some(runtime_id) => {
            tasks = TaskRepo::list_pending_for_runtime(pool, &runtime_id)
                .await
                .context("list pending tasks")?;
        }
        None => {
            if let Some(workspace_id) = find_default_workspace(store).await? {
                let runtimes = AgentRuntimeRepo::list_by_workspace(pool, &workspace_id)
                    .await
                    .context("list runtimes")?;
                for rt in runtimes {
                    let mut pending = TaskRepo::list_pending_for_runtime(pool, &rt.id)
                        .await
                        .context("list pending tasks")?;
                    tasks.append(&mut pending);
                }
            }
        }
    }
    render_task_list(&tasks, format);
    Ok(())
}

/// `hangar task cancel`: `{queued|dispatched|running} -> cancelled`.
async fn run_task_cancel(store: &Store, args: TaskIdArgs) -> Result<()> {
    let pool = store.pool();
    let clock = SystemClock;
    let outcome = CancelTaskService::cancel(pool, &args.id, &clock)
        .await
        .with_context(|| format!("cancel task {}", args.id))?;
    println!("task {} cancel: {outcome:?}", args.id);
    Ok(())
}

/// `hangar task retry`: spawn a retry child for a retryable failed task.
async fn run_task_retry(store: &Store, args: TaskIdArgs) -> Result<()> {
    let pool = store.pool();
    let task: Task = TaskRepo::get_by_id(pool, &args.id)
        .await
        .context("fetch task")?
        .with_context(|| format!("no task with id {}", args.id))?;
    let clock = SystemClock;
    let idgen = SystemIdGen;
    let new_id = idgen.new_ulid();
    let decision = RetryService::maybe_retry_failed(pool, &task, &new_id, &clock)
        .await
        .with_context(|| format!("retry task {}", args.id))?;
    match decision {
        RetryDecision::Spawned { new_task_id } => {
            println!("task {} retried: spawned {new_task_id}", args.id);
        }
        RetryDecision::DoNotRetry => {
            println!(
                "task {} not retried (non-retryable or attempts exhausted)",
                args.id
            );
        }
    }
    Ok(())
}

/// `hangar beads reconcile`: delegate to the daemon's reconcile dispatcher.
async fn run_beads_reconcile(args: BeadsReconcileArgs) -> Result<()> {
    use ainb_hangar_daemon::beads_sync::reconcile;
    use reconcile::cli::{OutputFormat as ReconcileFormat, ReconcileArgs};

    let daemon_args = ReconcileArgs {
        dry_run: args.dry_run,
        label: args.label,
        format: if args.json {
            ReconcileFormat::Json
        } else {
            ReconcileFormat::Text
        },
    };
    reconcile::dispatch(&daemon_args).await
}

/// `hangar daemon status`: probe the database reachability.
///
/// The daemon owns no pidfile yet, so "status" is the database-reachability
/// check (`Store::open_default` runs every migration). A reachable, migrated
/// database is the precondition the daemon needs to boot; this is the real,
/// backed signal available today (`daemon start|stop` land in a later phase).
async fn run_daemon_status() -> Result<()> {
    match Store::open_default().await {
        Ok(_) => {
            println!("hangar daemon: database reachable (migrations applied)");
            Ok(())
        }
        Err(e) => {
            println!("hangar daemon: database unreachable: {e}");
            Err(e.context("open hangar database"))
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Workspace bootstrap + render helpers.
// ──────────────────────────────────────────────────────────────────────────

/// An agent resolved for `issue create --assign`: its id + the runtime its task
/// will queue on.
struct AgentAssignment {
    /// The agent (`agent.id`).
    agent_id: String,
    /// The agent's runtime (`agent_runtime.id`) — the task queue.
    runtime_id: String,
}

/// Resolve an agent's runtime for assignment, erroring if the agent does not
/// exist in `workspace_id`. The lookup is workspace-scoped so an agent id from
/// another tenant can never be assigned a task here.
async fn resolve_agent_runtime(
    pool: &sqlx::SqlitePool,
    workspace_id: &str,
    agent_id: &str,
) -> Result<AgentAssignment> {
    let runtime_id: Option<String> =
        sqlx::query_scalar("SELECT runtime_id FROM agent WHERE id = ? AND workspace_id = ?")
            .bind(agent_id)
            .bind(workspace_id)
            .fetch_optional(pool)
            .await
            .context("look up agent runtime")?;
    let runtime_id =
        runtime_id.with_context(|| format!("no agent with id `{agent_id}` in this workspace"))?;
    Ok(AgentAssignment {
        agent_id: agent_id.to_string(),
        runtime_id,
    })
}

/// Return the default workspace id, or `None` if no workspace exists yet.
async fn find_default_workspace(store: &Store) -> Result<Option<String>> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM workspace ORDER BY created_at LIMIT 1")
            .fetch_optional(store.pool())
            .await
            .context("query default workspace")?;
    Ok(id)
}

/// Return the default workspace id, lazily bootstrapping a workspace + owner
/// user + member row when none exists.
///
/// The `issue` table's `workspace_id` FK requires a `workspace` row; the
/// `member:stevie` creator references a `member` row only at the service layer
/// (FK-less by design, per the actor module), so the member row is informational
/// but kept consistent. Idempotent: a second call returns the existing id.
async fn ensure_default_workspace(store: &Store) -> Result<String> {
    if let Some(id) = find_default_workspace(store).await? {
        return Ok(id);
    }
    let pool = store.pool();
    let idgen = SystemIdGen;
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
    let workspace_id = idgen.new_ulid();
    let user_id = idgen.new_ulid();

    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(&workspace_id)
        .bind(DEFAULT_WORKSPACE_SLUG)
        .bind(DEFAULT_WORKSPACE_NAME)
        .bind(now)
        .execute(pool)
        .await
        .context("bootstrap default workspace")?;
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(&user_id)
        .bind(DEFAULT_OWNER_EMAIL)
        .bind(now)
        .execute(pool)
        .await
        .context("bootstrap default owner user")?;
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(&workspace_id)
        .bind(&user_id)
        .execute(pool)
        .await
        .context("bootstrap default member")?;
    Ok(workspace_id)
}

/// Return the default owner user id (the first user, oldest first), or `None`
/// if no user exists yet.
async fn default_owner_id(store: &Store) -> Result<Option<String>> {
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM user ORDER BY created_at LIMIT 1")
        .fetch_optional(store.pool())
        .await
        .context("query default owner user")?;
    Ok(id)
}

// ──────────────────────────────────────────────────────────────────────────
// Token render helpers (pure, so the "plaintext shown once" + "list never
// prints plaintext" contracts are unit-testable without capturing stdout).
// ──────────────────────────────────────────────────────────────────────────

/// The output of a successful token mint: the id, an optional advisory label,
/// the plaintext once, and an explicit no-second-chance warning. This is the
/// **only** place a plaintext is ever surfaced.
///
/// `name` is advisory only: the `pat` schema carries no label column at v1, so
/// the supplied `--name` is echoed back here rather than silently dropped (and
/// is never persisted).
fn token_create_output(id: &str, name: Option<&str>, plaintext: &str) -> String {
    let label = name.map_or_else(String::new, |n| format!(" ({n})"));
    format!(
        "token {id}{label} created\n\
         {plaintext}\n\
         warning: this token is shown only once and is not recoverable — store it now.\n"
    )
}

/// Render a PAT list in the chosen format. By construction this can never emit
/// a plaintext: [`PatRecord`] carries only the digest, scope, and timestamps.
fn render_token_list(tokens: &[PatRecord], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let body = tokens.iter().map(pat_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", pat_csv_header());
            for t in tokens {
                println!("{}", pat_csv_row(t));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", pat_md_header());
            for t in tokens {
                println!("{}", pat_md_row(t));
            }
        }
        OutputFormat::Text => {
            if tokens.is_empty() {
                println!("no tokens");
            } else {
                for t in tokens {
                    println!("{}", pat_line(t));
                }
            }
        }
    }
}

/// One-line text summary of a PAT (id, scope, `created_at` — never the secret).
fn pat_line(t: &PatRecord) -> String {
    format!(
        "{}  scope={}  created_at={}",
        t.id,
        t.scope.as_deref().unwrap_or("-"),
        t.created_at
    )
}
const fn pat_csv_header() -> &'static str {
    "id,scope,created_at,last_used"
}
fn pat_csv_row(t: &PatRecord) -> String {
    format!(
        "{},{},{},{}",
        csv_field(&t.id),
        csv_field(t.scope.as_deref().unwrap_or("")),
        t.created_at,
        t.last_used.map_or_else(String::new, |v| v.to_string()),
    )
}
const fn pat_md_header() -> &'static str {
    "| id | scope | created_at | last_used |\n| --- | --- | --- | --- |\n"
}
fn pat_md_row(t: &PatRecord) -> String {
    format!(
        "| {} | {} | {} | {} |",
        md_cell(&t.id),
        md_cell(t.scope.as_deref().unwrap_or("-")),
        t.created_at,
        t.last_used.map_or_else(|| "-".to_string(), |v| v.to_string()),
    )
}
/// Minimal stable JSON object for one PAT (digest deliberately omitted — the
/// hash is a server-side secret-equivalent, not list output).
fn pat_to_json(t: &PatRecord) -> String {
    let scope = t.scope.as_deref().map_or_else(|| "null".to_string(), json_string);
    let last_used = t.last_used.map_or_else(|| "null".to_string(), |v| v.to_string());
    format!(
        "{{\"id\":{},\"scope\":{},\"created_at\":{},\"last_used\":{}}}",
        json_string(&t.id),
        scope,
        t.created_at,
        last_used,
    )
}

/// Render an issue list in the chosen format.
fn render_issue_list(issues: &[Issue], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let body = issues.iter().map(issue_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", issue_csv_header());
            for i in issues {
                println!("{}", issue_csv_row(i));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", issue_md_header());
            for i in issues {
                println!("{}", issue_md_row(i));
            }
        }
        OutputFormat::Text => {
            if issues.is_empty() {
                println!("no issues");
            } else {
                for i in issues {
                    println!("{}", issue_line(i));
                }
            }
        }
    }
}

/// One-line text summary of an issue. `priority` is 0..3 = P3..P0 (higher =
/// more urgent); the due date and labels are shown only when set.
fn issue_line(i: &Issue) -> String {
    let due = i.due_date.map_or_else(String::new, |d| format!("  due={d}"));
    let labels = if i.labels.is_empty() {
        String::new()
    } else {
        format!("  labels={}", i.labels.join(","))
    };
    format!(
        "{}  [{}]  priority={}  {}{due}{labels}",
        i.id, i.state, i.priority, i.title
    )
}

/// Minimal stable JSON object for one issue (hand-rolled to avoid pulling a
/// serde derive onto the store's `Issue` type from this crate).
fn issue_to_json(i: &Issue) -> String {
    let desc = i.description.as_deref().map_or_else(|| "null".to_string(), json_string);
    let due = i.due_date.map_or_else(|| "null".to_string(), |d| d.to_string());
    format!(
        "{{\"id\":{},\"workspace_id\":{},\"title\":{},\"description\":{},\"state\":{},\"created_at\":{},\"priority\":{},\"due_date\":{},\"labels\":{}}}",
        json_string(&i.id),
        json_string(&i.workspace_id),
        json_string(&i.title),
        desc,
        json_string(&i.state),
        i.created_at,
        i.priority,
        due,
        json_string_array(i.labels.iter().map(String::as_str)),
    )
}

/// Render an autopilot list (each paired with its last run's status) in the
/// chosen format.
///
/// The `enabled`/`disabled` badge and the cron expression are the load-bearing
/// columns the CLI surface test asserts on; `last_run` is the most-recent run's
/// status, or `-` when the autopilot has never fired.
fn render_autopilot_list(rows: &[(Autopilot, Option<String>)], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let body = rows
                .iter()
                .map(|(a, last)| autopilot_to_json(a, last.as_deref()))
                .collect::<Vec<_>>()
                .join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", autopilot_csv_header());
            for (a, last) in rows {
                println!("{}", autopilot_csv_row(a, last.as_deref()));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", autopilot_md_header());
            for (a, last) in rows {
                println!("{}", autopilot_md_row(a, last.as_deref()));
            }
        }
        OutputFormat::Text => {
            if rows.is_empty() {
                println!("no autopilots");
            } else {
                for (a, last) in rows {
                    println!("{}", autopilot_line(a, last.as_deref()));
                }
            }
        }
    }
}

/// The enabled/disabled badge shown in every format (load-bearing for the CLI
/// surface test's "disabled" assertion).
const fn autopilot_badge(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

/// One-line text summary of an autopilot.
fn autopilot_line(a: &Autopilot, last_run: Option<&str>) -> String {
    format!(
        "{}  {}  cron={}  next_tick={}  last_run={}  [{}]",
        a.id,
        a.name,
        a.cron_expr,
        a.next_tick_at.map_or_else(|| "-".to_string(), |v| v.to_string()),
        last_run.unwrap_or("-"),
        autopilot_badge(a.enabled),
    )
}

const fn autopilot_csv_header() -> &'static str {
    "id,name,cron,next_tick_at,enabled,last_run"
}
fn autopilot_csv_row(a: &Autopilot, last_run: Option<&str>) -> String {
    format!(
        "{},{},{},{},{},{}",
        csv_field(&a.id),
        csv_field(&a.name),
        csv_field(&a.cron_expr),
        a.next_tick_at.map_or_else(String::new, |v| v.to_string()),
        autopilot_badge(a.enabled),
        csv_field(last_run.unwrap_or("")),
    )
}
const fn autopilot_md_header() -> &'static str {
    "| id | name | cron | next_tick_at | enabled | last_run |\n\
     | --- | --- | --- | --- | --- | --- |\n"
}
fn autopilot_md_row(a: &Autopilot, last_run: Option<&str>) -> String {
    format!(
        "| {} | {} | {} | {} | {} | {} |",
        md_cell(&a.id),
        md_cell(&a.name),
        md_cell(&a.cron_expr),
        a.next_tick_at.map_or_else(|| "-".to_string(), |v| v.to_string()),
        autopilot_badge(a.enabled),
        md_cell(last_run.unwrap_or("-")),
    )
}
/// Minimal stable JSON object for one autopilot (hand-rolled to avoid pulling a
/// serde derive onto the store's `Autopilot` type from this crate).
fn autopilot_to_json(a: &Autopilot, last_run: Option<&str>) -> String {
    let instructions = a.instructions.as_deref().map_or_else(|| "null".to_string(), json_string);
    let next_tick = a.next_tick_at.map_or_else(|| "null".to_string(), |v| v.to_string());
    let last = last_run.map_or_else(|| "null".to_string(), json_string);
    format!(
        "{{\"id\":{},\"workspace_id\":{},\"agent_id\":{},\"name\":{},\"instructions\":{},\
          \"cron_expr\":{},\"max_concurrent_runs\":{},\"next_tick_at\":{},\"enabled\":{},\
          \"last_run\":{}}}",
        json_string(&a.id),
        json_string(&a.workspace_id),
        json_string(&a.agent_id),
        json_string(&a.name),
        instructions,
        json_string(&a.cron_expr),
        a.max_concurrent_runs,
        next_tick,
        a.enabled,
        last,
    )
}

/// Render a task list in the chosen format.
fn render_task_list(tasks: &[Task], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let body = tasks.iter().map(task_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", task_csv_header());
            for t in tasks {
                println!("{}", task_csv_row(t));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", task_md_header());
            for t in tasks {
                println!("{}", task_md_row(t));
            }
        }
        OutputFormat::Text => {
            if tasks.is_empty() {
                println!("no pending tasks");
            } else {
                for t in tasks {
                    println!("{}", task_line(t));
                }
            }
        }
    }
}

/// Render a skill list in the chosen format.
fn render_skill_list(skills: &[ainb_hangar_core::skill::SkillWithFiles], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let body = skills.iter().map(skill_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", skill_csv_header());
            for s in skills {
                println!("{}", skill_csv_row(s));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", skill_md_header());
            for s in skills {
                println!("{}", skill_md_row(s));
            }
        }
        OutputFormat::Text => {
            if skills.is_empty() {
                println!("no skills");
            } else {
                for s in skills {
                    println!("{}", skill_line(s));
                }
            }
        }
    }
}

/// One-line text summary of a skill (name, file count, description).
fn skill_line(s: &ainb_hangar_core::skill::SkillWithFiles) -> String {
    format!(
        "{}  files={}  {}",
        s.name,
        s.files.len(),
        s.description.as_deref().unwrap_or("-")
    )
}
const fn skill_csv_header() -> &'static str {
    "name,files,description"
}
fn skill_csv_row(s: &ainb_hangar_core::skill::SkillWithFiles) -> String {
    format!(
        "{},{},{}",
        csv_field(s.name.as_str()),
        s.files.len(),
        csv_field(s.description.as_deref().unwrap_or("")),
    )
}
const fn skill_md_header() -> &'static str {
    "| name | files | description |\n| --- | --- | --- |\n"
}
fn skill_md_row(s: &ainb_hangar_core::skill::SkillWithFiles) -> String {
    format!(
        "| {} | {} | {} |",
        md_cell(s.name.as_str()),
        s.files.len(),
        md_cell(s.description.as_deref().unwrap_or("-")),
    )
}
/// Minimal stable JSON object for one skill (name, file count, description).
fn skill_to_json(s: &ainb_hangar_core::skill::SkillWithFiles) -> String {
    let desc = s.description.as_deref().map_or_else(|| "null".to_string(), json_string);
    format!(
        "{{\"name\":{},\"files\":{},\"description\":{}}}",
        json_string(s.name.as_str()),
        s.files.len(),
        desc,
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Agent render helpers (e38.15) — over the store row model.
// ──────────────────────────────────────────────────────────────────────────

/// Render a list of agents in the requested format (id, name, archived, model,
/// thinking, arg count, env count).
fn render_agent_list(agents: &[ainb_hangar_store::repo::agent::Agent], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let body = agents.iter().map(agent_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", agent_csv_header());
            for a in agents {
                println!("{}", agent_csv_row(a));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", agent_md_header());
            for a in agents {
                println!("{}", agent_md_row(a));
            }
        }
        OutputFormat::Text => {
            if agents.is_empty() {
                println!("no agents");
            } else {
                for a in agents {
                    println!("{}", agent_line(a));
                }
            }
        }
    }
}

/// One-line text summary of an agent (id, name, archived badge, model).
fn agent_line(a: &ainb_hangar_store::repo::agent::Agent) -> String {
    format!(
        "{}  {}{}  model={}  args={}  env={}",
        a.id,
        a.name,
        if a.archived { "  [archived]" } else { "" },
        a.model.as_deref().unwrap_or("-"),
        a.cli_args.len(),
        a.agent_env.len(),
    )
}
const fn agent_csv_header() -> &'static str {
    "id,name,archived,model,thinking,args,env"
}
fn agent_csv_row(a: &ainb_hangar_store::repo::agent::Agent) -> String {
    format!(
        "{},{},{},{},{},{},{}",
        csv_field(a.id.as_str()),
        csv_field(a.name.as_str()),
        a.archived,
        csv_field(a.model.as_deref().unwrap_or("")),
        csv_field(a.thinking.as_deref().unwrap_or("")),
        a.cli_args.len(),
        a.agent_env.len(),
    )
}
const fn agent_md_header() -> &'static str {
    "| id | name | archived | model | thinking | args | env |\n\
     | --- | --- | --- | --- | --- | --- | --- |\n"
}
fn agent_md_row(a: &ainb_hangar_store::repo::agent::Agent) -> String {
    format!(
        "| {} | {} | {} | {} | {} | {} | {} |",
        md_cell(a.id.as_str()),
        md_cell(a.name.as_str()),
        a.archived,
        md_cell(a.model.as_deref().unwrap_or("-")),
        md_cell(a.thinking.as_deref().unwrap_or("-")),
        a.cli_args.len(),
        a.agent_env.len(),
    )
}
/// Minimal stable JSON object for one agent (id, name, archived + config knobs).
fn agent_to_json(a: &ainb_hangar_store::repo::agent::Agent) -> String {
    let model = a.model.as_deref().map_or_else(|| "null".to_string(), json_string);
    let thinking = a.thinking.as_deref().map_or_else(|| "null".to_string(), json_string);
    let args = json_string_array(a.cli_args.iter().map(String::as_str));
    let env = a
        .agent_env
        .iter()
        .map(|(k, v)| format!("{}:{}", json_string(k), json_string(v)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"id\":{},\"name\":{},\"archived\":{},\"model\":{},\"thinking\":{},\"args\":{},\"env\":{{{}}}}}",
        json_string(a.id.as_str()),
        json_string(a.name.as_str()),
        a.archived,
        model,
        thinking,
        args,
        env,
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Template render helpers (pure, over the IO-free embedded registry type).
// ──────────────────────────────────────────────────────────────────────────

/// Render a slice of templates as a list in the chosen format.
fn render_template_list(
    templates: &[ainb_hangar_core::template::AgentTemplate],
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let body = templates.iter().map(template_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", template_csv_header());
            for t in templates {
                println!("{}", template_csv_row(t));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", template_md_header());
            for t in templates {
                println!("{}", template_md_row(t));
            }
        }
        OutputFormat::Text => {
            if templates.is_empty() {
                println!("no templates");
            } else {
                for t in templates {
                    println!("{}", template_line(t));
                }
            }
        }
    }
}

/// One-line text summary of a template (name, skill count, description).
fn template_line(t: &ainb_hangar_core::template::AgentTemplate) -> String {
    format!("{}  skills={}  {}", t.name, t.skills.len(), t.description)
}

/// Full text dump of one template (for `templates show`).
fn template_detail(t: &ainb_hangar_core::template::AgentTemplate) -> String {
    format!(
        "name: {}\ndescription: {}\nagent: {}\nskills: {}\n\ninstructions:\n{}\n",
        t.name,
        t.description,
        t.agent_md_path,
        t.skills.join(", "),
        t.instructions,
    )
}

const fn template_csv_header() -> &'static str {
    "name,skills,agent_md_path,description"
}
fn template_csv_row(t: &ainb_hangar_core::template::AgentTemplate) -> String {
    format!(
        "{},{},{},{}",
        csv_field(&t.name),
        csv_field(&t.skills.join(" ")),
        csv_field(&t.agent_md_path),
        csv_field(&t.description),
    )
}
const fn template_md_header() -> &'static str {
    "| name | skills | agent | description |\n| --- | --- | --- | --- |\n"
}
fn template_md_row(t: &ainb_hangar_core::template::AgentTemplate) -> String {
    format!(
        "| {} | {} | {} | {} |",
        md_cell(&t.name),
        md_cell(&t.skills.join(", ")),
        md_cell(&t.agent_md_path),
        md_cell(&t.description),
    )
}
/// Stable JSON object for one template (the full embedded shape).
fn template_to_json(t: &ainb_hangar_core::template::AgentTemplate) -> String {
    let skills = json_string_array(t.skills.iter().map(String::as_str));
    format!(
        "{{\"name\":{},\"description\":{},\"agent_md_path\":{},\"instructions\":{},\"skills\":{}}}",
        json_string(&t.name),
        json_string(&t.description),
        json_string(&t.agent_md_path),
        json_string(&t.instructions),
        skills,
    )
}

// ---- csv / markdown renderers ----------------------------------------------

/// Quote a CSV field if it contains a comma, quote, or newline (RFC-4180).
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Escape a markdown table cell (only the pipe needs escaping).
fn md_cell(s: &str) -> String {
    s.replace('|', "\\|")
}

const fn issue_csv_header() -> &'static str {
    "id,state,title,description,created_at,priority,due_date,labels"
}
fn issue_csv_row(i: &Issue) -> String {
    let due = i.due_date.map_or_else(String::new, |d| d.to_string());
    format!(
        "{},{},{},{},{},{},{},{}",
        csv_field(&i.id),
        csv_field(&i.state),
        csv_field(&i.title),
        csv_field(i.description.as_deref().unwrap_or("")),
        i.created_at,
        i.priority,
        csv_field(&due),
        csv_field(&i.labels.join(" ")),
    )
}
const fn issue_md_header() -> &'static str {
    "| id | state | title | description | priority | due_date | labels |\n\
     | --- | --- | --- | --- | --- | --- | --- |\n"
}
fn issue_md_row(i: &Issue) -> String {
    let due = i.due_date.map_or_else(String::new, |d| d.to_string());
    format!(
        "| {} | {} | {} | {} | {} | {} | {} |",
        md_cell(&i.id),
        md_cell(&i.state),
        md_cell(&i.title),
        md_cell(i.description.as_deref().unwrap_or("")),
        i.priority,
        md_cell(&due),
        md_cell(&i.labels.join(" ")),
    )
}

/// One-line text summary of a task. `priority` is 0..3 = P3..P0 (higher =
/// more urgent; the claim ordering key).
fn task_line(t: &Task) -> String {
    format!(
        "{}  [{}]  priority={} runtime={} agent={}",
        t.id, t.status, t.priority, t.runtime_id, t.agent_id
    )
}
const fn task_csv_header() -> &'static str {
    "id,status,priority,runtime_id,agent_id,attempt,max_attempts"
}
fn task_csv_row(t: &Task) -> String {
    format!(
        "{},{},{},{},{},{},{}",
        csv_field(&t.id),
        csv_field(&t.status),
        t.priority,
        csv_field(&t.runtime_id),
        csv_field(&t.agent_id),
        t.attempt,
        t.max_attempts,
    )
}
const fn task_md_header() -> &'static str {
    "| id | status | priority | runtime | agent | attempt |\n| --- | --- | --- | --- | --- | --- |\n"
}
fn task_md_row(t: &Task) -> String {
    format!(
        "| {} | {} | {} | {} | {} | {}/{} |",
        md_cell(&t.id),
        md_cell(&t.status),
        t.priority,
        md_cell(&t.runtime_id),
        md_cell(&t.agent_id),
        t.attempt,
        t.max_attempts,
    )
}

/// Minimal stable JSON object for one task.
fn task_to_json(t: &Task) -> String {
    format!(
        "{{\"id\":{},\"status\":{},\"priority\":{},\"runtime_id\":{},\"agent_id\":{},\"attempt\":{},\"max_attempts\":{}}}",
        json_string(&t.id),
        json_string(&t.status),
        t.priority,
        json_string(&t.runtime_id),
        json_string(&t.agent_id),
        t.attempt,
        t.max_attempts,
    )
}

/// Escape a string as a JSON string literal (quotes, backslash, control chars).
fn json_string(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // `write!` into a String is infallible; the Result is discarded.
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::registry::CommandRegistry;
    use clap::FromArgMatches;

    /// Build the real `ainb` clap surface (root + every registered command,
    /// including the `hangar` subtree) and parse `argv` through it.
    fn parse_hangar(argv: &[&str]) -> HangarCommand {
        let registry = CommandRegistry::built_ins();
        let app = registry.build_clap(crate::cli::root_clap_command());
        let matches = app
            .try_get_matches_from(argv)
            .unwrap_or_else(|e| panic!("clap rejected {argv:?}: {e}"));
        let (name, sub) = matches.subcommand().expect("subcommand present");
        assert_eq!(name, "hangar", "expected hangar subcommand for {argv:?}");
        HangarCommand::from_arg_matches(sub).expect("extract HangarCommand")
    }

    #[test]
    fn parses_issue_create_with_title_and_description() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Fix bug",
            "--description",
            "details",
        ]);
        let HangarCommand::Issue(IssueCommand::Create(args)) = cmd else {
            panic!("expected issue create, got {cmd:?}");
        };
        assert_eq!(args.title, "Fix bug");
        assert_eq!(args.description.as_deref(), Some("details"));
        assert_eq!(args.state, DEFAULT_ISSUE_STATE);
    }

    #[test]
    fn parses_issue_create_priority() {
        // Explicit value parses through.
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Urgent",
            "--priority",
            "3",
        ]);
        let HangarCommand::Issue(IssueCommand::Create(args)) = cmd else {
            panic!("expected issue create, got {cmd:?}");
        };
        assert_eq!(args.priority, 3, "--priority 3 = P0, most urgent");

        // Omitted -> routine default 0 (P3).
        let cmd = parse_hangar(&["ainb", "hangar", "issue", "create", "--title", "Routine"]);
        let HangarCommand::Issue(IssueCommand::Create(args)) = cmd else {
            panic!("expected issue create, got {cmd:?}");
        };
        assert_eq!(args.priority, 0, "priority defaults to 0 (P3)");
    }

    #[test]
    fn issue_create_priority_rejects_out_of_range() {
        let registry = CommandRegistry::built_ins();
        let app = registry.build_clap(crate::cli::root_clap_command());
        let err = app.try_get_matches_from([
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Bad",
            "--priority",
            "4",
        ]);
        assert!(err.is_err(), "--priority is clamped to 0..=3 (P3..P0)");
    }

    #[test]
    fn parses_issue_create_due_date_and_labels() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Urgent",
            "--priority",
            "2",
            "--due",
            "2026-06-30",
            "--label",
            "bug",
            "--label",
            "p0",
        ]);
        let HangarCommand::Issue(IssueCommand::Create(args)) = cmd else {
            panic!("expected issue create, got {cmd:?}");
        };
        assert_eq!(args.priority, 2, "--priority 2 = P1");
        // 2026-06-30 00:00:00 UTC in epoch millis.
        assert_eq!(
            args.due,
            Some(1_782_777_600_000),
            "--due parses YYYY-MM-DD to UTC-midnight epoch millis"
        );
        assert_eq!(
            args.labels,
            vec!["bug".to_string(), "p0".to_string()],
            "--label is repeatable and order-preserving"
        );

        // Omitted -> no due date, no labels.
        let cmd = parse_hangar(&["ainb", "hangar", "issue", "create", "--title", "Plain"]);
        let HangarCommand::Issue(IssueCommand::Create(args)) = cmd else {
            panic!("expected issue create, got {cmd:?}");
        };
        assert_eq!(args.due, None, "no --due means no due date");
        assert!(args.labels.is_empty(), "no --label means no labels");
    }

    #[test]
    fn issue_create_rejects_malformed_due_date() {
        let registry = CommandRegistry::built_ins();
        let app = registry.build_clap(crate::cli::root_clap_command());
        let err = app.try_get_matches_from([
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Bad",
            "--due",
            "next-tuesday",
        ]);
        assert!(err.is_err(), "--due must be a YYYY-MM-DD date");
    }

    /// User-visible proof: `hangar issue create --priority --due --label`
    /// persists all three attributes onto the created issue, read back through
    /// the store the way `issue list` / `issue show` would surface them.
    #[tokio::test]
    async fn issue_create_persists_priority_due_date_and_labels_end_to_end() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");

        let HangarCommand::Issue(IssueCommand::Create(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Ship it",
            "--priority",
            "3",
            "--due",
            "2026-06-30",
            "--label",
            "bug",
            "--label",
            "p0",
        ]) else {
            panic!("expected issue create");
        };

        run_issue_create(&store, args).await.expect("create issue");

        let workspace_id = find_default_workspace(&store)
            .await
            .expect("find workspace")
            .expect("workspace bootstrapped by create");
        let issues =
            IssueRepo::list_by_workspace_state(store.pool(), &workspace_id, DEFAULT_ISSUE_STATE)
                .await
                .expect("list issues");
        let issue = issues
            .iter()
            .find(|i| i.title == "Ship it")
            .expect("created issue present in the open list");

        assert_eq!(issue.priority, 3, "create persisted --priority 3 (P0)");
        assert_eq!(
            issue.due_date,
            Some(1_782_777_600_000),
            "create persisted --due as UTC-midnight epoch millis"
        );
        assert_eq!(
            issue.labels,
            vec!["bug".to_string(), "p0".to_string()],
            "create persisted both --label values"
        );

        // And the CLI render surfaces them so `issue show` is not lossy.
        let line = issue_line(issue);
        assert!(
            line.contains("priority=3"),
            "text line shows priority: {line}"
        );
        assert!(
            line.contains("labels=bug,p0"),
            "text line shows labels: {line}"
        );
        let json = issue_to_json(issue);
        assert!(
            json.contains("\"priority\":3"),
            "json shows priority: {json}"
        );
        assert!(
            json.contains("\"labels\":[\"bug\",\"p0\"]"),
            "json shows labels array: {json}"
        );
    }

    #[test]
    fn parses_issue_list_default_state_open() {
        let cmd = parse_hangar(&["ainb", "hangar", "issue", "list"]);
        let HangarCommand::Issue(IssueCommand::List(args)) = cmd else {
            panic!("expected issue list, got {cmd:?}");
        };
        assert_eq!(args.state, "open");
    }

    #[test]
    fn parses_issue_show_with_id() {
        let cmd = parse_hangar(&["ainb", "hangar", "issue", "show", "01ABC"]);
        let HangarCommand::Issue(IssueCommand::Show(args)) = cmd else {
            panic!("expected issue show, got {cmd:?}");
        };
        assert_eq!(args.id, "01ABC");
    }

    #[test]
    fn parses_task_list_optional_runtime() {
        let cmd = parse_hangar(&["ainb", "hangar", "task", "list", "--runtime", "rt-1"]);
        let HangarCommand::Task(TaskCommand::List(args)) = cmd else {
            panic!("expected task list, got {cmd:?}");
        };
        assert_eq!(args.runtime.as_deref(), Some("rt-1"));

        let cmd = parse_hangar(&["ainb", "hangar", "task", "list"]);
        let HangarCommand::Task(TaskCommand::List(args)) = cmd else {
            panic!("expected task list, got {cmd:?}");
        };
        assert!(args.runtime.is_none());
    }

    #[test]
    fn parses_task_cancel_and_retry() {
        let cmd = parse_hangar(&["ainb", "hangar", "task", "cancel", "t-1"]);
        let HangarCommand::Task(TaskCommand::Cancel(args)) = cmd else {
            panic!("expected task cancel, got {cmd:?}");
        };
        assert_eq!(args.id, "t-1");

        let cmd = parse_hangar(&["ainb", "hangar", "task", "retry", "t-2"]);
        let HangarCommand::Task(TaskCommand::Retry(args)) = cmd else {
            panic!("expected task retry, got {cmd:?}");
        };
        assert_eq!(args.id, "t-2");
    }

    #[test]
    fn parses_beads_reconcile_flags() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "beads",
            "reconcile",
            "--dry-run",
            "--label",
            "foo",
            "--label",
            "bar",
            "--json",
        ]);
        let HangarCommand::Beads(BeadsCommand::Reconcile(args)) = cmd else {
            panic!("expected beads reconcile, got {cmd:?}");
        };
        assert!(args.dry_run);
        assert!(args.json);
        assert_eq!(args.label, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn parses_daemon_status() {
        let cmd = parse_hangar(&["ainb", "hangar", "daemon", "status"]);
        assert!(matches!(cmd, HangarCommand::Daemon(DaemonCommand::Status)));
    }

    #[test]
    fn hangar_requires_a_subcommand() {
        let registry = CommandRegistry::built_ins();
        let app = registry.build_clap(crate::cli::root_clap_command());
        let err = app.try_get_matches_from(["ainb", "hangar"]);
        assert!(
            err.is_err(),
            "bare `ainb hangar` must error (subcommand_required)"
        );
    }

    #[test]
    fn parses_skills_sync_with_all_flags() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "skills",
            "sync",
            "--workspace",
            "default",
            "--source",
            "/tmp/skills",
            "--dry-run",
        ]);
        let HangarCommand::Skills(SkillsCommand::Sync(args)) = cmd else {
            panic!("expected skills sync, got {cmd:?}");
        };
        assert_eq!(args.workspace.as_deref(), Some("default"));
        assert_eq!(
            args.source.as_deref(),
            Some(std::path::Path::new("/tmp/skills"))
        );
        assert!(args.dry_run);
    }

    #[test]
    fn parses_skills_sync_defaults() {
        let cmd = parse_hangar(&["ainb", "hangar", "skills", "sync"]);
        let HangarCommand::Skills(SkillsCommand::Sync(args)) = cmd else {
            panic!("expected skills sync, got {cmd:?}");
        };
        assert!(args.workspace.is_none());
        assert!(args.source.is_none());
        assert!(!args.dry_run, "dry-run defaults off");
    }

    #[test]
    fn parses_skills_list_optional_workspace() {
        let cmd = parse_hangar(&["ainb", "hangar", "skills", "list", "--workspace", "team-a"]);
        let HangarCommand::Skills(SkillsCommand::List(args)) = cmd else {
            panic!("expected skills list, got {cmd:?}");
        };
        assert_eq!(args.workspace.as_deref(), Some("team-a"));

        let cmd = parse_hangar(&["ainb", "hangar", "skills", "list"]);
        let HangarCommand::Skills(SkillsCommand::List(args)) = cmd else {
            panic!("expected skills list, got {cmd:?}");
        };
        assert!(args.workspace.is_none());
    }

    #[test]
    fn parses_logs_tail_with_all_flags() {
        let cmd = parse_hangar(&[
            "ainb", "hangar", "logs", "tail", "--follow", "--lines", "50", "--level", "warn",
        ]);
        let HangarCommand::Logs(LogsCommand::Tail(args)) = cmd else {
            panic!("expected logs tail, got {cmd:?}");
        };
        assert!(args.follow);
        assert_eq!(args.lines, 50);
        assert_eq!(args.level.as_deref(), Some("warn"));
        assert!(!args.no_follow);
    }

    #[test]
    fn parses_logs_tail_no_follow_defaults() {
        let cmd = parse_hangar(&["ainb", "hangar", "logs", "tail", "--no-follow"]);
        let HangarCommand::Logs(LogsCommand::Tail(args)) = cmd else {
            panic!("expected logs tail, got {cmd:?}");
        };
        assert!(args.no_follow);
        assert!(!args.follow);
        assert_eq!(args.lines, 200, "default tail window");
        assert!(args.level.is_none());
    }

    #[test]
    fn logs_pretty_line_renders_full_event() {
        use ainb_hangar_core::logs::LogLine;
        let line = LogLine {
            timestamp: "2026-05-31T12:00:00.000001Z".into(),
            level: "INFO".into(),
            target: "ainb_hangar_daemon".into(),
            message: "daemon ready".into(),
            fields: vec![("task_id".into(), "t-aaa".into())],
        };
        let out = logs_pretty_line(&line);
        assert!(out.contains("daemon ready"), "message: {out}");
        assert!(out.contains("INFO"), "level: {out}");
        assert!(out.contains("ainb_hangar_daemon"), "target: {out}");
        assert!(out.contains("task_id=t-aaa"), "field tail: {out}");
    }

    #[test]
    fn logs_pretty_line_tolerates_missing_fields() {
        use ainb_hangar_core::logs::LogLine;
        // Every field absent — the printer must not panic and must yield a
        // (possibly empty) string (the P8.6 acceptance criterion).
        let empty = LogLine::default();
        assert_eq!(logs_pretty_line(&empty), "");

        // Only a message, no timestamp/level/target/fields.
        let msg_only = LogLine {
            message: "just a message".into(),
            ..LogLine::default()
        };
        assert_eq!(logs_pretty_line(&msg_only), "just a message");
    }

    #[test]
    fn skill_renderers_emit_name_files_description() {
        use ainb_hangar_core::ids::SkillId;
        use ainb_hangar_core::skill::{SkillFileInput, SkillName, SkillWithFiles};
        let skill = SkillWithFiles {
            id: SkillId::from_str("01SKILL").unwrap(),
            workspace_id: "ws-1".into(),
            name: SkillName::new("commit").unwrap(),
            description: Some("make commits".into()),
            content: Some("body".into()),
            files: vec![SkillFileInput::new("SKILL.md", "body")],
        };
        assert!(skill_line(&skill).contains("commit"));
        assert!(skill_line(&skill).contains("files=1"));
        assert!(skill_to_json(&skill).contains("\"name\":\"commit\""));
        assert!(skill_to_json(&skill).contains("\"files\":1"));
        assert!(skill_csv_row(&skill).starts_with("commit,1,"));
        assert!(skill_md_row(&skill).contains("| commit |"));
    }

    #[test]
    fn parses_templates_list() {
        let cmd = parse_hangar(&["ainb", "hangar", "templates", "list"]);
        assert!(matches!(
            cmd,
            HangarCommand::Templates(TemplatesCommand::List)
        ));
    }

    #[test]
    fn parses_templates_show_with_name() {
        let cmd = parse_hangar(&["ainb", "hangar", "templates", "show", "code-reviewer"]);
        let HangarCommand::Templates(TemplatesCommand::Show(args)) = cmd else {
            panic!("expected templates show, got {cmd:?}");
        };
        assert_eq!(args.name, "code-reviewer");
    }

    #[test]
    fn parses_templates_use_with_workspace_and_agent_name() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "templates",
            "use",
            "code-reviewer",
            "--workspace",
            "default",
            "--agent-name",
            "my-reviewer",
        ]);
        let HangarCommand::Templates(TemplatesCommand::Use(args)) = cmd else {
            panic!("expected templates use, got {cmd:?}");
        };
        assert_eq!(args.name, "code-reviewer");
        assert_eq!(args.workspace.as_deref(), Some("default"));
        assert_eq!(args.agent_name.as_deref(), Some("my-reviewer"));
    }

    #[test]
    fn parses_templates_use_defaults() {
        let cmd = parse_hangar(&["ainb", "hangar", "templates", "use", "planner"]);
        let HangarCommand::Templates(TemplatesCommand::Use(args)) = cmd else {
            panic!("expected templates use, got {cmd:?}");
        };
        assert_eq!(args.name, "planner");
        assert!(args.workspace.is_none());
        assert!(args.agent_name.is_none());
    }

    #[test]
    fn template_renderers_emit_name_skills_description() {
        use ainb_hangar_core::template::AgentTemplate;
        let t = AgentTemplate {
            name: "code-reviewer".into(),
            description: "review code".into(),
            agent_md_path: "engineering/code-reviewer.md".into(),
            instructions: "be careful".into(),
            skills: vec!["commit".into(), "find-missing-tests".into()],
        };
        assert!(template_line(&t).contains("code-reviewer"));
        assert!(template_line(&t).contains("skills=2"));
        assert!(template_detail(&t).contains("instructions:"));
        assert!(template_detail(&t).contains("commit, find-missing-tests"));
        assert!(template_to_json(&t).contains("\"name\":\"code-reviewer\""));
        assert!(template_to_json(&t).contains("\"skills\":[\"commit\",\"find-missing-tests\"]"));
        assert!(template_csv_row(&t).starts_with("code-reviewer,commit find-missing-tests,"));
        assert!(template_md_row(&t).contains("| code-reviewer |"));
    }

    #[test]
    fn render_template_list_handles_empty() {
        // Smoke: empty list path does not panic in any format.
        for fmt in [
            OutputFormat::Text,
            OutputFormat::Json,
            OutputFormat::Csv,
            OutputFormat::Markdown,
        ] {
            render_template_list(&[], fmt);
        }
    }

    #[test]
    fn json_string_escapes_quotes_and_control() {
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("x\ny"), "\"x\\ny\"");
    }

    #[test]
    fn parses_config_env_allow_list() {
        let cmd = parse_hangar(&["ainb", "hangar", "config", "env.allow", "list"]);
        assert!(matches!(
            cmd,
            HangarCommand::Config(ConfigCommand::EnvAllow(EnvAllowCommand::List))
        ));
    }

    #[test]
    fn parses_config_env_allow_add_with_key() {
        let cmd = parse_hangar(&["ainb", "hangar", "config", "env.allow", "add", "FOO_BAR"]);
        let HangarCommand::Config(ConfigCommand::EnvAllow(EnvAllowCommand::Add(args))) = cmd else {
            panic!("expected config env.allow add, got {cmd:?}");
        };
        assert_eq!(args.key, "FOO_BAR");
    }

    #[test]
    fn parses_config_env_allow_remove_with_key() {
        let cmd = parse_hangar(&["ainb", "hangar", "config", "env.allow", "remove", "BAZ_QUX"]);
        let HangarCommand::Config(ConfigCommand::EnvAllow(EnvAllowCommand::Remove(args))) = cmd
        else {
            panic!("expected config env.allow remove, got {cmd:?}");
        };
        assert_eq!(args.key, "BAZ_QUX");
    }

    #[test]
    fn json_string_array_renders_compact_array() {
        assert_eq!(json_string_array(["A", "B"].into_iter()), "[\"A\",\"B\"]");
        assert_eq!(json_string_array(std::iter::empty()), "[]");
    }

    #[test]
    fn parses_token_create_scope_and_name() {
        let cmd = parse_hangar(&[
            "ainb", "hangar", "auth", "token", "create", "--scope", "write", "--name", "ci",
        ]);
        let HangarCommand::Auth(AuthCommand::Token(TokenCommand::Create(args))) = cmd else {
            panic!("expected auth token create, got {cmd:?}");
        };
        assert_eq!(args.scope, TokenScope::Write);
        assert_eq!(args.name.as_deref(), Some("ci"));
    }

    #[test]
    fn token_create_defaults_to_read_scope() {
        let cmd = parse_hangar(&["ainb", "hangar", "auth", "token", "create"]);
        let HangarCommand::Auth(AuthCommand::Token(TokenCommand::Create(args))) = cmd else {
            panic!("expected auth token create, got {cmd:?}");
        };
        assert_eq!(args.scope, TokenScope::Read);
    }

    #[test]
    fn parses_token_list_and_revoke() {
        let cmd = parse_hangar(&["ainb", "hangar", "auth", "token", "list"]);
        assert!(matches!(
            cmd,
            HangarCommand::Auth(AuthCommand::Token(TokenCommand::List))
        ));

        let cmd = parse_hangar(&["ainb", "hangar", "auth", "token", "revoke", "pat-1"]);
        let HangarCommand::Auth(AuthCommand::Token(TokenCommand::Revoke(args))) = cmd else {
            panic!("expected auth token revoke, got {cmd:?}");
        };
        assert_eq!(args.id, "pat-1");
    }

    #[test]
    fn parses_hidden_daemon_token_create() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "auth",
            "daemon-token",
            "create",
            "--runtime-id",
            "rt-1",
        ]);
        let HangarCommand::Auth(AuthCommand::DaemonToken(DaemonTokenCommand::Create(args))) = cmd
        else {
            panic!("expected daemon-token create, got {cmd:?}");
        };
        assert_eq!(args.runtime_id, "rt-1");
    }

    #[test]
    fn token_create_output_shows_plaintext_once_with_warning() {
        let out = token_create_output("pat-1", None, "ainb_SECRETBODY");
        assert!(
            out.contains("ainb_SECRETBODY"),
            "plaintext must be printed once"
        );
        assert_eq!(out.matches("ainb_SECRETBODY").count(), 1, "exactly once");
        assert!(
            out.to_lowercase().contains("once") && out.to_lowercase().contains("not recoverable"),
            "must warn there is no second chance: {out}"
        );
    }

    #[test]
    fn token_create_output_echoes_advisory_name() {
        let out = token_create_output("pat-1", Some("ci-bot"), "ainb_SECRETBODY");
        assert!(
            out.contains("ci-bot"),
            "advisory --name must be echoed: {out}"
        );
        // The label is cosmetic; the plaintext is still shown exactly once.
        assert_eq!(out.matches("ainb_SECRETBODY").count(), 1);
    }

    #[test]
    fn token_scope_round_trips_to_storage_string() {
        assert_eq!(TokenScope::Read.as_str(), "read");
        assert_eq!(TokenScope::Write.as_str(), "write");
        assert_eq!(TokenScope::Admin.as_str(), "admin");
    }

    #[test]
    fn pat_list_renderers_never_emit_a_plaintext() {
        // A PatRecord only ever carries the digest, never a plaintext. Render it
        // through every format and confirm no `ainb_`/`mdt_` prefixed body
        // appears — the list surface cannot leak a secret.
        let rec = PatRecord {
            id: "pat-1".into(),
            user_id: "user-1".into(),
            sha256_token: "a".repeat(64),
            scope: Some("read".into()),
            created_at: 1_700_000_000_000,
            last_used: None,
        };
        for rendered in [
            pat_line(&rec),
            pat_csv_row(&rec),
            pat_md_row(&rec),
            pat_to_json(&rec),
        ] {
            assert!(
                !rendered.contains("ainb_") && !rendered.contains("mdt_"),
                "token list output must never contain a plaintext token: {rendered}"
            );
            assert!(
                !rendered.contains(&"a".repeat(64)),
                "token list output must not even leak the stored digest: {rendered}"
            );
        }
    }

    #[test]
    fn parses_autopilot_create_with_all_flags() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "autopilot",
            "create",
            "--name",
            "smoke",
            "--cron",
            "*/5 * * * *",
            "--agent",
            "ag-1",
            "--instructions",
            "say hi",
            "--max-concurrent-runs",
            "3",
            "--workspace",
            "default",
        ]);
        let HangarCommand::Autopilot(AutopilotCommand::Create(args)) = cmd else {
            panic!("expected autopilot create, got {cmd:?}");
        };
        assert_eq!(args.name, "smoke");
        assert_eq!(args.cron, "*/5 * * * *");
        assert_eq!(args.agent, "ag-1");
        assert_eq!(args.instructions.as_deref(), Some("say hi"));
        assert_eq!(args.max_concurrent_runs, 3);
        assert_eq!(args.workspace.as_deref(), Some("default"));
    }

    #[test]
    fn parses_autopilot_create_defaults() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "autopilot",
            "create",
            "--name",
            "n",
            "--cron",
            "0 9 * * *",
            "--agent",
            "ag-1",
        ]);
        let HangarCommand::Autopilot(AutopilotCommand::Create(args)) = cmd else {
            panic!("expected autopilot create, got {cmd:?}");
        };
        assert_eq!(args.max_concurrent_runs, 1, "default concurrency is 1");
        assert!(args.instructions.is_none());
        assert!(args.workspace.is_none());
    }

    #[test]
    fn parses_autopilot_disable_enable_run_with_id() {
        for (verb, is) in [("disable", "disable"), ("enable", "enable"), ("run", "run")] {
            let cmd = parse_hangar(&["ainb", "hangar", "autopilot", verb, "ap-1"]);
            match (is, cmd) {
                ("disable", HangarCommand::Autopilot(AutopilotCommand::Disable(a)))
                | ("enable", HangarCommand::Autopilot(AutopilotCommand::Enable(a)))
                | ("run", HangarCommand::Autopilot(AutopilotCommand::Run(a))) => {
                    assert_eq!(a.id, "ap-1");
                }
                (_, other) => panic!("expected autopilot {verb}, got {other:?}"),
            }
        }
    }

    /// Build a stored autopilot fixture for the render tests.
    fn sample_autopilot(enabled: bool) -> Autopilot {
        Autopilot {
            id: "01AP".into(),
            workspace_id: "ws-1".into(),
            agent_id: "ag-1".into(),
            name: "daily".into(),
            instructions: Some("triage".into()),
            cron_expr: "0 9 * * *".into(),
            max_concurrent_runs: 1,
            next_tick_at: Some(1_767_258_000_000),
            enabled,
            created_at: 0,
        }
    }

    #[test]
    fn autopilot_renderers_emit_name_cron_and_badge() {
        let ap = sample_autopilot(true);
        let last = Some("completed".to_string());

        let line = autopilot_line(&ap, last.as_deref());
        assert!(line.contains("daily"), "text missing name: {line}");
        assert!(line.contains("0 9 * * *"), "text missing cron: {line}");
        assert!(line.contains("[enabled]"), "text missing badge: {line}");
        assert!(line.contains("completed"), "text missing last_run: {line}");

        let json = autopilot_to_json(&ap, last.as_deref());
        assert!(
            json.contains("\"name\":\"daily\""),
            "json missing name: {json}"
        );
        assert!(
            json.contains("\"cron_expr\":\"0 9 * * *\""),
            "json missing cron: {json}"
        );
        assert!(
            json.contains("\"enabled\":true"),
            "json missing enabled: {json}"
        );
        assert!(
            json.contains("\"last_run\":\"completed\""),
            "json missing last_run: {json}"
        );

        assert!(autopilot_csv_row(&ap, last.as_deref()).contains("0 9 * * *"));
        assert!(autopilot_md_row(&ap, last.as_deref()).contains("| daily |"));
    }

    #[test]
    fn autopilot_renderers_show_disabled_badge_and_no_run() {
        let ap = sample_autopilot(false);
        assert!(autopilot_line(&ap, None).contains("[disabled]"));
        assert!(autopilot_line(&ap, None).contains("last_run=-"));
        assert!(autopilot_to_json(&ap, None).contains("\"enabled\":false"));
        assert!(autopilot_to_json(&ap, None).contains("\"last_run\":null"));
    }
}
