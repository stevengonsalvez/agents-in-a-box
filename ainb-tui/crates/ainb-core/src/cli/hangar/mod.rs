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
//! | `hangar issue create`         | [`ainb_hangar_store::repo::issue::IssueRepo::insert`] + [`ainb_hangar_store::repo::issue::IssueRepo::set_origin`] |
//! | `hangar issue list`           | `IssueRepo::list_by_workspace_state`                     |
//! | `hangar issue show`           | `IssueRepo::get_by_id`                                   |
//! | `hangar task list`            | `ainb_hangar_store::repo::task::TaskRepo`                |
//! | `hangar task cancel`          | `ainb_hangar_store::service::cancel::CancelTaskService`  |
//! | `hangar task retry`           | `ainb_hangar_store::service::retry::RetryService`        |
//! | `hangar beads reconcile`      | `ainb_hangar_daemon::beads_sync::reconcile`              |
//! | `hangar daemon status`        | PID-file + socket liveness + db reachability             |
//! | `hangar daemon run`           | [`ainb_hangar_daemon::boot`] (foreground)                |
//! | `hangar daemon start`         | spawn the daemon binary as a background child + PID file |
//! | `hangar daemon stop`          | signal the exact recorded PID via `nix` + remove PID file|
//! | `hangar daemon restart`       | `stop` then `start`                                      |
//! | `hangar daemon setup`         | ensure db + socket token, then `start`                   |
//!
//! # Deliberately NOT wired
//!
//! `init` and `tui` land in later phases. The
//! [`HangarCommand`](crate::cli::registry) subtree is built with derive
//! `Subcommand`s, so a later phase slots a new verb in by adding a variant — no
//! stubs are shipped for unimplemented verbs today.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};

use ainb_hangar_core::acceptance::AcceptanceCriterion;
use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::SystemClock;
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::origin::IssueOrigin;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot::Autopilot;
use ainb_hangar_store::repo::autopilot_rule_version::AutopilotRuleVersionRepo;
use ainb_hangar_store::repo::issue::{Issue, IssueRepo, NewIssue};
use ainb_hangar_store::repo::task::{Task, TaskRepo};
use ainb_hangar_store::repo::token::{PatRecord, PatRepo, mint_daemon_token, mint_pat};
use ainb_hangar_store::service::cancel::CancelTaskService;
use ainb_hangar_store::service::retry::{RetryDecision, RetryService};

use crate::cli::OutputFormat;

/// The default workspace slug + owner details now live in
/// [`ainb_hangar_store::bootstrap`] — the single shared, idempotent lay-down the
/// CLI, the daemon boot seed, and `agent_create` all delegate to. The wrappers
/// [`ensure_default_workspace`] / [`find_default_workspace`] / [`default_owner_id`]
/// keep the existing CLI callers untouched.
///
/// Member id used as the default issue creator (`member:stevie`).
const DEFAULT_CREATOR_ID: &str = "stevie";
/// Lifecycle state a freshly-created issue lands in.
const DEFAULT_ISSUE_STATE: &str = "open";

// The single derive enum the registry's `HangarCommand` augments onto the root
// `ainb` command. Each variant is a noun group whose inner enum carries the
// verbs. The user-facing `about` is set in `cli/registry.rs` (`.about()` after
// `augment_subcommands`). Keep the doc-comment below a SINGLE line — a
// multi-line doc also becomes clap `long_about` and would leak into
// `ainb hangar --help`.
/// Hangar managed-agents control plane.
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
    /// List, re-role, and remove workspace members.
    #[command(subcommand)]
    Member(MemberCommand),
    /// Create squads, manage membership, and view squad status + leader.
    #[command(subcommand)]
    Squad(SquadCommand),
    /// Create and control cron-scheduled autopilots.
    #[command(subcommand)]
    Autopilot(AutopilotCommand),
    /// View + set per-workspace config (context prompt, issue prefix, repo
    /// whitelist).
    #[command(subcommand)]
    Workspace(WorkspaceCommand),
    /// Read the daemon's structured logs.
    #[command(subcommand)]
    Logs(LogsCommand),
    /// Define and archive a workspace's custom issue properties.
    #[command(subcommand)]
    Property(PropertyCommand),
    /// Post issue comments and preview their `@`-mention routing.
    #[command(subcommand)]
    Comment(CommentCommand),
    /// Read an actor's notification inbox.
    #[command(subcommand)]
    Inbox(InboxCommand),
}

/// `hangar comment <verb>` — post a comment and see EXACTLY where its
/// `@`-mentions went (multica parity #2-rest).
///
/// Store-direct, like `issue timeline`: no daemon required, which is what makes
/// the routing behaviour provable against a bare SQLite file.
#[derive(Subcommand, Debug)]
pub enum CommentCommand {
    /// Post a comment on an issue and print one row per routed `@`-mention.
    Add(CommentAddArgs),
    /// DRY-RUN the mention router over a draft body: identical resolution and
    /// identical gates, zero writes.
    Preview(CommentAddArgs),
}

/// `hangar inbox <verb>` — the read surface that proves a mention of a human
/// actually landed on that human.
#[derive(Subcommand, Debug)]
pub enum InboxCommand {
    /// List one actor's inbox entries, newest first.
    List(InboxListArgs),
}

/// Arguments shared by `hangar comment add` and `hangar comment preview`.
#[derive(Args, Debug)]
pub struct CommentAddArgs {
    /// Issue id (ULID) to comment on.
    #[arg(long)]
    pub issue: String,
    /// The comment body. `@handle` and `[@Label](mention://type/id)` both route.
    #[arg(long)]
    pub body: String,
    /// The author as a canonical actor-ref. `member:me` is the local operator,
    /// which the invocation gate resolves to the workspace owner.
    #[arg(long, default_value = "member:me")]
    pub author: String,
    /// The comment this one replies to — drives the reply-parent fallback and
    /// multica's parent-mention inheritance.
    #[arg(long)]
    pub parent: Option<String>,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar inbox list`.
#[derive(Args, Debug)]
pub struct InboxListArgs {
    /// Whose inbox to read, as `member:<user-id>` / `agent:<agent-id>`.
    #[arg(long, default_value = "member:me")]
    pub recipient: String,
    /// Show only UNREAD entries.
    #[arg(long)]
    pub unread: bool,
    /// How many entries to show, newest first.
    #[arg(long, default_value_t = 50)]
    pub limit: i64,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar property <verb>` — the per-workspace CUSTOM PROPERTY catalog
/// (multica parity #17).
///
/// Values on issues are keyed by the definition's ID, never by its name, so
/// renaming a property (`define` again with a new `--name`) is a catalog-only
/// write that touches zero issue rows. A definition is ARCHIVED, never deleted:
/// its stored values survive and render again if it is un-archived.
#[derive(Subcommand, Debug)]
pub enum PropertyCommand {
    /// Create or update ONE custom property definition (idempotent by key).
    Define(PropertyDefineArgs),
    /// List the workspace's custom property catalog, in render order.
    List(PropertyListArgs),
    /// Archive (or un-archive) a definition. NEVER deletes stored values.
    Archive(PropertyArchiveArgs),
}

/// Arguments for `hangar property define`.
#[derive(Args, Debug)]
pub struct PropertyDefineArgs {
    /// Stable slug the CLI and RPC address this property by.
    #[arg(long)]
    pub key: String,
    /// Display label. Defaults to the key on a new definition; changing it is
    /// a free rename.
    #[arg(long)]
    pub name: Option<String>,
    /// Value type: text, number, select, multi_select, date, checkbox, url.
    #[arg(long)]
    pub kind: Option<String>,
    /// One catalogued option (repeat). Required for select / multi_select.
    #[arg(long = "option")]
    pub options: Vec<String>,
    /// Render order within the workspace (ascending).
    #[arg(long)]
    pub position: Option<i64>,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar property list`.
#[derive(Args, Debug)]
pub struct PropertyListArgs {
    /// Include archived definitions too.
    #[arg(long)]
    pub include_archived: bool,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar property archive`.
#[derive(Args, Debug)]
pub struct PropertyArchiveArgs {
    /// The definition's stable slug.
    #[arg(long)]
    pub key: String,
    /// Un-archive instead: bring the definition (and its values) back.
    #[arg(long)]
    pub unarchive: bool,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar workspace <verb>`.
///
/// The per-workspace config surface (e38.21): `config` sets one or more of the
/// workspace's agent-run config knobs, `show` renders the current config. Every
/// verb is workspace-scoped (`--workspace`, else the bootstrapped `default`).
///
/// The three knobs each take effect at dispatch / create time:
/// - `context_prompt` is injected into every agent run in the workspace as a
///   `CLAUDE.md` in the task's working dir,
/// - `issue_prefix` is prepended to the title of every issue created in the
///   workspace,
/// - `repo_whitelist` gates which repositories a workspace task may check out
///   (persisted + validated; the checkout flow that consumes it lands later).
#[derive(Subcommand, Debug)]
pub enum WorkspaceCommand {
    /// Create a new workspace (slug + display name).
    Create(WorkspaceCreateArgs),
    /// List every workspace on this instance.
    List(WorkspaceListArgs),
    /// Set one or more of the workspace's config knobs.
    Config(WorkspaceConfigArgs),
    /// Show the workspace's current config.
    Show(WorkspaceShowArgs),
}

/// Arguments for `hangar workspace create`.
///
/// Refused when the instance has workspace creation locked down
/// (`daemon config set workspace.creation_disabled true`, or the
/// `HANGAR_DISABLE_WORKSPACE_CREATION` env override) — the refusal is
/// store-side and exits non-zero.
#[derive(Args, Debug)]
pub struct WorkspaceCreateArgs {
    /// Short handle for the workspace (`^[a-z0-9]+(-[a-z0-9]+)*$`), unique
    /// host-wide.
    #[arg(long)]
    pub slug: String,
    /// Human-readable display name.
    #[arg(long)]
    pub name: String,
    /// Optional prefix prepended to a newly-created issue's title in this
    /// workspace (e.g. `OPS`). Omitted leaves titles verbatim.
    #[arg(long)]
    pub issue_prefix: Option<String>,
}

/// Arguments for `hangar workspace list` — host-wide, so no `--workspace`.
#[derive(Args, Debug)]
pub struct WorkspaceListArgs {}

/// Arguments for `hangar workspace config`.
///
/// Each `--…` flag overwrites that knob; the matching `--clear-…` flag unsets it
/// (writes NULL — back to the v1 "not configured" behaviour). A flag that is not
/// given leaves the stored value untouched. `--clear-…` and the value flag for
/// the same knob are mutually exclusive.
#[derive(Args, Debug)]
pub struct WorkspaceConfigArgs {
    /// Set the context prompt injected into every agent run as a `CLAUDE.md`.
    #[arg(long, conflicts_with = "clear_context_prompt")]
    pub context_prompt: Option<String>,
    /// Unset the context prompt (back to no per-workspace context).
    #[arg(long)]
    pub clear_context_prompt: bool,
    /// Set the prefix prepended to a newly-created issue's title (e.g. `[OPS] `).
    #[arg(long, conflicts_with = "clear_issue_prefix")]
    pub issue_prefix: Option<String>,
    /// Unset the issue prefix (titles used verbatim).
    #[arg(long)]
    pub clear_issue_prefix: bool,
    /// Set the repo whitelist as a comma-separated list of `owner/name` slugs
    /// (e.g. `org/api,org/web`). The empty string sets a configured-but-empty
    /// whitelist (allows nothing); use `--clear-repo-whitelist` to remove the gate.
    #[arg(long, conflicts_with = "clear_repo_whitelist", value_delimiter = ',')]
    pub repo_whitelist: Option<Vec<String>>,
    /// Unset the repo whitelist (no gate — every repo allowed).
    #[arg(long)]
    pub clear_repo_whitelist: bool,
    /// Workspace slug to configure. Defaults to the bootstrapped `default`
    /// workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar workspace show`.
#[derive(Args, Debug)]
pub struct WorkspaceShowArgs {
    /// Workspace slug to show. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
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
    /// Edit an autopilot's config (cron / agent / instructions / policy).
    /// A substantive edit appends a rule version naming the accountable human;
    /// a rename alone is cosmetic and mints none.
    Edit(AutopilotEditArgs),
    /// Show the autopilot's rule-version ledger (who published what, when).
    Versions(AutopilotVersionsArgs),
    /// Fire one tick immediately, bypassing the schedule (`--source` picks the
    /// trigger recorded on the run: `manual` by default, or `api`).
    Run(AutopilotRunNowArgs),
    /// Arm (or `--disable`) the bare programmatic `api` trigger.
    ApiTrigger(AutopilotIdArgs),
    /// List the autopilot's recent runs (status, trigger source, reason).
    Runs(AutopilotRunsArgs),
    /// Configure the HTTP webhook trigger (enable/disable, rotate secret, filter).
    Webhook(AutopilotWebhookArgs),
    /// List the autopilot's recent webhook deliveries (audit log).
    Deliveries(AutopilotDeliveriesArgs),
    /// Manage the rule's explicit WRITE-GRANT set (multica parity #27).
    #[command(subcommand)]
    Collaborator(AutopilotActorCommand),
    /// Manage the rule's STANDING subscriber list — every issue the rule spawns
    /// auto-subscribes it (multica parity #27).
    #[command(subcommand)]
    Subscriber(AutopilotActorCommand),
    /// Open or restrict who may WRITE this rule (multica parity #27).
    Access(AutopilotAccessArgs),
}

/// The add / remove / list verbs shared by `autopilot collaborator` and
/// `autopilot subscriber` — the addressed tuple is identical, so the shape is.
#[derive(Subcommand, Debug)]
pub enum AutopilotActorCommand {
    /// Add an actor to the set (idempotent; a re-add keeps the FIRST grant).
    Add(AutopilotActorArgs),
    /// Remove an actor from the set (idempotent).
    Remove(AutopilotActorArgs),
    /// List the set, oldest first.
    List(AutopilotActorArgs),
}

/// Arguments for every `hangar autopilot collaborator|subscriber` verb.
#[derive(Args, Debug)]
pub struct AutopilotActorArgs {
    /// The autopilot id (`autopilot.id`).
    #[arg(long)]
    pub id: String,
    /// The target actor, `member:<id>` / `agent:<id>`. Defaults to the local
    /// human (`member:me`).
    #[arg(long)]
    pub actor: Option<String>,
    /// Collaborator ADD only: `editor` (the default) grants write, `viewer`
    /// grants visibility only.
    #[arg(long)]
    pub role: Option<String>,
    /// The ACTING human, as the write gate's subject and the grant's
    /// attribution. Defaults to the local human.
    #[arg(long = "as-user")]
    pub as_user: Option<String>,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar autopilot access`.
#[derive(Args, Debug)]
pub struct AutopilotAccessArgs {
    /// The autopilot id (`autopilot.id`).
    #[arg(long)]
    pub id: String,
    /// `open` (any actor in the workspace may write — the default and every
    /// pre-0064 rule) or `restricted` (owner / workspace owner+admin / an
    /// explicit `editor` collaborator only).
    #[arg(long)]
    pub mode: String,
    /// The acting human (write gate subject + rule-version attribution).
    #[arg(long = "as-user")]
    pub as_user: Option<String>,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar autopilot webhook <id>`.
///
/// Configures the per-autopilot HTTP webhook trigger. By default it ENABLES the
/// webhook and mints a fresh HMAC signing secret, printing the webhook URL + the
/// secret ONCE (the secret is never shown again). `--rotate` mints a new secret
/// for an already-enabled webhook; `--disable` turns the webhook off (the secret
/// is cleared). `--event` sets the optional exact-match event filter;
/// `--clear-event` removes it. Workspace-scoped.
#[derive(Args, Debug)]
pub struct AutopilotWebhookArgs {
    /// The autopilot id (`autopilot.id`).
    pub id: String,
    /// Disable the webhook (clears the secret); mutually exclusive with
    /// `--rotate`.
    #[arg(long, conflicts_with = "rotate")]
    pub disable: bool,
    /// Mint a fresh signing secret for an already-enabled webhook (prints the new
    /// secret once).
    #[arg(long)]
    pub rotate: bool,
    /// Set the exact-match event filter (only this event name fires). Mutually
    /// exclusive with `--clear-event`.
    #[arg(long, conflicts_with = "clear_event")]
    pub event: Option<String>,
    /// Clear the event filter (fire on every signed request).
    #[arg(long = "clear-event")]
    pub clear_event: bool,
    /// The host:port the webhook ingress listens on, used only to render the URL
    /// hint (defaults to `127.0.0.1:8718`). The daemon's actual bind is set by
    /// `AINB_HANGAR_WEBHOOK_PORT`.
    #[arg(long = "url-host", default_value = "127.0.0.1:8718")]
    pub url_host: String,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar autopilot deliveries <id>`.
#[derive(Args, Debug)]
pub struct AutopilotDeliveriesArgs {
    /// The autopilot id (`autopilot.id`).
    pub id: String,
    /// Maximum number of deliveries to show (latest-first).
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
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
    /// Maximum simultaneous in-flight runs before the concurrency policy applies.
    #[arg(long = "max-concurrent-runs", default_value_t = 1)]
    pub max_concurrent_runs: i64,
    /// What a fired tick materialises: `run-only` (a task with no issue, the
    /// default) or `create-issue` (an issue plus a task against it).
    #[arg(long = "execution-mode", value_enum, default_value_t = ExecutionModeArg::RunOnly)]
    pub execution_mode: ExecutionModeArg,
    /// What the scheduler does when a tick comes due at the in-flight limit:
    /// `skip` (drop it, the default), `queue` (fire it anyway to run after the
    /// in-flight one), or `replace` (supersede the in-flight run and fire fresh).
    #[arg(long = "concurrency-policy", value_enum, default_value_t = ConcurrencyPolicyArg::Skip)]
    pub concurrency_policy: ConcurrencyPolicyArg,
    /// The ACCOUNTABLE HUMAN for this rule (`user.id` or email). Recorded on
    /// rule-version v1, which creation writes in the same transaction. Omitted
    /// defaults to the local human (`member:me`) — a CLI create always has a
    /// human at the keyboard.
    #[arg(long = "as-user")]
    pub as_user: Option<String>,
    /// Workspace slug to create in. Defaults to the bootstrapped `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// CLI surface of [`ExecutionMode`](ainb_hangar_store::repo::autopilot::ExecutionMode).
///
/// A thin clap [`ValueEnum`] mirror so the flag accepts the kebab-case
/// `run-only` / `create-issue` values and maps onto the store enum at dispatch.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ExecutionModeArg {
    /// Enqueue a task with no issue (the v1 default).
    RunOnly,
    /// Create an issue, then enqueue a task against it.
    CreateIssue,
}

impl From<ExecutionModeArg> for ainb_hangar_store::repo::autopilot::ExecutionMode {
    fn from(a: ExecutionModeArg) -> Self {
        match a {
            ExecutionModeArg::RunOnly => Self::RunOnly,
            ExecutionModeArg::CreateIssue => Self::CreateIssue,
        }
    }
}

/// CLI surface of
/// [`ConcurrencyPolicy`](ainb_hangar_store::repo::autopilot::ConcurrencyPolicy).
///
/// A thin clap [`ValueEnum`] mirror so the flag accepts the lower-case
/// `skip` / `queue` / `replace` values and maps onto the store enum at dispatch.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ConcurrencyPolicyArg {
    /// Drop a tick that comes due at the in-flight limit (the v1 default).
    Skip,
    /// Fire the tick anyway; the queue runs it after the in-flight one.
    Queue,
    /// Supersede the in-flight run and fire fresh.
    Replace,
}

impl From<ConcurrencyPolicyArg> for ainb_hangar_store::repo::autopilot::ConcurrencyPolicy {
    fn from(a: ConcurrencyPolicyArg) -> Self {
        match a {
            ConcurrencyPolicyArg::Skip => Self::Skip,
            ConcurrencyPolicyArg::Queue => Self::Queue,
            ConcurrencyPolicyArg::Replace => Self::Replace,
        }
    }
}

/// Arguments for `hangar autopilot list`.
#[derive(Args, Debug)]
pub struct AutopilotListArgs {
    /// Workspace slug to list. Defaults to the bootstrapped `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for the id-only autopilot verbs (`disable`, `enable`,
/// `api-trigger`).
#[derive(Args, Debug)]
pub struct AutopilotIdArgs {
    /// The autopilot id (`autopilot.id`).
    pub id: String,
    /// Turn the trigger OFF instead of on (`api-trigger` only).
    #[arg(long)]
    pub disable: bool,
    /// The accountable human for this publish (`user.id` or email). Pausing,
    /// resuming and arming a trigger are all SUBSTANTIVE publishes, so each
    /// stamps a rule version. Defaults to the local human (`member:me`).
    #[arg(long = "as-user")]
    pub as_user: Option<String>,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Which trigger a manual `hangar autopilot run` records on the run it creates
/// (`autopilot_run.source`, migration 0057).
///
/// `manual` is the operator's explicit override and always allowed. `api` is the
/// bare programmatic trigger and is REFUSED unless the autopilot has armed it
/// (`hangar autopilot api-trigger <id>`) — a half-configured trigger is never
/// firable, exactly like the webhook one.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunSourceArg {
    /// An operator firing by hand (the default).
    #[default]
    Manual,
    /// The bare programmatic `api` trigger; requires it to be armed.
    Api,
}

/// Arguments for `hangar autopilot run <id>`.
#[derive(Args, Debug)]
pub struct AutopilotRunNowArgs {
    /// The autopilot id (`autopilot.id`).
    pub id: String,
    /// Which trigger to record on the run (`manual` | `api`).
    #[arg(long, value_enum, default_value_t = RunSourceArg::Manual)]
    pub source: RunSourceArg,
    /// The human firing it (`user.id` or email). A `manual` run attributes to
    /// this human (`direct_human`) — them, not the rule's owner. An `api` run
    /// stays UNATTENDED (`rule_owner`), matching multica. Defaults to the local
    /// human (`member:me`).
    #[arg(long = "as-user")]
    pub as_user: Option<String>,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar autopilot edit <id>` (multica parity #14).
///
/// An all-optional patch: an omitted flag leaves the field alone. `--name` is
/// COSMETIC — changing only it lands the rename but mints no rule version, so a
/// title tweak never re-assigns blame for an unattended run.
#[derive(Args, Debug)]
pub struct AutopilotEditArgs {
    /// The autopilot id (`autopilot.id`).
    pub id: String,
    /// New display name (cosmetic on its own).
    #[arg(long)]
    pub name: Option<String>,
    /// New cron expression (UTC, 5-field) — revalidated before any write.
    #[arg(long)]
    pub cron: Option<String>,
    /// Re-target the rule at a different agent (`agent.id`).
    #[arg(long)]
    pub agent: Option<String>,
    /// New instructions handed to the agent on every tick.
    #[arg(long, conflicts_with = "clear_instructions")]
    pub instructions: Option<String>,
    /// Clear the instructions entirely.
    #[arg(long = "clear-instructions")]
    pub clear_instructions: bool,
    /// New maximum simultaneous in-flight runs.
    #[arg(long = "max-concurrent-runs")]
    pub max_concurrent_runs: Option<i64>,
    /// New execution mode (`run-only` | `create-issue`).
    #[arg(long = "execution-mode", value_enum)]
    pub execution_mode: Option<ExecutionModeArg>,
    /// New concurrency policy (`skip` | `queue` | `replace`).
    #[arg(long = "concurrency-policy", value_enum)]
    pub concurrency_policy: Option<ConcurrencyPolicyArg>,
    /// The ACCOUNTABLE HUMAN for this edit (`user.id` or email) — the name
    /// recorded on the minted rule version. Defaults to the local human
    /// (`member:me`).
    #[arg(long = "as-user")]
    pub as_user: Option<String>,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar autopilot versions <id>` (the rule-version ledger).
#[derive(Args, Debug)]
pub struct AutopilotVersionsArgs {
    /// The autopilot id (`autopilot.id`).
    pub id: String,
    /// Maximum number of versions to show (newest-first).
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
    /// Workspace slug the autopilot belongs to. Defaults to `default`.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar autopilot runs <id>` (the run-history read surface).
#[derive(Args, Debug)]
pub struct AutopilotRunsArgs {
    /// The autopilot id (`autopilot.id`).
    pub id: String,
    /// Maximum number of runs to show (latest-first).
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
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
    /// Create a new agent from scratch (fills workspace/runtime/owner behind the scenes).
    Create(AgentCreateArgs),
    /// List the workspace's agents (active by default; `--all` includes archived).
    List(AgentListArgs),
    /// Edit an agent's config knobs (model / args / MCP / thinking / env / name).
    Edit(AgentEditArgs),
    /// Archive an agent (hide it from the active picker).
    Archive(AgentArchiveArgs),
    /// Un-archive an agent (restore it to the active picker).
    Unarchive(AgentArchiveArgs),
    /// Set an agent's invocation permission mode (gap #8: `private`/`public_to`).
    Permission(AgentPermissionArgs),
    /// Manage an agent's invocation allow-list (add/revoke/list a target).
    Allow(AgentAllowArgs),
    /// Report whether a user (or agent actor) may invoke an agent (`ALLOW`/`DENY`).
    CanInvoke(AgentCanInvokeArgs),
    /// Show an agent's per-agent env: variable NAMES only, values masked.
    Env(AgentEnvArgs),
}

/// Arguments for `hangar agent env` — the REDACTED read of one agent's env.
///
/// Deliberately has no `--reveal` (parity #30 deviation D-1): hangar masks
/// unconditionally, so there is exactly one plaintext egress (the provider
/// child's environment at exec) and no CLI affordance that re-opens the hole.
#[derive(Args, Debug)]
pub struct AgentEnvArgs {
    /// Agent id (ULID) to inspect.
    pub id: String,
    /// Workspace slug the agent belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar agent permission`.
#[derive(Args, Debug)]
pub struct AgentPermissionArgs {
    /// Agent id (ULID) to set the permission mode on.
    pub id: String,
    /// The new mode: `private` (owner-only, deny-by-default) or `public_to` (the
    /// allow-list decides).
    #[arg(long)]
    pub mode: String,
    /// Workspace slug the agent belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar agent allow`.
///
/// Adds (or, with `--revoke`, removes) one invocation-target row, or lists the
/// current allow-list with `--list`. Exactly one target flag (`--workspace` /
/// `--member` / `--team`) is required unless `--list` is given. Adding a target
/// implies `permission_mode=public_to` (mirroring multica's "share ⇒ public_to").
#[derive(Args, Debug)]
pub struct AgentAllowArgs {
    /// Agent id (ULID) whose allow-list to manage.
    pub id: String,
    /// Grant/revoke the WHOLE workspace (a workspace target). Mutually exclusive
    /// with `--member` / `--team`.
    #[arg(long, conflicts_with_all = ["member", "team"])]
    pub workspace: bool,
    /// Grant/revoke a specific member (a user id or email). Mutually exclusive with
    /// `--workspace` / `--team`.
    #[arg(long, conflicts_with_all = ["workspace", "team"])]
    pub member: Option<String>,
    /// Grant/revoke a reserved team target (inert in V1). Mutually exclusive with
    /// `--workspace` / `--member`.
    #[arg(long, conflicts_with_all = ["workspace", "member"])]
    pub team: Option<String>,
    /// Remove the named target instead of adding it.
    #[arg(long)]
    pub revoke: bool,
    /// Print the current allow-list (ignores the target flags).
    #[arg(long)]
    pub list: bool,
    /// Workspace slug the agent belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace_slug: Option<String>,
}

/// Arguments for `hangar agent can-invoke`.
#[derive(Args, Debug)]
pub struct AgentCanInvokeArgs {
    /// Agent id (ULID) to test invocation on.
    pub id: String,
    /// The invoking user id or email to judge the run by.
    #[arg(long = "as")]
    pub as_user: String,
    /// Treat the invoker as an `agent` actor (no resolved originator) rather than a
    /// `member`. Exercises the A2A / workspaceBroad path.
    #[arg(long)]
    pub actor: Option<String>,
    /// Workspace slug the agent belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar agent create`.
///
/// The daemon-less create-from-scratch path: fills the workspace / runtime /
/// owner FKs behind the scenes so the caller supplies only a `name`. `provider`
/// is optional (`claude`/`codex`/`copilot`, default `claude`) and is HONOURED at
/// dispatch — the daemon spawns that provider's backend for the agent's tasks.
#[derive(Args, Debug)]
pub struct AgentCreateArgs {
    /// The new agent's name.
    #[arg(long)]
    pub name: String,
    /// Provider to record (`claude`/`codex`/`copilot`); defaults to `claude`.
    #[arg(long)]
    pub provider: Option<String>,
    /// Optional per-agent model override (e.g. `sonnet`, `gpt-5-codex`).
    #[arg(long)]
    pub model: Option<String>,
    /// Optional instructions / system prompt for the agent.
    #[arg(long)]
    pub instructions: Option<String>,
    /// Optional short blurb rendered beside the agent (≤255 characters).
    #[arg(long)]
    pub description: Option<String>,
    /// Optional avatar token (e.g. `emoji:🦊`); omitted mints a random emoji.
    #[arg(long)]
    pub avatar: Option<String>,
    /// Optional Codex service tier (e.g. `priority`); omitted inherits the local
    /// Codex config. Stored + surfaced only — no dispatch-time override yet.
    #[arg(long = "service-tier")]
    pub service_tier: Option<String>,
    /// Workspace slug to create the agent in. Defaults to the bootstrapped
    /// `default` workspace (created if absent).
    #[arg(long)]
    pub workspace: Option<String>,
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
    // NOTE: each of the three env flags keeps a SINGLE-paragraph doc comment on
    // purpose — a second paragraph flips clap's whole `agent edit` help into the
    // long-help layout, which churns the generated `docs/tui/cli.md` reference.
    /// A `KEY=VALUE` env var for the agent (repeatable; ANY `--env` REPLACES the whole map). Visible in `ps` / shell history — prefer `--env-stdin` / `--env-file` for secrets.
    #[arg(long = "env", value_parser = parse_env_kv, action = clap::ArgAction::Append,
          conflicts_with_all = ["env_stdin", "env_file"])]
    pub env: Vec<(String, String)>,
    /// Read the whole env map from STDIN as a JSON object of string→string, keeping secrets off argv; `{}` clears it and empty input is an ERROR, not a clear
    #[arg(long = "env-stdin", conflicts_with_all = ["env", "env_file"])]
    pub env_stdin: bool,
    /// Read the whole env map from a FILE as a JSON object of string→string (same contract as `--env-stdin`)
    #[arg(long = "env-file", conflicts_with_all = ["env", "env_stdin"])]
    pub env_file: Option<std::path::PathBuf>,
    /// New token budget (rtk/headroom, migration 0042); omitted leaves it.
    /// Mutually exclusive with `--clear-token-budget`.
    #[arg(long = "token-budget", conflicts_with = "clear_token_budget")]
    pub token_budget: Option<i64>,
    /// Clear the token budget (back to unlimited); omitted leaves it.
    #[arg(long = "clear-token-budget")]
    pub clear_token_budget: bool,
    /// New description (≤255 characters); omitted leaves it. Pass `--description ""`
    /// to blank it (the column is NOT NULL, so `""` IS its cleared state).
    #[arg(long)]
    pub description: Option<String>,
    /// New avatar token; omitted leaves it. Mutually exclusive with
    /// `--clear-avatar`.
    #[arg(long, conflicts_with = "clear_avatar")]
    pub avatar: Option<String>,
    /// Clear the avatar; omitted leaves it.
    #[arg(long = "clear-avatar")]
    pub clear_avatar: bool,
    /// New Codex service tier; omitted leaves it. Mutually exclusive with
    /// `--clear-service-tier`.
    #[arg(long = "service-tier", conflicts_with = "clear_service_tier")]
    pub service_tier: Option<String>,
    /// Clear the service tier (back to inheriting the local Codex config).
    #[arg(long = "clear-service-tier")]
    pub clear_service_tier: bool,
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
    /// The `user.id` recorded as the archiving actor (migration 0052). Omitted
    /// defaults to the workspace owner — the ordinary single-operator archive.
    #[arg(long)]
    pub by: Option<String>,
}

/// `hangar member <verb>`.
///
/// The workspace-membership surface (e38.11): `list` shows the workspace's
/// members with their roles; `set-role` changes one member's role
/// (`owner`/`admin`/`member`); `remove` drops a member's membership (the `user`
/// row survives). Every verb is workspace-scoped (`--workspace`, else the
/// bootstrapped `default`). Both mutations guard the last-owner invariant — a
/// workspace must always keep at least one owner, so demoting or removing the sole
/// owner is rejected.
#[derive(Subcommand, Debug)]
pub enum MemberCommand {
    /// Add a human member (find-or-create the user by email, then join).
    Add(MemberAddArgs),
    /// List the workspace's members (email + role).
    List(MemberListArgs),
    /// Change a member's role (`owner` / `admin` / `member`).
    #[command(name = "set-role")]
    SetRole(MemberSetRoleArgs),
    /// Remove a member from the workspace (the user row survives).
    Remove(MemberRemoveArgs),
    /// Invite an email to join (pending until accepted — parity #18).
    Invite(MemberInviteArgs),
    /// List the workspace's live pending invitations.
    Invites(MemberInvitesArgs),
    /// Accept an invitation addressed to you (this is what adds the member).
    Accept(MemberInviteActArgs),
    /// Decline an invitation addressed to you (no member is created).
    Decline(MemberInviteActArgs),
    /// Withdraw a still-pending invitation (admin-side).
    Revoke(MemberInviteRevokeArgs),
}

/// Arguments for `hangar member invite` (parity #18).
#[derive(Args, Debug)]
pub struct MemberInviteArgs {
    /// The invitee's email. Normalised (trimmed + lowercased) by the store.
    #[arg(long)]
    pub email: String,
    /// The role the invitee will hold on accept: `admin` or `member` (default
    /// `member`). `owner` parses but is rejected — ownership is transferred,
    /// never invited.
    #[arg(long, value_enum, default_value_t = MemberRoleArg::Member)]
    pub role: MemberRoleArg,
    /// Email of the inviting member. Defaults to the bootstrapped workspace
    /// owner. Must already be a member of the workspace.
    #[arg(long)]
    pub from: Option<String>,
    /// Workspace slug to invite into. Defaults to the bootstrapped `default`
    /// workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar member invites` (parity #18).
#[derive(Args, Debug)]
pub struct MemberInvitesArgs {
    /// Workspace slug to list. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar member accept` / `hangar member decline` (parity #18).
#[derive(Args, Debug)]
pub struct MemberInviteActArgs {
    /// The invitation id to act on.
    pub invitation_id: String,
    /// The acting human's email. REQUIRED, not defaulted: hangar has no session,
    /// so the identity must be explicit or the ownership gate is theatre.
    #[arg(long = "as")]
    pub acting_as: String,
}

/// Arguments for `hangar member revoke` (parity #18).
#[derive(Args, Debug)]
pub struct MemberInviteRevokeArgs {
    /// The invitation id to withdraw.
    pub invitation_id: String,
    /// Workspace slug the invitation belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar member add`.
#[derive(Args, Debug)]
pub struct MemberAddArgs {
    /// The member's email (find-or-create the user by this address).
    #[arg(long)]
    pub email: String,
    /// The role to grant: `owner`, `admin`, or `member` (default `member`).
    #[arg(long, value_enum, default_value_t = MemberRoleArg::Member)]
    pub role: MemberRoleArg,
    /// Workspace slug to add the member to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar member list`.
#[derive(Args, Debug)]
pub struct MemberListArgs {
    /// Workspace slug to list. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar member set-role`.
#[derive(Args, Debug)]
pub struct MemberSetRoleArgs {
    /// The member's user id (`user.id`).
    pub user_id: String,
    /// The new role: `owner`, `admin`, or `member`.
    #[arg(value_enum)]
    pub role: MemberRoleArg,
    /// Workspace slug the member belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar member remove`.
#[derive(Args, Debug)]
pub struct MemberRemoveArgs {
    /// The member's user id (`user.id`) to remove.
    pub user_id: String,
    /// Workspace slug the member belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// The closed role set for `hangar member set-role`, mirroring the store's
/// [`MemberRole`](ainb_hangar_store::repo::member::MemberRole) and migration
/// 0001's `CHECK`. Constraining it at the CLI surface rejects a junk role before
/// it ever reaches the store.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberRoleArg {
    /// Full administrative control; a workspace must always keep one.
    Owner,
    /// Elevated management, short of ownership.
    Admin,
    /// A regular member.
    Member,
}

impl MemberRoleArg {
    /// Map the CLI role onto the store's [`MemberRole`].
    ///
    /// [`MemberRole`]: ainb_hangar_store::repo::member::MemberRole
    const fn to_repo(self) -> ainb_hangar_store::repo::member::MemberRole {
        use ainb_hangar_store::repo::member::MemberRole;
        match self {
            Self::Owner => MemberRole::Owner,
            Self::Admin => MemberRole::Admin,
            Self::Member => MemberRole::Member,
        }
    }
}

/// `hangar squad <verb>`.
///
/// The squads surface (e38.17): a squad is a workspace-scoped group of actors with
/// a designated LEADER. `list` is the status view — each squad with its leader and
/// members; `create` makes a squad with a leader actor-ref; `add-member` /
/// `remove-member` mutate membership. Every verb is workspace-scoped (`--workspace`,
/// else the bootstrapped `default`).
///
/// # Leader routing
///
/// A squad does NOT introduce a new actor kind. Work assigned to a squad routes
/// through its LEADER actor-ref: when the leader is an `agent`, a squad-assigned
/// task carries the leader's `agent_id` and the existing claim/dispatch path
/// reaches the leader — so the leader is the actor a squad's work lands on.
#[derive(Subcommand, Debug)]
pub enum SquadCommand {
    /// List the workspace's squads (name, leader, members) — the status view.
    List(SquadListArgs),
    /// Create a squad with a leader actor-ref (`agent:<id>` / `member:<id>`).
    Create(SquadCreateArgs),
    /// Add a member actor to a squad (`agent:<id>` / `member:<id>`).
    #[command(name = "add-member")]
    AddMember(SquadMemberArgs),
    /// Remove a member actor from a squad (`agent:<id>` / `member:<id>`).
    #[command(name = "remove-member")]
    RemoveMember(SquadMemberArgs),
    /// Route a task to the squad's LEADER (leader routing taking effect).
    Assign(SquadAssignArgs),
    /// Archive a squad: it leaves the active list and refuses new assignments.
    Archive(SquadArchiveArgs),
    /// Restore an archived squad (clears the archive audit stamp).
    Unarchive(SquadArchiveArgs),
    /// Set or clear an existing member's free-text role on a squad.
    #[command(name = "member-role")]
    MemberRole(SquadMemberRoleArgs),
    /// Show, set, or clear a squad's user-authored routing instructions.
    Instructions(SquadInstructionsArgs),
    /// Print the leader briefing this squad would inject into a leader run.
    Briefing(SquadBriefingArgs),
}

/// Arguments for `hangar squad briefing`.
#[derive(Args, Debug)]
pub struct SquadBriefingArgs {
    /// Squad id whose leader briefing to render.
    pub id: String,
    /// Workspace slug the squad belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar squad list`.
#[derive(Args, Debug)]
pub struct SquadListArgs {
    /// Workspace slug to list. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Include ARCHIVED squads (migration 0052). The default list is active-only.
    #[arg(long)]
    pub all: bool,
}

/// Arguments for `hangar squad archive` / `hangar squad unarchive`.
#[derive(Args, Debug)]
pub struct SquadArchiveArgs {
    /// Squad id to (un)archive.
    pub id: String,
    /// Workspace slug the squad belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
    /// The `user.id` recorded as the archiving actor (migration 0052). Omitted
    /// defaults to the workspace owner.
    #[arg(long)]
    pub by: Option<String>,
}

/// Arguments for `hangar squad create`.
#[derive(Args, Debug)]
pub struct SquadCreateArgs {
    /// The squad name (unique within the workspace).
    pub name: String,
    /// The squad leader as an actor-ref (`agent:<id>` / `member:<id>`). An `agent`
    /// leader is the actor a squad-assigned task is routed to.
    #[arg(long)]
    pub leader: String,
    /// Initial routing guidance for the squad, rendered VERBATIM as the leader
    /// briefing's `## Squad Instructions` section. Omitted leaves it empty, and
    /// a blank field omits that section entirely.
    #[arg(long)]
    pub instructions: Option<String>,
    /// Workspace slug the squad belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar squad add-member` / `hangar squad remove-member`.
#[derive(Args, Debug)]
pub struct SquadMemberArgs {
    /// The squad id (`squad.id`) to mutate.
    pub squad_id: String,
    /// The member actor-ref (`agent:<id>` / `member:<id>`).
    #[arg(long)]
    pub member: String,
    /// Free-text role for the ADDED member ("owns the migrations"), which the
    /// squad leader reads in its briefing. Honoured by `add-member` and IGNORED
    /// by `remove-member`. Omitted leaves an existing member's role untouched.
    #[arg(long)]
    pub role: Option<String>,
    /// Workspace slug the squad belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar squad member-role`: set or clear an EXISTING
/// membership's free-text role (migration 0053).
#[derive(Args, Debug)]
pub struct SquadMemberRoleArgs {
    /// The squad id (`squad.id`) whose membership to edit.
    pub squad_id: String,
    /// The existing member actor-ref (`agent:<id>` / `member:<id>`).
    #[arg(long)]
    pub member: String,
    /// The free-text role label. Pass an empty string to clear it.
    #[arg(long, default_value = "")]
    pub role: String,
    /// Workspace slug the squad belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar squad instructions`: show, set, or clear a squad's
/// user-authored routing guidance (migration 0053).
#[derive(Args, Debug)]
pub struct SquadInstructionsArgs {
    /// The squad id (`squad.id`) to read or edit.
    pub squad_id: String,
    /// Replace the squad's instructions with this text (stored verbatim).
    #[arg(long, conflicts_with = "clear")]
    pub set: Option<String>,
    /// Clear the squad's instructions, so the leader briefing omits the section.
    #[arg(long)]
    pub clear: bool,
    /// Workspace slug the squad belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar squad assign`: route a task to the squad's leader.
#[derive(Args, Debug)]
pub struct SquadAssignArgs {
    /// The squad id (`squad.id`) whose leader the task routes to.
    pub squad_id: String,
    /// The issue the routed task carries (`issue.id`), or omit for an ad-hoc task.
    #[arg(long)]
    pub issue: Option<String>,
    /// The run's working directory, or omit.
    #[arg(long)]
    pub work_dir: Option<String>,
    /// Claim urgency (0..3, higher = more urgent). Defaults to `0` (routine).
    #[arg(long, default_value_t = 0)]
    pub priority: i64,
    /// Fan the work out across the WHOLE squad (leader brief + one task per
    /// distinct `agent` member) instead of briefing the leader alone.
    #[arg(long)]
    pub fanout: bool,
    /// The user the invocation-permission gate judges this assignment by (a user
    /// id or an email). Omitted defaults to the workspace owner — the ordinary
    /// single-operator assign, which the gate always admits.
    #[arg(long)]
    pub invoker: Option<String>,
    /// Workspace slug the squad belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Parse a `--env KEY=VALUE` argument into a `(key, value)` pair.
///
/// # Errors
///
/// Returns a CONTENT-FREE message if the input has no `=` or an empty key.
/// The message deliberately never echoes `raw` (parity #30, multica
/// `cmd_agent.go:757-759`): the whole point of `--env` is that the argument
/// carries a secret, and clap prints a `value_parser` error to stderr, which
/// lands in shell logs and CI transcripts.
fn parse_env_kv(raw: &str) -> Result<(String, String), String> {
    let (key, value) =
        raw.split_once('=').ok_or_else(|| "--env: expected KEY=VALUE".to_string())?;
    if key.is_empty() {
        return Err("--env: env var name must not be empty".to_string());
    }
    Ok((key.to_string(), value.to_string()))
}

/// Decode a JSON-object-of-strings env payload from `--env-file` / `--env-stdin`.
///
/// Ported shape-for-shape from multica's `resolveCustomEnv`
/// (`server/cmd/multica/cmd_agent.go:788-852`):
///
/// * empty / whitespace-only input is an **error**, never a clear — an empty
///   read almost always means a broken upstream pipe, and treating it as a clear
///   would silently wipe the agent's secrets;
/// * the explicit `{}` is the only clear;
/// * a parse failure is CONTENT-FREE — `serde_json`'s message quotes fragments
///   of the input, which here IS the secret.
///
/// # Errors
///
/// Returns a value-free message when the payload is blank or is not a JSON
/// object of string keys to string values.
fn parse_env_json(raw: &str, flag: &str) -> Result<Vec<(String, String)>, String> {
    if raw.trim().is_empty() {
        return Err(format!("{flag}: empty input; pass '{{}}' to clear the env"));
    }
    let shape = format!("{flag} must be a valid JSON object of string keys and string values");
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|_| shape.clone())?;
    let obj = value.as_object().ok_or_else(|| shape.clone())?;
    obj.iter()
        .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())).ok_or_else(|| shape.clone()))
        .collect()
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
/// `sync` imports a `ainb-toolkit/skills/`-shaped directory into the default
/// workspace (idempotent on `(workspace, name)`); `list` shows the imported
/// skills. Both are workspace-scoped via `--workspace`.
#[derive(Subcommand, Debug)]
pub enum SkillsCommand {
    /// Import skills from a toolkit directory into a workspace (idempotent).
    Sync(SkillsSyncArgs),
    /// List the skills imported into a workspace.
    List(SkillsListArgs),
    /// Attach a skill to an agent (idempotent; never re-enables a disabled link).
    Attach(SkillsLinkArgs),
    /// Detach a skill from an agent (idempotent).
    Detach(SkillsLinkArgs),
    /// Enable or disable an already-attached skill for one agent (parity #24).
    Toggle(SkillsToggleArgs),
}

/// Arguments for `hangar skills sync`.
#[derive(Args, Debug)]
pub struct SkillsSyncArgs {
    /// Workspace slug to import into. Defaults to the bootstrapped `default`
    /// workspace.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Source directory holding `<name>/SKILL.md` skill dirs. Defaults to
    /// `$AINB_TOOLKIT_SKILLS_DIR`, else a walk up to `ainb-toolkit/skills`.
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
    /// List one agent's ATTACHMENTS (with their enabled/disabled state) instead
    /// of the workspace's skills. Accepts an agent id or its name.
    #[arg(long)]
    pub agent: Option<String>,
}

/// Arguments for `hangar skills attach` / `hangar skills detach`.
///
/// Both the skill and the agent accept an id OR a name, because
/// `hangar agent create` prints only the name — requiring ids would force every
/// caller through an `agent list --format json | jq` scrape.
#[derive(Args, Debug)]
pub struct SkillsLinkArgs {
    /// Skill to link: its id, or its kebab-case name within the workspace.
    pub skill: String,
    /// Agent to link it to: its id, or its name within the workspace.
    #[arg(long)]
    pub agent: String,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar skills toggle` (parity #24).
///
/// `--enabled` takes an explicit `true`/`false` rather than being a flip, so the
/// command is idempotent and two operators converge on the same state.
#[derive(Args, Debug)]
pub struct SkillsToggleArgs {
    /// Skill to toggle: its id, or its kebab-case name within the workspace.
    pub skill: String,
    /// Agent whose link is toggled: its id, or its name within the workspace.
    #[arg(long)]
    pub agent: String,
    /// `true` = the link materialises; `false` = it stays attached but is
    /// suppressed at dispatch.
    #[arg(long, action = clap::ArgAction::Set)]
    pub enabled: bool,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
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
    /// Search issues by title, description, or comment body (ranked).
    Search(IssueSearchArgs),
    /// Show one issue by id.
    Show(IssueShowArgs),
    /// Edit an existing issue's state, assignee, priority, or due date.
    Update(IssueUpdateArgs),
    /// Apply ONE lifecycle state to several issues, cascading to parents ONCE.
    #[command(name = "batch-state")]
    BatchState(IssueBatchStateArgs),
    /// Delete an issue and all its history (dry-run without `--yes`).
    Delete(IssueDeleteArgs),
    /// Attach or detach a label on an issue.
    #[command(subcommand)]
    Label(IssueLabelCommand),
    /// Inspect or tick off an issue's acceptance criteria.
    #[command(subcommand)]
    Criteria(IssueCriteriaCommand),
    /// Add, remove, or list an issue's typed links to other issues.
    #[command(subcommand)]
    Link(IssueLinkCommand),
    /// Subscribe an actor to an issue's notifications (multica parity #22).
    Subscribe(IssueSubscribeArgs),
    /// Unsubscribe an actor from an issue's notifications. Idempotent.
    Unsubscribe(IssueSubscribeArgs),
    /// List who watches an issue, with the reason each one was subscribed.
    Subscribers(IssueSubscribersArgs),
    /// Add, remove, or list an issue's emoji reactions.
    #[command(subcommand)]
    React(IssueReactCommand),
    /// Explain why an issue did (or did not) dispatch — its admission history.
    #[command(alias = "dispatch-log")]
    Why(IssueWhyArgs),
    /// Show one issue's activity timeline: state changes, assignments, comments.
    #[command(alias = "activity")]
    Timeline(IssueTimelineArgs),
    /// Set or clear one of the workspace's custom properties on an issue.
    #[command(subcommand)]
    Property(IssuePropertyCommand),
    /// Read and write an issue's agent metadata scratch bag.
    #[command(subcommand, alias = "metadata")]
    Meta(IssueMetaCommand),
}

/// `hangar issue property <verb>` (multica parity #17).
#[derive(Subcommand, Debug)]
pub enum IssuePropertyCommand {
    /// Set ONE custom property's value on an issue.
    Set(IssuePropertySetArgs),
    /// Clear ONE custom property from an issue. Idempotent.
    Clear(IssuePropertyClearArgs),
}

/// Arguments for `hangar issue property set`.
#[derive(Args, Debug)]
pub struct IssuePropertySetArgs {
    /// Issue id (ULID) to write.
    pub id: String,
    /// The definition's stable slug.
    #[arg(long)]
    pub key: String,
    /// The value. Repeat for a multi_select.
    #[arg(long = "value")]
    pub values: Vec<String>,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue property clear`.
#[derive(Args, Debug)]
pub struct IssuePropertyClearArgs {
    /// Issue id (ULID) to write.
    pub id: String,
    /// The definition's stable slug.
    #[arg(long)]
    pub key: String,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar issue meta <verb>` — the AGENT scratch bag (multica parity #17).
///
/// Flat primitive KV for pipeline bookkeeping. Every mutation is single-key
/// atomic, so `hangar issue update` never disturbs it.
#[derive(Subcommand, Debug)]
pub enum IssueMetaCommand {
    /// List an issue's metadata entries, key-sorted.
    List(IssueMetaListArgs),
    /// Print ONE metadata value.
    Get(IssueMetaKeyArgs),
    /// Set ONE metadata key.
    Set(IssueMetaSetArgs),
    /// Delete ONE metadata key. Idempotent.
    Delete(IssueMetaKeyArgs),
}

/// Arguments for `hangar issue meta list`.
#[derive(Args, Debug)]
pub struct IssueMetaListArgs {
    /// Issue id (ULID) whose bag to read.
    pub id: String,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue meta get|delete`.
#[derive(Args, Debug)]
pub struct IssueMetaKeyArgs {
    /// Issue id (ULID) whose bag to address.
    pub id: String,
    /// The metadata key.
    #[arg(long)]
    pub key: String,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue meta set`.
#[derive(Args, Debug)]
pub struct IssueMetaSetArgs {
    /// Issue id (ULID) whose bag to write.
    pub id: String,
    /// The metadata key (letters, digits, `_`, `.`, `-`; max 64).
    #[arg(long)]
    pub key: String,
    /// The value.
    #[arg(long)]
    pub value: String,
    /// Force the value's type: string, number, or bool. Default sniffs.
    #[arg(long = "type")]
    pub value_type: Option<String>,
    /// Workspace slug. Defaults to the bootstrapped `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue timeline` (multica parity #13).
#[derive(Args, Debug)]
pub struct IssueTimelineArgs {
    /// Issue id (ULID) whose narrative to print.
    pub id: String,
    /// How many entries to show — the newest window, printed oldest-first.
    #[arg(long, default_value_t = 200)]
    pub limit: i64,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue why` (multica parity #12).
#[derive(Args, Debug)]
pub struct IssueWhyArgs {
    /// Issue id (ULID) whose dispatch history to explain.
    pub id: String,
    /// How many attempts to show, newest first.
    #[arg(long, default_value_t = 20)]
    pub limit: i64,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar issue link <verb>` (multica parity #20).
///
/// An issue relates to another with a KIND: `blocked-by` (the gating relation —
/// the issue refuses to run until the other finishes), `blocks` (the same
/// relation authored from the other end), or `related` (a plain association that
/// NEVER gates and never auto-runs). Talks to the store repo directly, exactly
/// like `issue criteria`, so it works with no daemon running.
#[derive(Subcommand, Debug)]
pub enum IssueLinkCommand {
    /// Link two issues. Re-adding a pair with a new kind replaces the kind.
    Add(IssueLinkArgs),
    /// Remove a link between two issues. Idempotent.
    Remove(IssueLinkArgs),
    /// List an issue's links (`🔒`/`✓` blocked-by, `→` blocks, `~` related).
    List(IssueLinkListArgs),
}

/// The `--kind` selector for `hangar issue link add|remove`.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkKindArg {
    /// This issue is blocked by the other — the gating default.
    #[default]
    BlockedBy,
    /// This issue blocks the other (stored as the reverse gating link).
    Blocks,
    /// The two issues are merely associated: never gating, never auto-running.
    Related,
}

impl LinkKindArg {
    /// The store-side kind this selector authors.
    fn to_kind(self) -> ainb_hangar_store::repo::card_dependency::LinkKind {
        use ainb_hangar_store::repo::card_dependency::LinkKind;
        match self {
            Self::BlockedBy => LinkKind::BlockedBy,
            Self::Blocks => LinkKind::Blocks,
            Self::Related => LinkKind::Related,
        }
    }
}

/// Arguments for `hangar issue link add|remove`.
#[derive(Args, Debug)]
pub struct IssueLinkArgs {
    /// Issue id (ULID) the link is authored ON.
    pub id: String,
    /// The OTHER issue id (ULID) at the far end of the link.
    pub other: String,
    /// The link kind. Defaults to `blocked-by`, the gating relation.
    #[arg(long, value_enum, default_value_t = LinkKindArg::BlockedBy)]
    pub kind: LinkKindArg,
    /// Workspace slug both issues belong to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue link list`.
#[derive(Args, Debug)]
pub struct IssueLinkListArgs {
    /// Issue id (ULID) whose links to list.
    pub id: String,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue subscribe|unsubscribe` (multica parity #22).
#[derive(Args, Debug)]
pub struct IssueSubscribeArgs {
    /// Issue id (ULID) to watch / stop watching.
    pub id: String,
    /// The actor, as `member:<id>` / `agent:<id>`. Defaults to the LOCAL HUMAN
    /// (`member:me`), mirroring the reference's "the target defaults to the
    /// caller".
    #[arg(long)]
    pub actor: Option<String>,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue subscribers` (multica parity #22).
#[derive(Args, Debug)]
pub struct IssueSubscribersArgs {
    /// Issue id (ULID) whose watchers to list.
    pub id: String,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar issue react <verb>` (multica parity #22).
///
/// An emoji reaction is unique per `(issue, actor, emoji)`, so `add` is
/// idempotent and `remove` never errors on an absent one. Talks to the store
/// repo directly, exactly like `issue link`, so it works with no daemon running.
#[derive(Subcommand, Debug)]
pub enum IssueReactCommand {
    /// React to an issue with an emoji. Idempotent.
    Add(IssueReactArgs),
    /// Remove your reaction. Idempotent.
    Remove(IssueReactArgs),
    /// List an issue's reactions as `<emoji> <count>` buckets.
    List(IssueSubscribersArgs),
}

/// Arguments for `hangar issue react add|remove`.
#[derive(Args, Debug)]
pub struct IssueReactArgs {
    /// Issue id (ULID) to react to.
    pub id: String,
    /// The emoji. Required and non-blank (the reference's "emoji is required").
    #[arg(long)]
    pub emoji: String,
    /// The reacting actor. Defaults to the LOCAL HUMAN (`member:me`).
    #[arg(long)]
    pub actor: Option<String>,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar issue criteria <verb>` (multica parity #11-rest).
///
/// An issue's definition-of-done is a list of individually addressable criteria.
/// `list` prints them with their ordinal, stable id, and `☑`/`☐` state; `check`
/// and `uncheck` set one criterion's state by id OR 1-based ordinal, through the
/// same store mutator the `hangar/issue_criterion_set` daemon RPC uses.
#[derive(Subcommand, Debug)]
pub enum IssueCriteriaCommand {
    /// List an issue's acceptance criteria with ordinal, id, and ☑/☐ state.
    List(IssueCriteriaListArgs),
    /// Tick a criterion off (by id or 1-based ordinal). Idempotent.
    Check(IssueCriteriaSetArgs),
    /// Un-tick a criterion (by id or 1-based ordinal). Idempotent.
    Uncheck(IssueCriteriaSetArgs),
}

/// Arguments for `hangar issue criteria list`.
#[derive(Args, Debug)]
pub struct IssueCriteriaListArgs {
    /// Issue id (ULID) whose criteria to list.
    pub id: String,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue criteria check|uncheck`.
///
/// `criterion` addresses ONE element either by its stable id (`ac-…`) or by its
/// 1-based ordinal as printed by `criteria list` — an agent reading the detail
/// card sees positions, not ids. The mutation is workspace-scoped: an issue id
/// outside the tenant touches no row.
#[derive(Args, Debug)]
pub struct IssueCriteriaSetArgs {
    /// Issue id (ULID) whose criterion to (un)tick.
    pub id: String,
    /// Criterion id (`ac-…`) or 1-based ordinal (`2`).
    pub criterion: String,
    /// Who ticked it (`agent:<id>` / `member:<id>`); recorded on a check and
    /// cleared on an uncheck.
    #[arg(long)]
    pub actor: Option<String>,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for `hangar issue delete`.
///
/// Without `--yes` this is a DRY RUN: it prints the issue title and the counts of
/// what a real delete would remove (comments / tasks / placements) and exits
/// WITHOUT deleting. Pass `--yes` to actually perform the cascade. The delete is
/// workspace-scoped and refuses while any task on the issue is ACTIVE (cancel the
/// run first), exactly like the `hangar/issue_delete` daemon RPC.
#[derive(Args, Debug)]
pub struct IssueDeleteArgs {
    /// Issue id (ULID) to delete.
    pub id: String,
    /// Actually perform the delete. Without this flag the command only PREVIEWS
    /// what would be removed and exits without touching the database.
    #[arg(long)]
    pub yes: bool,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
}

/// `hangar issue label <verb>`.
///
/// Attach or detach a label on one issue, workspace-scoped. Mirrors the
/// `hangar/issue_label_attach` / `hangar/issue_label_detach` daemon RPCs over the
/// CLI: attach resolve-or-creates a label by `(workspace, name)`; detach removes
/// the link (idempotent), leaving the label definition intact.
#[derive(Subcommand, Debug)]
pub enum IssueLabelCommand {
    /// Attach a label to an issue (resolve-or-creates the label; idempotent).
    Attach(IssueLabelArgs),
    /// Detach a label from an issue (idempotent; the definition is kept).
    Detach(IssueLabelArgs),
}

/// Arguments for `hangar issue label attach|detach`.
///
/// `id` is the issue; `name` is the label. `--color` is an optional presentation
/// hint applied only when an attach mints a fresh label (ignored on detach and
/// when an existing label is reused). The mutation is workspace-scoped: a
/// `--workspace` selects the tenant (default: the bootstrapped `default`), and an
/// issue id outside it touches no row.
#[derive(Args, Debug)]
pub struct IssueLabelArgs {
    /// Issue id (ULID) to (de)label.
    pub id: String,
    /// Label name to attach / detach.
    pub name: String,
    /// Optional presentation colour (hex, e.g. `#ff0000`) for a freshly-created
    /// label; ignored on detach / when an existing label is reused.
    #[arg(long)]
    pub color: Option<String>,
    /// Workspace slug the issue belongs to. Defaults to the bootstrapped
    /// `default` workspace.
    #[arg(long)]
    pub workspace: Option<String>,
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
    /// New lifecycle state — one of `backlog`, `todo`, `in_progress`,
    /// `in_review`, `done`, `blocked`, `cancelled`; omitted leaves it.
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

/// Arguments for `hangar issue batch-state` (multica parity #3-rest).
#[derive(Args, Debug)]
pub struct IssueBatchStateArgs {
    /// Issue ids (ULIDs) to transition. Duplicates collapse.
    #[arg(num_args = 1.., required = true)]
    pub ids: Vec<String>,
    /// The lifecycle state applied to EVERY id — one of `backlog`, `todo`,
    /// `in_progress`, `in_review`, `done`, `blocked`, `cancelled`.
    #[arg(long)]
    pub state: String,
    /// Workspace slug the issues belong to. Defaults to the bootstrapped
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
    /// (reference ordering parity), so a higher value jumps the queue while equal
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
    /// Each name is resolve-or-created in the workspace and joined to the issue
    /// through the `label` / `issue_label` tables (migration 0016), so a repeated
    /// name yields exactly one attachment.
    #[arg(long = "label", action = clap::ArgAction::Append)]
    pub labels: Vec<String>,
    /// An acceptance criterion (repeatable: `--acceptance "x" --acceptance "y"`).
    ///
    /// Persisted as the issue's ordered acceptance-criteria list (migration 0048,
    /// multica parity); rendered on the detail card's `Acceptance:` block.
    #[arg(long = "acceptance", action = clap::ArgAction::Append)]
    pub acceptance_criteria: Vec<String>,
    /// A context reference — URL / `owner/repo#123` / note (repeatable).
    ///
    /// Persisted as the issue's ordered context-reference list (migration 0048,
    /// multica parity); rendered on the detail card's `Context:` block.
    #[arg(long = "context-ref", action = clap::ArgAction::Append)]
    pub context_refs: Vec<String>,
    /// The repo the run executes in: an absolute checkout path, the literal
    /// `scratch`, or a REMOTE (`owner/repo`, a full URL, or `git@…`) — a remote
    /// is cloned once into the shared clone cache and the local path persisted,
    /// exactly like the board card-create path (migration 0032/0042).
    #[arg(long)]
    pub repo: Option<String>,
    /// The SOURCE branch the run branches FROM (migration 0042); omitted uses
    /// the repo's default branch. Persisted on the issue AND the enqueued task.
    #[arg(long = "source-branch")]
    pub source_branch: Option<String>,
    /// The TARGET branch a future PR lands INTO (migration 0042); stored on the
    /// issue for later PR automation.
    #[arg(long = "target-branch")]
    pub target_branch: Option<String>,
    /// Make this a SUB-ISSUE of an existing issue (`issue.id`, migration 0046).
    ///
    /// The parent must exist in the same workspace; completing the last child of
    /// the lowest unfinished stage cascades a roll-up comment onto the parent.
    #[arg(long)]
    pub parent: Option<String>,
    /// The 1-based STAGE BARRIER this sub-issue belongs to (migration 0046).
    ///
    /// Only meaningful with `--parent`. Siblings sharing a stage close their
    /// barrier together: when the LAST of them finishes, ONE aggregated roll-up
    /// comment is posted on the parent naming every child that closed it
    /// (multica parity #3-rest), not one comment per child.
    #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
    pub stage: Option<i64>,
    /// Provenance of this issue: `autopilot` | `comment_mention` | `manual`
    /// (migration 0056, multica parity #21).
    ///
    /// Defaults to `$HANGAR_ORIGIN_TYPE` — the daemon injects it into a
    /// dispatched agent's environment, so an issue an agent creates mid-run is
    /// attributable back to the comment / autopilot that asked for it. With
    /// neither flag nor env, a create is stamped `manual`.
    #[arg(long = "origin-type")]
    pub origin_type: Option<String>,
    /// The provenance id: the autopilot id for `autopilot`, the comment id for
    /// `comment_mention`. REQUIRED for every kind except `manual`.
    ///
    /// Defaults to `$HANGAR_ORIGIN_ID`. Supplying an id with no
    /// `--origin-type` is an error, never a silent drop.
    #[arg(long = "origin-id")]
    pub origin_id: Option<String>,
}

/// Resolve an issue create's ORIGIN PROVENANCE from flags, then env, then the
/// `manual` default (migration 0056, multica parity #21).
///
/// Precedence rule: an explicit `--origin-type` suppresses the env pair
/// ENTIRELY, so a flag kind is never mixed with an inherited env id. Only when
/// NEITHER flag is given does the daemon-injected `HANGAR_ORIGIN_TYPE` /
/// `HANGAR_ORIGIN_ID` pair apply.
///
/// # Errors
///
/// Returns an error for an id without a kind, a kind outside the allow-list, or
/// a kind that requires an id and got none — the same messages the RPC handler
/// produces, so both surfaces say the same thing.
fn resolve_cli_origin(
    flag_type: Option<&str>,
    flag_id: Option<&str>,
    env_type: Option<&str>,
    env_id: Option<&str>,
) -> Result<IssueOrigin> {
    let (kind, id) = if flag_type.is_some() || flag_id.is_some() {
        (flag_type, flag_id)
    } else {
        (env_type, env_id)
    };
    Ok(IssueOrigin::from_wire(kind, id)?.unwrap_or_else(IssueOrigin::manual))
}

/// Parse a `--due` value (`YYYY-MM-DD`) into an epoch-millisecond timestamp at
/// UTC midnight.
///
/// # Errors
///
/// Returns a human-readable message if the input is not a valid `YYYY-MM-DD`
/// date (surfaced by clap as the flag's value error).
fn parse_due_date(raw: &str) -> Result<i64, String> {
    // One parser, one error message, shared with the TUI create wizard so a
    // calendar day typed in either place resolves to the same UTC-midnight ms.
    ainb_hangar_proto::dates::parse_calendar_date_ms(raw)
}

/// Arguments for `hangar issue list`.
#[derive(Args, Debug)]
pub struct IssueListArgs {
    /// Restrict to issues in this lifecycle state.
    #[arg(long, default_value = DEFAULT_ISSUE_STATE)]
    pub state: String,
}

/// Arguments for `hangar issue search`.
///
/// `query` is the case-insensitive substring to match across the issue title,
/// description, and comment bodies; results are ranked title > description >
/// comment. Unlike the plugin's client-side `/` title-only filter, this reaches
/// into description + comment bodies and across the whole workspace (not just a
/// loaded page). A `--workspace` selects the tenant (default: the bootstrapped
/// `default` workspace); a blank query matches nothing.
#[derive(Args, Debug)]
pub struct IssueSearchArgs {
    /// The text to search for (matched across title / description / comments).
    pub query: String,
    /// Workspace slug to search within. Defaults to the bootstrapped `default`
    /// workspace.
    #[arg(long)]
    pub workspace: Option<String>,
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
    /// Report whether the daemon is running (PID file + socket) and the
    /// database is reachable.
    Status,
    /// Run the daemon in the FOREGROUND (boot + claim loop until interrupted).
    ///
    /// This blocks; `start` is the background variant. Equivalent to launching
    /// the `ainb-hangar-daemon` binary directly.
    Run,
    /// Start the daemon as a BACKGROUND child, recording its PID.
    ///
    /// Spawns the `ainb-hangar-daemon` binary detached and writes its exact pid
    /// to `<hangar_home>/hangar/daemon.pid`. A no-op (with a notice) if a live
    /// daemon is already recorded.
    Start,
    /// Stop the running daemon: signal the exact recorded PID, then remove the
    /// PID file.
    Stop,
    /// Restart the daemon: `stop` (if running) then `start`.
    Restart,
    /// One-command bring-up: ensure the store + socket-auth token, then `start`.
    Setup,
    /// View + edit the daemon's user-config knobs (`list`/`get`/`set`).
    #[command(subcommand)]
    Config(DaemonConfigCommand),
    /// Manage the one-time, host-wide `claude` credential the daemon injects
    /// into confined headless runs (`status`/`set`/`clear`).
    #[command(subcommand)]
    Cred(DaemonCredCommand),
}

/// `hangar daemon cred <verb>` — the daemon-level claude credential.
///
/// The credential is a SECRET (not a `daemon_config` knob), so it lives in the
/// platform secret store via `ainb_hangar_daemon::claude_cred`, never in the
/// plaintext `daemon_config` table. It is configured ONCE at daemon level; there
/// is no per-agent form.
#[derive(clap::Subcommand, Debug)]
pub enum DaemonCredCommand {
    /// Report whether a credential is configured and where it resolves from
    /// (env override / secret store / not set). Never prints the value.
    Status,
    /// Store a long-lived token. Reads the token from STDIN by default (so it
    /// never lands on argv or in shell history); `--setup-token` instead drives
    /// the interactive `claude setup-token` browser flow and captures the result.
    Set(DaemonCredSetArgs),
    /// Remove the stored credential. Idempotent.
    Clear,
}

/// Arguments for `hangar daemon cred set`.
#[derive(Args, Debug)]
pub struct DaemonCredSetArgs {
    /// Drive `claude setup-token` (browser OAuth) and capture the minted token,
    /// instead of reading a token from STDIN.
    #[arg(long)]
    pub setup_token: bool,
}

/// `hangar daemon config <verb>`.
///
/// The full user-configurable daemon knob surface — the CLI leg of the same
/// `daemon_config` registry the TUI Settings → Daemon section edits. Every verb
/// iterates [`ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY`], so a
/// knob added to the registry is listed/gettable/settable here with no new code.
/// Writes land straight in the `daemon_config` table; the watcher reloads its
/// config each scan tick, so an edit takes effect without a restart.
#[derive(Subcommand, Debug)]
pub enum DaemonConfigCommand {
    /// List every configurable: key, current value (or default), default, type.
    List,
    /// Print one knob's current value (or its default when unset).
    Get(DaemonConfigGetArgs),
    /// Validate + persist one knob's value (rejects unknown keys / bad values).
    Set(DaemonConfigSetArgs),
}

/// Arguments for `hangar daemon config get`.
#[derive(Args, Debug)]
pub struct DaemonConfigGetArgs {
    /// The config key (e.g. `autostandup.stagnant_min`). Must be a known knob.
    pub key: String,
}

/// Arguments for `hangar daemon config set`.
#[derive(Args, Debug)]
pub struct DaemonConfigSetArgs {
    /// The config key to write (must be a known knob).
    pub key: String,
    /// The new value; validated against the knob's type/range before persisting.
    pub value: String,
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
        HangarCommand::Daemon(c) => dispatch_daemon(c, format).await,
        HangarCommand::Auth(c) => dispatch_auth(c, format).await,
        HangarCommand::Config(c) => dispatch_config(c, format),
        HangarCommand::Skills(c) => dispatch_skills(c, format).await,
        HangarCommand::Templates(c) => dispatch_templates(c, format).await,
        HangarCommand::Agent(c) => dispatch_agent(c, format).await,
        HangarCommand::Member(c) => dispatch_member(c, format).await,
        HangarCommand::Squad(c) => dispatch_squad(c, format).await,
        HangarCommand::Autopilot(c) => dispatch_autopilot(c, format).await,
        HangarCommand::Workspace(c) => dispatch_workspace(c, format).await,
        HangarCommand::Logs(LogsCommand::Tail(args)) => run_logs_tail(args).await,
        HangarCommand::Property(c) => dispatch_property(c, format).await,
        HangarCommand::Comment(c) => dispatch_comment(c, format).await,
        HangarCommand::Inbox(InboxCommand::List(args)) => run_inbox_list(args, format).await,
    }
}

/// `hangar comment add|preview`: post a comment (or dry-run one) and report
/// where every `@`-mention went (multica parity #2-rest).
///
/// Both verbs drive the SAME `service::mention::route` the daemon drives, with
/// `dry_run` flipped — the shared path is the contract, so the preview applies
/// the identical invocation gate and can never disagree with the write.
async fn dispatch_comment(cmd: CommentCommand, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    use ainb_hangar_core::idgen::{IdGen as _, SystemIdGen};
    use ainb_hangar_store::repo::comment::{CommentRepo, NewComment};
    use ainb_hangar_store::service::mention::{MentionRouteRequest, route};
    use std::str::FromStr as _;

    let (args, dry_run) = match cmd {
        CommentCommand::Add(a) => (a, false),
        CommentCommand::Preview(a) => (a, true),
    };
    if args.body.trim().is_empty() {
        anyhow::bail!("comment body must not be empty");
    }
    let author = ActorRef::from_str(&args.author)
        .map_err(|e| anyhow::anyhow!("author must be `agent:<id>` or `member:<id>`: {e}"))?;

    let store = Store::open_default().await.context("open hangar database")?;
    // A typo'd workspace is an ERROR, never a silently empty routing report.
    let workspace_id = resolve_skills_workspace(&store, args.workspace.as_deref()).await?;

    // The write happens FIRST and separately, exactly as the daemon does it: a
    // routing fault can then never lose the comment.
    let comment_id = if dry_run {
        None
    } else {
        let id = SystemIdGen.new_ulid();
        let landed = CommentRepo::insert(
            store.pool(),
            &workspace_id,
            &NewComment {
                id: id.clone(),
                issue_id: args.issue.clone(),
                author: author.clone(),
                body: args.body.clone(),
                created_at: SystemClock.now_ms(),
                parent_id: args.parent.clone(),
            },
        )
        .await
        .context("write comment")?;
        anyhow::ensure!(
            landed,
            "no issue `{}` in this workspace (nothing was written)",
            args.issue
        );
        Some(id)
    };

    let rows = route(
        store.pool(),
        &SystemIdGen,
        &SystemClock,
        &MentionRouteRequest {
            workspace_id: &workspace_id,
            issue_id: &args.issue,
            comment_id: comment_id.as_deref(),
            parent_comment_id: args.parent.as_deref(),
            author: &author,
            body: &args.body,
            dry_run,
        },
    )
    .await
    .context("route comment mentions")?;

    print_mention_rows(&rows, format, dry_run);
    Ok(())
}

/// Render the routing rows in the requested output format.
fn print_mention_rows(
    rows: &[ainb_hangar_store::service::mention::MentionRouteRow],
    format: OutputFormat,
    dry_run: bool,
) {
    let reason = |r: &ainb_hangar_store::service::mention::MentionRouteRow| {
        r.reason.map(|d| d.as_db_str()).unwrap_or_default()
    };
    match format {
        OutputFormat::Json => {
            let wire: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "target_type": r.target_type,
                        "target_id": r.target_id,
                        "handle": r.handle,
                        "outcome": r.outcome.as_str(),
                        "reason": reason(r),
                        "task_id": r.task_id,
                        "source": r.source.as_str(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string(&wire).unwrap_or_else(|_| "[]".to_string())
            );
        }
        OutputFormat::Csv => {
            println!("target_type,target_id,handle,outcome,reason,source,task_id");
            for r in rows {
                println!(
                    "{},{},{},{},{},{},{}",
                    r.target_type,
                    csv_field(&r.target_id),
                    csv_field(&r.handle),
                    r.outcome.as_str(),
                    reason(r),
                    r.source.as_str(),
                    r.task_id.as_deref().unwrap_or_default()
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| target | handle | outcome | reason | source |");
            println!("|---|---|---|---|---|");
            for r in rows {
                println!(
                    "| {}:{} | @{} | {} | {} | {} |",
                    r.target_type,
                    r.target_id,
                    r.handle,
                    r.outcome.as_str(),
                    reason(r),
                    r.source.as_str()
                );
            }
        }
        OutputFormat::Text => {
            if rows.is_empty() {
                println!("no mentions routed");
                return;
            }
            for r in rows {
                let why = if reason(r).is_empty() {
                    String::new()
                } else {
                    format!(" ({})", reason(r))
                };
                println!(
                    "{:<8} @{:<20} {}{}  [{}]",
                    r.target_type,
                    r.handle,
                    r.outcome.as_str(),
                    why,
                    r.source.as_str()
                );
            }
            if dry_run {
                println!("(preview — nothing was written)");
            }
        }
    }
}

/// `hangar inbox list`: one actor's notification entries, newest first.
///
/// The read surface that proves an `@`-mention of a HUMAN actually landed on
/// that human (migration 0060 made `inbox_entry` actor-polymorphic).
async fn run_inbox_list(args: InboxListArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_store::repo::inbox::InboxRepo;
    use std::str::FromStr as _;

    let recipient = ActorRef::from_str(&args.recipient)
        .map_err(|e| anyhow::anyhow!("recipient must be `agent:<id>` or `member:<id>`: {e}"))?;
    let store = Store::open_default().await.context("open hangar database")?;
    let workspace_id = resolve_skills_workspace(&store, args.workspace.as_deref()).await?;
    let mut entries = InboxRepo::list(store.pool(), &workspace_id, &recipient, args.limit.max(1))
        .await
        .context("read inbox")?;
    // `--unread` filters HERE rather than in SQL: the repo's list is the shared
    // read the TUI also drives, and the unread model is a single nullable
    // column, so a post-filter cannot drift from `unread_count`'s definition.
    if args.unread {
        entries.retain(|e| e.read_at.is_none());
    }

    match format {
        OutputFormat::Json => {
            let wire: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "id": e.id,
                        "kind": e.kind.as_str(),
                        "event": e.event,
                        "subject_id": e.subject_id,
                        "summary": e.summary,
                        "created_at": e.created_at,
                        "read_at": e.read_at,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string(&wire).unwrap_or_else(|_| "[]".to_string())
            );
        }
        OutputFormat::Csv => {
            println!("kind,event,subject_id,summary,created_at,read");
            for e in &entries {
                println!(
                    "{},{},{},{},{},{}",
                    e.kind.as_str(),
                    csv_field(&e.event),
                    csv_field(&e.subject_id),
                    csv_field(&e.summary),
                    e.created_at,
                    e.read_at.is_some()
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| when | event | subject | summary |");
            println!("|---|---|---|---|");
            for e in &entries {
                println!(
                    "| {} | {} | {} | {} |",
                    fmt_epoch_ms_utc(e.created_at),
                    e.event,
                    e.subject_id,
                    md_cell(&e.summary)
                );
            }
        }
        OutputFormat::Text => {
            if entries.is_empty() {
                println!("inbox empty for {}", args.recipient);
                return Ok(());
            }
            for e in &entries {
                println!(
                    "{}  {}  {:<14} {:<28} {}",
                    fmt_epoch_ms_utc(e.created_at),
                    if e.read_at.is_some() { " " } else { "*" },
                    e.event,
                    e.subject_id,
                    e.summary
                );
            }
        }
    }
    Ok(())
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
        AutopilotCommand::ApiTrigger(args) => run_autopilot_api_trigger(&store, args).await,
        AutopilotCommand::Runs(args) => run_autopilot_runs(&store, args, format).await,
        AutopilotCommand::Webhook(args) => run_autopilot_webhook(&store, args).await,
        AutopilotCommand::Edit(args) => run_autopilot_edit(&store, args).await,
        AutopilotCommand::Versions(args) => run_autopilot_versions(&store, args, format).await,
        AutopilotCommand::Deliveries(args) => run_autopilot_deliveries(&store, args, format).await,
        AutopilotCommand::Collaborator(cmd) => {
            run_autopilot_collaborator(&store, cmd, format).await
        }
        AutopilotCommand::Subscriber(cmd) => run_autopilot_subscriber(&store, cmd, format).await,
        AutopilotCommand::Access(args) => run_autopilot_access(&store, args).await,
    }
}

/// Resolve `(workspace_id, autopilot_id)` and fail LOUDLY on a foreign /
/// unknown id — the repos' tenant join would otherwise turn a typo into a
/// silent no-op that looks like success.
async fn require_autopilot_in_workspace(
    store: &Store,
    workspace: Option<&str>,
    id: &str,
) -> Result<(String, ainb_hangar_core::ids::AutopilotId)> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::AutopilotRepo;

    let workspace_id = resolve_skills_workspace(store, workspace).await?.to_string();
    let ws = WorkspaceId::from_str(workspace_id.clone()).context("workspace id was empty")?;
    let autopilot_id = AutopilotId::from_str(id.to_string()).context("autopilot id was empty")?;
    if AutopilotRepo::get(store.pool(), &ws, &autopilot_id)
        .await
        .context("read autopilot")?
        .is_none()
    {
        anyhow::bail!("no autopilot `{id}` in this workspace");
    }
    Ok((workspace_id, autopilot_id))
}

/// The CLI writes the sqlite file DIRECTLY, so it must apply the same
/// restricted-mode write gate the daemon's request seam does — otherwise
/// `ainb hangar autopilot ...` would be a trivially open back door around it.
async fn require_autopilot_write(
    store: &Store,
    workspace_id: &str,
    id: &ainb_hangar_core::ids::AutopilotId,
    actor: &ainb_hangar_core::actor::ActorRef,
) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::autopilot_access::{WriteDecision, can_write};

    let ws = WorkspaceId::from_str(workspace_id.to_string()).context("workspace id was empty")?;
    match can_write(store.pool(), &ws, id, actor).await.context("write predicate")? {
        // No such rule here: fall through so the caller's own not-found path
        // reports it honestly rather than as a permission refusal.
        WriteDecision::Allowed(_) | WriteDecision::NotFound => Ok(()),
        WriteDecision::Denied => anyhow::bail!(
            "actor `{actor}` may not modify autopilot `{id}` (access_mode = restricted)"
        ),
    }
}

/// `hangar autopilot collaborator add|remove|list` (multica parity #27).
async fn run_autopilot_collaborator(
    store: &Store,
    cmd: AutopilotActorCommand,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_store::repo::autopilot_access::{AutopilotCollaboratorRepo, CollaboratorRole};

    let (args, add) = match cmd {
        AutopilotActorCommand::Add(a) => (a, Some(true)),
        AutopilotActorCommand::Remove(a) => (a, Some(false)),
        AutopilotActorCommand::List(a) => (a, None),
    };
    let (workspace_id, id) =
        require_autopilot_in_workspace(store, args.workspace.as_deref(), &args.id).await?;

    if let Some(add) = add {
        let target = parse_actor_arg(args.actor.as_deref())?;
        let acting = resolve_publisher(store, args.as_user.as_deref()).await?;
        require_autopilot_write(store, &workspace_id, &id, &acting).await?;
        if add {
            // An unknown role is a caller error, not a silent downgrade to
            // viewer — which would look exactly like the grant worked.
            let role = match args.role.as_deref() {
                None => CollaboratorRole::Editor,
                Some(raw) => CollaboratorRole::parse(raw)
                    .ok_or_else(|| anyhow::anyhow!("--role must be `editor` or `viewer`"))?,
            };
            let landed = AutopilotCollaboratorRepo::add(
                store.pool(),
                &workspace_id,
                id.as_str(),
                &target,
                role,
                Some(&acting),
                <ainb_hangar_core::clock::SystemClock as ainb_hangar_core::clock::HangarClock>::now_ms(
                    &ainb_hangar_core::clock::SystemClock,
                ),
            )
            .await
            .context("add collaborator")?;
            // Set membership: a re-add keeps the FIRST grant, so an explicit
            // role change stays explicit.
            if !landed {
                AutopilotCollaboratorRepo::set_role(
                    store.pool(),
                    &workspace_id,
                    id.as_str(),
                    &target,
                    role,
                )
                .await
                .context("update collaborator role")?;
            }
        } else {
            AutopilotCollaboratorRepo::remove(store.pool(), &workspace_id, id.as_str(), &target)
                .await
                .context("remove collaborator")?;
        }
    }

    let rows = AutopilotCollaboratorRepo::list(store.pool(), id.as_str())
        .await
        .context("read collaborators")?;
    if format == OutputFormat::Json {
        let json: Vec<_> = rows
            .iter()
            .map(|c| {
                serde_json::json!({
                    "actor": c.actor.to_string(),
                    "role": c.role_raw,
                    "created_by": c.created_by.as_ref().map(ToString::to_string),
                    "created_at": c.created_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no collaborators");
        return Ok(());
    }
    for c in rows {
        println!("{}  {}  {}", c.actor, c.role_raw, c.created_at);
    }
    Ok(())
}

/// `hangar autopilot subscriber add|remove|list` (multica parity #27).
async fn run_autopilot_subscriber(
    store: &Store,
    cmd: AutopilotActorCommand,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_store::repo::autopilot_access::AutopilotSubscriberRepo;

    let (args, add) = match cmd {
        AutopilotActorCommand::Add(a) => (a, Some(true)),
        AutopilotActorCommand::Remove(a) => (a, Some(false)),
        AutopilotActorCommand::List(a) => (a, None),
    };
    let (workspace_id, id) =
        require_autopilot_in_workspace(store, args.workspace.as_deref(), &args.id).await?;

    if let Some(add) = add {
        let target = parse_actor_arg(args.actor.as_deref())?;
        let acting = resolve_publisher(store, args.as_user.as_deref()).await?;
        require_autopilot_write(store, &workspace_id, &id, &acting).await?;
        if add {
            AutopilotSubscriberRepo::add(
                store.pool(),
                &workspace_id,
                id.as_str(),
                &target,
                Some(&acting),
                <ainb_hangar_core::clock::SystemClock as ainb_hangar_core::clock::HangarClock>::now_ms(
                    &ainb_hangar_core::clock::SystemClock,
                ),
            )
            .await
            .context("add subscriber")?;
        } else {
            AutopilotSubscriberRepo::remove(store.pool(), &workspace_id, id.as_str(), &target)
                .await
                .context("remove subscriber")?;
        }
    }

    let rows = AutopilotSubscriberRepo::list(store.pool(), id.as_str())
        .await
        .context("read subscribers")?;
    if format == OutputFormat::Json {
        let json: Vec<_> = rows
            .iter()
            .map(|sub| {
                serde_json::json!({
                    "actor": sub.actor.to_string(),
                    "created_by": sub.created_by.as_ref().map(ToString::to_string),
                    "created_at": sub.created_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no subscribers");
        return Ok(());
    }
    for sub in rows {
        println!("{}  {}", sub.actor, sub.created_at);
    }
    Ok(())
}

/// `hangar autopilot access --mode open|restricted` (multica parity #27).
///
/// Goes through `update_as` with the acting human so the flip mints a rule
/// version — widening who may write a rule is exactly the kind of publish the
/// accountability ledger exists to record.
async fn run_autopilot_access(store: &Store, args: AutopilotAccessArgs) -> Result<()> {
    use ainb_hangar_store::repo::autopilot::{
        AccessMode, AutopilotEdit, AutopilotRepo, UpdateOutcome,
    };

    let mode = match args.mode.as_str() {
        "open" => AccessMode::Open,
        "restricted" => AccessMode::Restricted,
        // Validated, never tolerantly coerced: a typo must not quietly leave a
        // rule world-writable.
        other => anyhow::bail!("--mode must be `open` or `restricted`, got `{other}`"),
    };
    let (workspace_id, id) =
        require_autopilot_in_workspace(store, args.workspace.as_deref(), &args.id).await?;
    let acting = resolve_publisher(store, args.as_user.as_deref()).await?;
    require_autopilot_write(store, &workspace_id, &id, &acting).await?;

    let ws = ainb_hangar_core::ids::WorkspaceId::from_str(workspace_id)
        .context("workspace id was empty")?;
    let outcome = AutopilotRepo::update_as(
        store.pool(),
        &ainb_hangar_core::clock::SystemClock,
        &ws,
        &id,
        &AutopilotEdit {
            access_mode: Some(mode),
            ..AutopilotEdit::default()
        },
        Some(&acting),
    )
    .await
    .context("set access mode")?;
    match outcome {
        UpdateOutcome::NotFound => anyhow::bail!("no autopilot `{}` in this workspace", id),
        UpdateOutcome::Updated { version } => {
            match version {
                Some(v) => println!("access_mode = {}  (rule version v{v})", mode.as_str()),
                // Already in that mode: the write landed, it just was not a
                // change worth an accountability row.
                None => println!("access_mode = {}  (unchanged)", mode.as_str()),
            }
            Ok(())
        }
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
        execution_mode: args.execution_mode.into(),
        concurrency_policy: args.concurrency_policy.into(),
        api_trigger_enabled: false,
    };

    // The accountable human for rule-version v1, written in the SAME
    // transaction as the insert (multica parity #14).
    let actor = resolve_publisher(store, args.as_user.as_deref()).await?;
    let id = AutopilotRepo::create_as(store.pool(), &SystemClock, &req, Some(&actor))
        .await
        .with_context(|| format!("create autopilot `{}` (cron `{}`)", args.name, args.cron))?;
    println!(
        "created autopilot {id} `{}` (cron `{}`) (v1 created by {actor})",
        args.name, args.cron
    );
    Ok(())
}

/// Resolve `--as-user` to the canonical `member:<user.id>` actor ref.
///
/// An omitted flag defaults to [`local_member`](ainb_hangar_core::actor::local_member)
/// (`member:me`), the same honest local-human default migration 0060 established
/// for inbox recipients — deliberately NOT `None`: a CLI mutation always has a
/// human at the keyboard, and recording "unattributed" would be less true than
/// recording "the local human".
async fn resolve_publisher(
    store: &Store,
    as_user: Option<&str>,
) -> Result<ainb_hangar_core::actor::ActorRef> {
    use ainb_hangar_core::actor::{ActorKind, ActorRef, local_member};
    match as_user {
        Some(token) => {
            let user_id = resolve_user_id(store, token).await?;
            ActorRef::new(ActorKind::Member, user_id).context("--as-user resolved to an empty id")
        }
        None => Ok(local_member()),
    }
}

/// `hangar autopilot edit <id>`: patch an autopilot's config (multica parity
/// #14).
///
/// Prints the minted rule version, or announces the COSMETIC case explicitly —
/// a rename lands but mints no version, and silently printing "updated" would
/// hide that distinction from the operator.
async fn run_autopilot_edit(store: &Store, args: AutopilotEditArgs) -> Result<()> {
    use ainb_hangar_core::ids::{AgentId, AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::{AutopilotEdit, AutopilotRepo, UpdateOutcome};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;
    let actor = resolve_publisher(store, args.as_user.as_deref()).await?;
    // The CLI writes sqlite DIRECTLY, so it applies the same restricted-mode
    // write gate the daemon's request seam does (multica parity #27).
    require_autopilot_write(store, ws.as_str(), &id, &actor).await?;

    let edit = AutopilotEdit {
        name: args.name.clone(),
        agent_id: match args.agent.clone() {
            Some(a) => Some(AgentId::from_str(a).context("agent id was empty")?),
            None => None,
        },
        instructions: if args.clear_instructions {
            Some(None)
        } else {
            args.instructions.clone().map(Some)
        },
        cron_expr: args.cron.clone(),
        max_concurrent_runs: args.max_concurrent_runs,
        execution_mode: args.execution_mode.map(Into::into),
        concurrency_policy: args.concurrency_policy.map(Into::into),
        // `ainb hangar autopilot access` is the one door onto the write-access
        // mode, so the generic edit patch deliberately leaves it alone.
        access_mode: None,
    };
    anyhow::ensure!(
        !edit.is_empty(),
        "nothing to edit — pass at least one of --name/--cron/--agent/--instructions/\
         --clear-instructions/--max-concurrent-runs/--execution-mode/--concurrency-policy"
    );

    match AutopilotRepo::update_as(store.pool(), &SystemClock, &ws, &id, &edit, Some(&actor))
        .await
        .with_context(|| format!("edit autopilot `{}`", args.id))?
    {
        UpdateOutcome::NotFound => {
            anyhow::bail!("no autopilot `{}` in this workspace", args.id)
        }
        UpdateOutcome::Updated {
            version: Some(version),
        } => {
            let kind = AutopilotRuleVersionRepo::latest(store.pool(), &ws, &id)
                .await
                .ok()
                .flatten()
                .map_or_else(|| "updated".to_string(), |v| v.change_kind);
            println!(
                "autopilot {} updated (v{version} {kind} by {actor})",
                args.id
            );
        }
        UpdateOutcome::Updated { version: None } => {
            println!("autopilot {} updated (no new version — cosmetic)", args.id);
        }
    }
    Ok(())
}

/// `hangar autopilot versions <id>`: the append-only rule-version ledger —
/// who published this rule, when, and why. Workspace-scoped: a foreign id
/// yields an empty set. An UNVERSIONED (pre-0061, never-edited) rule also yields
/// an empty set: the ledger was deliberately not backfilled.
async fn run_autopilot_versions(
    store: &Store,
    args: AutopilotVersionsArgs,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;

    let versions = AutopilotRuleVersionRepo::list(store.pool(), &ws, &id, args.limit)
        .await
        .context("list autopilot rule versions")?;
    render_autopilot_versions(&versions, format);
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

    // Pausing / resuming is a SUBSTANTIVE publish: it changes whether the rule
    // fires unattended, so it re-stamps who is accountable (multica parity #14).
    let actor = resolve_publisher(store, args.as_user.as_deref()).await?;
    // The CLI writes sqlite DIRECTLY, so it applies the same restricted-mode
    // write gate the daemon's request seam does (multica parity #27).
    require_autopilot_write(store, ws.as_str(), &id, &actor).await?;
    if enabled {
        AutopilotRepo::enable_as(store.pool(), &SystemClock, &ws, &id, Some(&actor))
            .await
            .with_context(|| format!("enable autopilot `{}`", args.id))?;
        println!("enabled autopilot {} (resumed by {actor})", args.id);
    } else {
        AutopilotRepo::disable_as(store.pool(), &SystemClock, &ws, &id, Some(&actor))
            .await
            .with_context(|| format!("disable autopilot `{}`", args.id))?;
        println!("disabled autopilot {} (paused by {actor})", args.id);
    }
    Ok(())
}

/// `hangar autopilot run <id> [--source manual|api]`: dispatch one tick
/// immediately, bypassing the schedule. Workspace-scoped: a foreign id is
/// rejected.
///
/// `--source api` is REFUSED unless the autopilot has armed its api trigger —
/// a half-configured trigger is never firable (the same discipline as the
/// webhook one).
///
/// The dispatch goes through the SAME admission gate the scheduler uses, so at
/// the concurrency limit under the `skip` policy it is DECLINED and recorded as
/// a terminal `skipped` run. That is a successful, no-op dispatch (exit 0), not
/// an error — matching multica's dispatch contract.
async fn run_autopilot_run_now(store: &Store, args: AutopilotRunNowArgs) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::AutopilotRepo;
    use ainb_hangar_store::repo::autopilot_run::{
        DispatchOutcome, RunAttribution, RunSource, dispatch_with_admission_as,
    };

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;

    let autopilot = AutopilotRepo::get(store.pool(), &ws, &id)
        .await
        .context("look up autopilot")?
        .with_context(|| format!("no autopilot `{}` in this workspace", args.id))?;

    let source = match args.source {
        RunSourceArg::Manual => RunSource::Manual,
        RunSourceArg::Api => {
            anyhow::ensure!(
                autopilot.api_trigger_enabled,
                "api trigger not enabled for autopilot {} — run `ainb hangar autopilot api-trigger {}`",
                args.id,
                args.id
            );
            RunSource::Api
        }
    };

    // THE ATTRIBUTION FORK (multica parity #14): a MANUAL fire is attributed to
    // the human at the keyboard (`direct_human`); an `api` fire is UNATTENDED,
    // so it resolves the rule owner from the version chain instead.
    let attribution = match source {
        RunSource::Manual => {
            RunAttribution::DirectHuman(resolve_publisher(store, args.as_user.as_deref()).await?)
        }
        _ => RunAttribution::RuleOwner,
    };

    match dispatch_with_admission_as(store.pool(), &SystemClock, &autopilot, source, &attribution)
        .await
        .with_context(|| format!("fire autopilot `{}`", args.id))?
    {
        DispatchOutcome::Fired {
            run_id, task_id, ..
        } => println!("fired autopilot {} → run {run_id} task {task_id}", args.id),
        DispatchOutcome::Skipped { run_id, reason, .. } => {
            println!("skipped autopilot {} → run {run_id} ({reason})", args.id);
        }
    }
    Ok(())
}

/// `hangar autopilot api-trigger <id> [--disable]`: arm (or disarm) the bare
/// programmatic `api` trigger (migration 0057). Workspace-scoped.
async fn run_autopilot_api_trigger(store: &Store, args: AutopilotIdArgs) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::AutopilotRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;
    let enabled = !args.disable;

    let actor = resolve_publisher(store, args.as_user.as_deref()).await?;
    // The CLI writes sqlite DIRECTLY, so it applies the same restricted-mode
    // write gate the daemon's request seam does (multica parity #27).
    require_autopilot_write(store, ws.as_str(), &id, &actor).await?;
    let updated = AutopilotRepo::set_api_trigger_enabled_as(
        store.pool(),
        &SystemClock,
        &ws,
        &id,
        enabled,
        Some(&actor),
    )
    .await
    .with_context(|| format!("set api trigger for autopilot `{}`", args.id))?;
    anyhow::ensure!(updated, "no autopilot `{}` in this workspace", args.id);

    if enabled {
        println!("enabled api trigger for autopilot {}", args.id);
        println!(
            "  fire with: ainb hangar autopilot run {} --source api",
            args.id
        );
    } else {
        println!("disabled api trigger for autopilot {}", args.id);
    }
    Ok(())
}

/// `hangar autopilot runs <id>`: the run-history read surface — every run with
/// its status, the trigger that fired it, and (for a declined dispatch) the
/// admission reason. Workspace-scoped: a foreign id yields an empty set.
async fn run_autopilot_runs(
    store: &Store,
    args: AutopilotRunsArgs,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot::AutopilotRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;

    let runs = AutopilotRepo::list_runs(store.pool(), &ws, &id, args.limit)
        .await
        .context("list autopilot runs")?;
    render_autopilot_runs(&runs, format);
    Ok(())
}

/// `hangar autopilot webhook <id>`: configure the HTTP webhook trigger.
///
/// Default (no `--disable`/`--rotate`) ENABLES the webhook and mints a fresh HMAC
/// secret; `--rotate` mints a new secret for an already-enabled webhook;
/// `--disable` turns it off and clears the secret. `--event`/`--clear-event` set
/// the optional exact-match event filter. The secret plaintext is printed ONCE
/// (it is never recoverable from the stored digest) and written to the daemon's
/// 0600 secret file so the ingress can recompute the body HMAC.
async fn run_autopilot_webhook(store: &Store, args: AutopilotWebhookArgs) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot_webhook::{AutopilotWebhookRepo, WebhookSecretStore};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;

    // The autopilot must exist in this workspace.
    let mut config = AutopilotWebhookRepo::get_config(store.pool(), &ws, &id)
        .await
        .context("read webhook config")?
        .with_context(|| format!("no autopilot `{}` in this workspace", args.id))?;

    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;
    let secrets = WebhookSecretStore::in_home(&home);

    // Apply the event-filter edit (independent of enable/disable/rotate).
    if args.clear_event {
        config.event_filter = None;
    } else if let Some(ev) = args.event.clone() {
        config.event_filter = Some(ev);
    }

    if args.disable {
        config.enabled = false;
        config.secret_sha256 = None;
        AutopilotWebhookRepo::set_config(store.pool(), &ws, &id, &config)
            .await
            .context("disable webhook")?;
        secrets.remove(&args.id).context("clear webhook secret")?;
        println!("disabled webhook for autopilot {}", args.id);
        return Ok(());
    }

    // Enable (default) or rotate: mint a fresh secret unless we are only editing
    // the filter on an already-secret-bearing webhook (no `--rotate` and a secret
    // already exists keeps the current secret).
    let needs_new_secret = args.rotate || config.secret_sha256.is_none();
    if needs_new_secret {
        let minted = ainb_hangar_core::webhook::mint_secret(&mut rand::rngs::OsRng);
        config.secret_sha256 = Some(minted.sha256_hex);
        config.enabled = true;
        AutopilotWebhookRepo::set_config(store.pool(), &ws, &id, &config)
            .await
            .context("set webhook config")?;
        // Persist the recoverable plaintext to the 0600 file the ingress reads.
        secrets.set(&args.id, &minted.plaintext).context("write webhook secret")?;
        println!("enabled webhook for autopilot {}", args.id);
        println!(
            "  url:    http://{}/hangar/webhook/{}",
            args.url_host, args.id
        );
        // The secret is shown ONCE — it is unrecoverable from the stored digest.
        println!("  secret: {} (shown once — store it now)", minted.plaintext);
        println!("  sign requests: X-Hangar-Signature: <hex HMAC-SHA256(secret, body)>");
    } else {
        config.enabled = true;
        AutopilotWebhookRepo::set_config(store.pool(), &ws, &id, &config)
            .await
            .context("set webhook config")?;
        println!(
            "updated webhook for autopilot {} (secret unchanged)",
            args.id
        );
        println!("  url: http://{}/hangar/webhook/{}", args.url_host, args.id);
    }
    if let Some(ev) = &config.event_filter {
        println!("  event filter: {ev}");
    }
    Ok(())
}

/// `hangar autopilot deliveries <id>`: list the recent webhook deliveries (the
/// audit log), latest-first, workspace-scoped.
async fn run_autopilot_deliveries(
    store: &Store,
    args: AutopilotDeliveriesArgs,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
    use ainb_hangar_store::repo::autopilot_webhook::AutopilotWebhookRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = AutopilotId::from_str(args.id.clone()).context("autopilot id was empty")?;

    let deliveries = AutopilotWebhookRepo::list_deliveries(store.pool(), &ws, &id, args.limit)
        .await
        .context("list webhook deliveries")?;
    render_webhook_deliveries(&deliveries, format);
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
        AgentCommand::Create(args) => run_agent_create(&store, args).await,
        AgentCommand::List(args) => run_agent_list(&store, args, format).await,
        AgentCommand::Edit(args) => run_agent_edit(&store, args).await,
        AgentCommand::Archive(args) => run_agent_set_archived(&store, args, true).await,
        AgentCommand::Unarchive(args) => run_agent_set_archived(&store, args, false).await,
        AgentCommand::Permission(args) => run_agent_permission(&store, args).await,
        AgentCommand::Allow(args) => run_agent_allow(&store, args).await,
        AgentCommand::CanInvoke(args) => run_agent_can_invoke(&store, args).await,
        AgentCommand::Env(args) => run_agent_env(&store, args, format).await,
    }
}

/// Resolve a `user_id_or_email` to a `user.id`: an `@`-bearing token is looked up
/// in the `user` table (email is UNIQUE); anything else is treated as a ULID id
/// verbatim. Errors when an email names no user.
async fn resolve_user_id(store: &Store, token: &str) -> Result<String> {
    let token = token.trim();
    if token.contains('@') {
        let id: Option<String> = sqlx::query_scalar("SELECT id FROM user WHERE email = ?")
            .bind(token)
            .fetch_optional(store.pool())
            .await
            .context("look up user by email")?;
        id.with_context(|| format!("no user with email {token}"))
    } else {
        Ok(token.to_string())
    }
}

/// Fetch an agent by id, erroring when it does not exist.
async fn require_agent(store: &Store, id: &str) -> Result<ainb_hangar_store::repo::agent::Agent> {
    use ainb_hangar_store::repo::agent::AgentRepo;
    AgentRepo::get(store.pool(), id)
        .await
        .context("look up agent")?
        .with_context(|| format!("no agent with id {id}"))
}

/// `hangar agent permission`: set the invocation permission mode + re-derive the
/// legacy visibility label. Prints the new mode and derived visibility.
async fn run_agent_permission(store: &Store, args: AgentPermissionArgs) -> Result<()> {
    use ainb_hangar_store::repo::agent::AgentRepo;

    let mode = args.mode.trim();
    if mode != "private" && mode != "public_to" {
        anyhow::bail!("mode must be `private` or `public_to`, got `{mode}`");
    }
    let touched = AgentRepo::set_permission_mode(store.pool(), &args.id, mode)
        .await
        .with_context(|| format!("set permission mode for agent {}", args.id))?;
    if !touched {
        anyhow::bail!("no agent with id {}", args.id);
    }
    let agent = require_agent(store, &args.id).await?;
    println!(
        "agent {} permission_mode={} visibility={}",
        args.id, agent.permission_mode, agent.visibility
    );
    Ok(())
}

/// `hangar agent allow`: add / revoke / list one invocation target. Adding a
/// target flips the agent to `public_to` (so the grant actually takes effect).
async fn run_agent_allow(store: &Store, args: AgentAllowArgs) -> Result<()> {
    use ainb_hangar_store::repo::agent::AgentRepo;
    use ainb_hangar_store::repo::agent_invocation_target::AgentInvocationTargetRepo;

    let agent = require_agent(store, &args.id).await?;

    if args.list {
        let targets = AgentInvocationTargetRepo::list(store.pool(), &args.id)
            .await
            .context("list invocation targets")?;
        if targets.is_empty() {
            println!("agent {} has no invocation targets", args.id);
        } else {
            for t in targets {
                println!("{}|{}", t.target_type, t.target_id);
            }
        }
        return Ok(());
    }

    // Resolve the (target_type, target_id) from the exactly-one target flag.
    let (target_type, target_id): (&str, String) = if args.workspace {
        ("workspace", agent.workspace_id.clone())
    } else if let Some(member) = args.member.as_deref() {
        ("member", resolve_user_id(store, member).await?)
    } else if let Some(team) = args.team.as_deref() {
        ("team", team.to_string())
    } else {
        anyhow::bail!(
            "pass exactly one of --workspace / --member <id|email> / --team <id> (or --list)"
        );
    };

    if args.revoke {
        let removed =
            AgentInvocationTargetRepo::remove(store.pool(), &args.id, target_type, &target_id)
                .await
                .context("remove invocation target")?;
        if removed {
            println!("revoked {target_type}|{target_id} from agent {}", args.id);
        } else {
            println!("agent {} had no {target_type}|{target_id} target", args.id);
        }
        return Ok(());
    }

    // Adding a target only matters when the agent is public_to — flip it if needed
    // (mirrors multica's "share ⇒ public_to").
    if agent.permission_mode != "public_to" {
        AgentRepo::set_permission_mode(store.pool(), &args.id, "public_to")
            .await
            .context("flip agent to public_to")?;
    }
    AgentInvocationTargetRepo::add(
        store.pool(),
        &ainb_hangar_core::idgen::SystemIdGen,
        &ainb_hangar_core::clock::SystemClock,
        &args.id,
        target_type,
        &target_id,
        None,
    )
    .await
    .context("add invocation target")?;
    println!("allowed {target_type}|{target_id} on agent {}", args.id);
    Ok(())
}

/// `hangar agent can-invoke`: print exactly `ALLOW` or `DENY` (exit 0 either way).
/// The deterministic readout the acceptance greps.
async fn run_agent_can_invoke(store: &Store, args: AgentCanInvokeArgs) -> Result<()> {
    use ainb_hangar_core::actor::ActorKind;
    use ainb_hangar_store::repo::agent::AgentRepo;

    let agent = require_agent(store, &args.id).await?;
    // An `--actor agent` invoke carries NO resolved originator (hangar has no
    // originator column yet), so the user id is dropped for the agent-actor case —
    // exactly the unattributed A2A path the gate fails closed against for
    // member/team targets. A `member` actor resolves the id.
    let is_agent_actor = args.actor.as_deref().map(str::trim) == Some("agent");
    let (kind, user_id) = if is_agent_actor {
        (ActorKind::Agent, None)
    } else {
        (
            ActorKind::Member,
            Some(resolve_user_id(store, &args.as_user).await?),
        )
    };
    let allowed = AgentRepo::can_invoke(store.pool(), &agent, kind, user_id.as_deref())
        .await
        .context("evaluate can_invoke")?;
    println!("{}", if allowed { "ALLOW" } else { "DENY" });
    Ok(())
}

/// Validate an optional `--description` against the 255-CODE-POINT cap
/// (migration 0050 / multica 060), returning the trimmed value.
///
/// Counted in `chars()`, not bytes, so an emoji-heavy blurb measures the way the
/// schema's `length()` CHECK measures it. Rejecting here gives the operator the
/// actionable message instead of an opaque store fault.
fn validated_description(desc: Option<&str>) -> Result<Option<String>> {
    let Some(desc) = desc else { return Ok(None) };
    let trimmed = desc.trim();
    if trimmed.chars().count() > ainb_hangar_store::repo::agent::MAX_DESCRIPTION_CHARS {
        anyhow::bail!("description must be 255 characters or fewer");
    }
    Ok(Some(trimmed.to_string()))
}

/// `hangar agent create`: create one agent from scratch, filling the workspace /
/// runtime / owner FKs behind the scenes. Prints the created agent's name (never
/// the id). An unsupported provider or empty name is a CLI error.
async fn run_agent_create(store: &Store, args: AgentCreateArgs) -> Result<()> {
    let name = args.name.trim();
    if name.is_empty() {
        anyhow::bail!("agent name must not be empty");
    }
    let provider = ainb_hangar_store::bootstrap::normalize_provider(args.provider.as_deref())
        .map_err(|e| anyhow::anyhow!(e))?;
    let description = validated_description(args.description.as_deref())?.unwrap_or_default();
    // An explicit --workspace must exist; the default is ensured (created if absent).
    let workspace_id = match args.workspace.as_deref() {
        Some(slug) => {
            let id: Option<String> = sqlx::query_scalar("SELECT id FROM workspace WHERE slug = ?")
                .bind(slug)
                .fetch_optional(store.pool())
                .await
                .context("look up workspace by slug")?;
            id.ok_or_else(|| anyhow::anyhow!("no workspace with slug {slug}"))?
        }
        None => ensure_default_workspace(store).await?,
    };
    let agent = ainb_hangar_store::bootstrap::create_agent_from(
        store.pool(),
        &workspace_id,
        ainb_hangar_store::bootstrap::AgentDraft {
            name: name.to_string(),
            provider,
            instructions: args.instructions,
            description,
            avatar_url: args.avatar,
            service_tier: args.service_tier,
            // `--model` rides the follow-up write below (unchanged); `kind` /
            // `system_key` are internal-only (a system agent is never CLI-minted).
            ..ainb_hangar_store::bootstrap::AgentDraft::default()
        },
    )
    .await
    .map_err(|e| {
        if ainb_hangar_store::repo::agent::is_duplicate_name(&e) {
            // Multica 046's 409 equivalent: a clear refusal + non-zero exit, never
            // a silent second identically-named actor the picker cannot tell apart.
            anyhow::anyhow!("an agent named `{name}` already exists in this workspace")
        } else {
            anyhow::Error::new(e).context("create agent")
        }
    })?;
    // Optional model override: mirror the daemon's create-time follow-up so the
    // CLI create path persists the model too. A blank value is treated as absent
    // (leaves `model` NULL rather than writing an empty string).
    if let Some(model) = args.model.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let update = ainb_hangar_store::repo::agent::AgentConfigUpdate {
            model: Some(Some(model.to_string())),
            ..Default::default()
        };
        ainb_hangar_store::repo::agent::AgentRepo::update_config(
            store.pool(),
            &workspace_id,
            &agent.id,
            &update,
        )
        .await
        .context("apply agent model override")?;
    }
    println!("created agent {}", agent.name);
    Ok(())
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

/// Resolve which of the three mutually-exclusive env write channels was used,
/// returning `None` when none was (leave the map unchanged).
///
/// `--env` is the argv channel (convenient, but the value lands in `ps` and
/// shell history); `--env-stdin` / `--env-file` are the SECRET channels multica
/// added for exactly that reason (`cmd_agent.go:788-852`). Clap already bars
/// combining them, so at most one arm can fire.
///
/// # Errors
///
/// Returns a value-free error when the JSON payload is blank or malformed, or
/// when the file cannot be read.
fn resolve_agent_env_write(args: &AgentEditArgs) -> Result<Option<Vec<(String, String)>>> {
    if !args.env.is_empty() {
        return Ok(Some(args.env.clone()));
    }
    if args.env_stdin {
        use std::io::Read as _;
        let mut raw = String::new();
        std::io::stdin().read_to_string(&mut raw).context("read --env-stdin")?;
        return parse_env_json(&raw, "--env-stdin").map(Some).map_err(|e| anyhow::anyhow!(e));
    }
    if let Some(path) = args.env_file.as_deref() {
        // The read error names the PATH, never the contents.
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read --env-file {}", path.display()))?;
        return parse_env_json(&raw, "--env-file").map(Some).map_err(|e| anyhow::anyhow!(e));
    }
    Ok(None)
}

/// `hangar agent env <id>`: print one agent's env with every VALUE masked.
///
/// The redacted-GET parity (multica `ListAgents`/`GetAgent` + `redactEnv`).
/// There is no plaintext mode — see [`AgentEnvArgs`].
///
/// # Errors
///
/// Returns an error when the workspace cannot be resolved, the store read
/// fails, or no agent with that id exists in the workspace.
async fn run_agent_env(store: &Store, args: AgentEnvArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_store::repo::agent::AgentRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let agent = AgentRepo::get(store.pool(), &args.id)
        .await
        .context("read agent")?
        .filter(|a| a.workspace_id == workspace_id)
        .ok_or_else(|| anyhow::anyhow!("no agent {} in this workspace", args.id))?;

    let redacted = agent.agent_env.redacted_pairs();
    match format {
        OutputFormat::Json => {
            let body = redacted
                .iter()
                .map(|(k, mask)| format!("{}:{}", json_string(k), json_string(mask)))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"env\":{{{body}}},\"env_key_count\":{},\"env_redacted\":{}}}",
                redacted.len(),
                !redacted.is_empty()
            );
        }
        OutputFormat::Csv => {
            println!("key,value");
            for (k, mask) in &redacted {
                println!("{k},{mask}");
            }
        }
        OutputFormat::Markdown => {
            println!("| key | value |\n| --- | --- |");
            for (k, mask) in &redacted {
                println!("| {} | {mask} |", md_cell(k));
            }
        }
        OutputFormat::Text => {
            for (k, mask) in &redacted {
                println!("{k}={mask}");
            }
            println!("{} keys (values hidden)", redacted.len());
        }
    }
    Ok(())
}

/// `hangar agent edit`: map the present flags onto an [`AgentConfigUpdate`] and
/// drive the workspace-scoped edit. An empty edit (no field flag) is rejected;
/// an agent id outside the workspace is reported as a not-found error.
async fn run_agent_edit(store: &Store, args: AgentEditArgs) -> Result<()> {
    use ainb_hangar_store::repo::agent::{AgentConfigUpdate, AgentRepo};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;

    // `--env` / `--env-stdin` / `--env-file` are mutually exclusive at the clap
    // layer; whichever is present REPLACES the whole map (parity #30). Resolved
    // FIRST because it borrows `args` whole (the reads below move out of it).
    let agent_env =
        resolve_agent_env_write(&args)?.map(ainb_hangar_core::agent_env::AgentEnv::from_pairs);
    // Each nullable text field uses its clear-flag to distinguish "clear to none"
    // from "leave unchanged" (a clap conflict already bars setting both).
    let instructions = clear_or_set(args.clear_instructions, args.instructions);
    let model = clear_or_set(args.clear_model, args.model);
    let mcp_config = clear_or_set(args.clear_mcp, args.mcp);
    let thinking = clear_or_set(args.clear_thinking, args.thinking);
    // `--arg` / `--env` REPLACE the list when any value is given (an empty Vec
    // means "no flag passed" → leave unchanged).
    let cli_args = (!args.args.is_empty()).then_some(args.args);

    let token_budget = clear_or_set(args.clear_token_budget, args.token_budget);
    // Migration 0050: `description` is NOT NULL so it has no clear-flag (`""` IS
    // its cleared state); avatar / service tier follow the clear-flag pattern.
    let description = validated_description(args.description.as_deref())?;
    let avatar_url = clear_or_set(args.clear_avatar, args.avatar);
    let service_tier = clear_or_set(args.clear_service_tier, args.service_tier);

    let update = AgentConfigUpdate {
        name: args.name.clone(),
        instructions,
        model,
        cli_args,
        mcp_config,
        thinking,
        agent_env,
        token_budget,
        description,
        avatar_url,
        service_tier,
    };

    if update.is_empty() {
        anyhow::bail!(
            "nothing to update: pass at least one of --name / --instructions / --clear-instructions \
             / --model / --clear-model / --arg / --mcp / --clear-mcp / --thinking / --clear-thinking \
             / --env / --env-stdin / --env-file / --token-budget / --clear-token-budget / \
             --description / --avatar / \
             --clear-avatar / --service-tier / --clear-service-tier"
        );
    }

    let touched = AgentRepo::update_config(store.pool(), &workspace_id, &args.id, &update)
        .await
        .map_err(|e| {
            // A RENAME onto a taken name is the same refusal create gives.
            if ainb_hangar_store::repo::agent::is_duplicate_name(&e) {
                anyhow::anyhow!(
                    "an agent named `{}` already exists in this workspace",
                    args.name.as_deref().unwrap_or_default()
                )
            } else {
                anyhow::Error::new(e).context(format!("update agent {}", args.id))
            }
        })?;
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
    let by = effective_archiver(store, &workspace_id, args.by.as_deref()).await?;
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
    let touched = AgentRepo::set_archived(
        store.pool(),
        &workspace_id,
        &args.id,
        archived,
        by.as_ref(),
        now,
    )
    .await
    .with_context(|| format!("archive agent {}", args.id))?;
    if touched {
        if archived {
            // Report the audit that was actually written, so the operator can see
            // WHO the archive was attributed to without re-reading the row.
            match &by {
                Some(actor) => println!("archived agent {} by {actor} at {now}", args.id),
                None => println!("archived agent {} at {now} (unattributed)", args.id),
            }
        } else {
            println!("un-archived agent {}", args.id);
        }
    } else {
        anyhow::bail!("no agent with id {} in this workspace", args.id);
    }
    Ok(())
}

/// Resolve the actor recorded as `archived_by` for a CLI archive (migration 0052),
/// with the SAME precedence the daemon uses
/// (`rpc::snapshots::effective_archiver`): an explicit `--by` user id, else the
/// workspace owner, else `None` (an honestly unattributed archive). Keeping the
/// two in lockstep means an archive reads identically whether it came from the CLI
/// or the RPC surface.
async fn effective_archiver(
    store: &Store,
    workspace_id: &str,
    supplied: Option<&str>,
) -> Result<Option<ainb_hangar_core::actor::ActorRef>> {
    use ainb_hangar_core::actor::{ActorKind, ActorRef};
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::workspace::WorkspaceRepo;

    if let Some(id) = supplied.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(ActorRef::new(ActorKind::Member, id).ok());
    }
    let Ok(ws) = WorkspaceId::from_str(workspace_id.to_string()) else {
        return Ok(None);
    };
    let owner = WorkspaceRepo::owner_id(store.pool(), &ws)
        .await
        .context("resolve the workspace owner as the default archiving actor")?;
    Ok(owner.and_then(|id| ActorRef::new(ActorKind::Member, id).ok()))
}

/// Collapse a `(clear_flag, optional_value)` pair into the store's nested-`Option`
/// three-state: the clear flag wins (`Some(None)`), else a present value sets
/// (`Some(Some(v))`), else leave unchanged (`None`). The clap `conflicts_with`
/// already bars both at once.
#[allow(clippy::option_option)] // the nested Option IS the store's 3-state encoding
fn clear_or_set<T>(clear: bool, value: Option<T>) -> Option<Option<T>> {
    if clear { Some(None) } else { value.map(Some) }
}

/// Dispatch the `hangar member` verbs (e38.11).
///
/// Opens the store, resolves the workspace the same way the skills/agent verbs do,
/// and drives the workspace-scoped [`MemberRepo`]. `list` shows the members;
/// `set-role` changes a role; `remove` drops a membership. Both mutations surface
/// the store's last-owner guard as a CLI error (a workspace must keep an owner).
///
/// [`MemberRepo`]: ainb_hangar_store::repo::member::MemberRepo
async fn dispatch_member(cmd: MemberCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        MemberCommand::Add(args) => run_member_add(&store, args).await,
        MemberCommand::List(args) => run_member_list(&store, args, format).await,
        MemberCommand::SetRole(args) => run_member_set_role(&store, args).await,
        MemberCommand::Remove(args) => run_member_remove(&store, args).await,
        MemberCommand::Invite(args) => run_member_invite(&store, args).await,
        MemberCommand::Invites(args) => run_member_invites(&store, args, format).await,
        MemberCommand::Accept(args) => run_member_invite_accept(&store, args).await,
        MemberCommand::Decline(args) => run_member_invite_decline(&store, args).await,
        MemberCommand::Revoke(args) => run_member_invite_revoke(&store, args).await,
    }
}

/// `hangar member invite`: issue a PENDING invitation (parity #18).
///
/// Unlike [`run_member_add`] (the instant join) this writes no member — the
/// membership only appears when the invitee runs `member accept`. `--from`
/// resolves the inviter's email to a `user.id`; an unknown address is an error
/// naming it, so a typo never silently invites "from" nobody.
async fn run_member_invite(store: &Store, args: MemberInviteArgs) -> Result<()> {
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::invitation::InvitationRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let inviter_email = args
        .from
        .unwrap_or_else(|| ainb_hangar_store::bootstrap::DEFAULT_OWNER_EMAIL.to_string());
    let inviter_id = user_id_for_email(store, &inviter_email).await?;

    let inv = InvitationRepo::create(
        store.pool(),
        &SystemClock,
        &ws,
        &inviter_id,
        &args.email,
        args.role.to_repo(),
    )
    .await
    .map_err(invitation_cli_err)?;
    println!(
        "invited {} as {} (invitation {}, expires {})",
        inv.invitee_email,
        inv.role,
        inv.id,
        fmt_epoch_ms_utc(inv.expires_at)
    );
    Ok(())
}

/// `hangar member invites`: list the workspace's LIVE pending invitations.
///
/// Sweeps past-due rows to `expired` first, so what prints is what can still be
/// accepted. A missing / unknown workspace lists as empty, mirroring
/// [`run_member_list`].
async fn run_member_invites(
    store: &Store,
    args: MemberInvitesArgs,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::invitation::InvitationRepo;

    let workspace_id = match args.workspace.as_deref() {
        Some(slug) => sqlx::query_scalar("SELECT id FROM workspace WHERE slug = ?")
            .bind(slug)
            .fetch_optional(store.pool())
            .await
            .context("look up workspace by slug")?,
        None => find_default_workspace(store).await?,
    };
    let Some(workspace_id) = workspace_id else {
        render_invitation_list(&[], format);
        return Ok(());
    };
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    InvitationRepo::expire_stale(store.pool(), &SystemClock, &ws)
        .await
        .context("expire stale invitations")?;
    let invites = InvitationRepo::list_pending(store.pool(), &SystemClock, &ws)
        .await
        .context("list pending invitations")?;
    render_invitation_list(&invites, format);
    Ok(())
}

/// `hangar member accept`: the invitee joins. This is the verb that adds the
/// member row (the invitation and the membership land in one transaction).
async fn run_member_invite_accept(store: &Store, args: MemberInviteActArgs) -> Result<()> {
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_store::repo::invitation::InvitationRepo;

    let member = InvitationRepo::accept(
        store.pool(),
        &SystemClock,
        &args.invitation_id,
        &args.acting_as,
    )
    .await
    .map_err(invitation_cli_err)?;
    println!(
        "{} joined as {} (user {})",
        member.email, member.role, member.user_id
    );
    Ok(())
}

/// `hangar member decline`: the invitee refuses. No member is created.
async fn run_member_invite_decline(store: &Store, args: MemberInviteActArgs) -> Result<()> {
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_store::repo::invitation::InvitationRepo;

    InvitationRepo::decline(
        store.pool(),
        &SystemClock,
        &args.invitation_id,
        &args.acting_as,
    )
    .await
    .map_err(invitation_cli_err)?;
    println!(
        "declined invitation {} for {}",
        args.invitation_id, args.acting_as
    );
    Ok(())
}

/// `hangar member revoke`: withdraw a still-pending invitation, workspace-scoped
/// (another tenant's invitation matches no row and is a not-found error).
async fn run_member_invite_revoke(store: &Store, args: MemberInviteRevokeArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::invitation::InvitationRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    InvitationRepo::revoke(store.pool(), &ws, &args.invitation_id)
        .await
        .map_err(invitation_cli_err)?;
    println!("revoked invitation {}", args.invitation_id);
    Ok(())
}

/// Resolve an email to its `user.id`, erroring with the address when unknown.
async fn user_id_for_email(store: &Store, email: &str) -> Result<String> {
    let normalized = ainb_hangar_store::repo::invitation::normalize_email(email);
    let id: Option<String> = sqlx::query_scalar("SELECT id FROM user WHERE email = ?")
        .bind(&normalized)
        .fetch_optional(store.pool())
        .await
        .context("look up user by email")?;
    id.ok_or_else(|| anyhow::anyhow!("no user with email {normalized}"))
}

/// Map an [`InvitationRepoError`] onto a human CLI error, keeping multica's
/// wording so the CLI, the RPC, and the reference all say the same thing.
///
/// [`InvitationRepoError`]: ainb_hangar_store::repo::invitation::InvitationRepoError
fn invitation_cli_err(
    e: ainb_hangar_store::repo::invitation::InvitationRepoError,
) -> anyhow::Error {
    use ainb_hangar_store::repo::invitation::InvitationRepoError as E;
    match e {
        E::EmptyEmail => anyhow::anyhow!("email must not be empty"),
        E::InvalidRole => anyhow::anyhow!("role must be one of admin/member"),
        E::CannotInviteOwner => anyhow::anyhow!("cannot invite as owner"),
        E::InviterNotMember => anyhow::anyhow!("only a workspace member can invite"),
        E::AlreadyMember => anyhow::anyhow!("user is already a member"),
        E::AlreadyPending => anyhow::anyhow!("invitation already pending for this email"),
        E::NotFound => anyhow::anyhow!("invitation not found"),
        E::NotYours => anyhow::anyhow!("invitation does not belong to you"),
        E::NotPending => anyhow::anyhow!("invitation is not pending"),
        E::Expired => anyhow::anyhow!("invitation has expired"),
        other => anyhow::Error::new(other).context("invitation mutation failed"),
    }
}

/// `hangar member add`: find-or-create the user by email, then join the member
/// to the workspace. Mirrors [`run_member_set_role`] (talks to the store
/// directly, not the daemon). Prints the minted/reused user id, email, and role.
async fn run_member_add(store: &Store, args: MemberAddArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::member::MemberRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let member = MemberRepo::add(store.pool(), &ws, &args.email, args.role.to_repo())
        .await
        .map_err(member_cli_err)?;
    println!(
        "added member {} ({}) as {}",
        member.user_id, member.email, member.role
    );
    Ok(())
}

/// `hangar member list`: list the workspace's members (email + role).
async fn run_member_list(store: &Store, args: MemberListArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::member::MemberRepo;

    // A missing/empty workspace lists as no members, not an error (mirrors agents).
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
        render_member_list(&[], format);
        return Ok(());
    };
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let members = MemberRepo::list(store.pool(), &ws).await.context("list members")?;
    render_member_list(&members, format);
    Ok(())
}

/// `hangar member set-role`: change a member's role, workspace-scoped. The
/// store's last-owner guard rejects demoting the workspace's sole owner; a member
/// id outside the workspace is reported as a not-found error.
async fn run_member_set_role(store: &Store, args: MemberSetRoleArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::member::MemberRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    MemberRepo::set_role(store.pool(), &ws, &args.user_id, args.role.to_repo())
        .await
        .map_err(member_cli_err)?;
    println!(
        "set role of member {} to {}",
        args.user_id,
        args.role.to_repo().as_str()
    );
    Ok(())
}

/// `hangar member remove`: drop a member's membership, workspace-scoped. The
/// store's last-owner guard rejects removing the workspace's sole owner.
async fn run_member_remove(store: &Store, args: MemberRemoveArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::member::MemberRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    MemberRepo::remove(store.pool(), &ws, &args.user_id)
        .await
        .map_err(member_cli_err)?;
    println!("removed member {}", args.user_id);
    Ok(())
}

/// Map a [`MemberRepoError`] onto a human CLI error, surfacing the not-found and
/// last-owner rejections with their own clear messages.
///
/// [`MemberRepoError`]: ainb_hangar_store::repo::member::MemberRepoError
fn member_cli_err(e: ainb_hangar_store::repo::member::MemberRepoError) -> anyhow::Error {
    use ainb_hangar_store::repo::member::MemberRepoError;
    match e {
        MemberRepoError::NotFound => {
            anyhow::anyhow!("no member with that user id in this workspace")
        }
        MemberRepoError::LastOwner => {
            anyhow::anyhow!("a workspace must always keep at least one owner")
        }
        MemberRepoError::AlreadyMember => {
            anyhow::anyhow!("that user is already a member of this workspace")
        }
        MemberRepoError::EmptyEmail => anyhow::anyhow!("email must not be empty"),
        other => anyhow::Error::new(other).context("member mutation failed"),
    }
}

/// Dispatch the `hangar workspace` verbs against a local store (e38.21).
///
/// `create` makes a workspace (refused under the instance lockdown), `list`
/// enumerates them host-wide, `config` sets one or more of the workspace's
/// agent-run config knobs (context prompt, issue prefix, repo whitelist), and
/// `show` renders the current config. The scoped verbs resolve the workspace the
/// same way the skills/member verbs do.
async fn dispatch_workspace(cmd: WorkspaceCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        WorkspaceCommand::Create(args) => run_workspace_create(&store, args).await,
        WorkspaceCommand::List(args) => run_workspace_list(&store, &args, format).await,
        WorkspaceCommand::Config(args) => run_workspace_config(&store, args).await,
        WorkspaceCommand::Show(args) => run_workspace_show(&store, args, format).await,
    }
}

/// `hangar workspace create`: validate the slug, then create the workspace + its
/// owner member row.
///
/// The instance lockdown (`workspace.creation_disabled`) is enforced store-side
/// inside `WorkspaceRepo::create`, so a locked-down instance surfaces
/// [`WorkspaceRepoError::CreationDisabled`] here and the command exits non-zero
/// having written nothing.
///
/// [`WorkspaceRepoError::CreationDisabled`]: ainb_hangar_store::repo::workspace::WorkspaceRepoError::CreationDisabled
async fn run_workspace_create(store: &Store, args: WorkspaceCreateArgs) -> Result<()> {
    use ainb_hangar_store::repo::workspace::{WorkspaceRepo, validate_slug};

    // A create needs the bootstrap owner user to link as the new workspace's
    // `owner` member, so seed it the way every other write verb does rather than
    // failing "no such workspace" on a never-touched database. This is
    // platform-owned creation and is deliberately NOT gated by the lockdown — a
    // locked-down instance still needs its default tenant.
    ensure_default_workspace(store).await?;
    let slug = validate_slug(&args.slug).map_err(workspace_cli_err)?;
    let row = WorkspaceRepo::create(
        store.pool(),
        &slug,
        &args.name,
        args.issue_prefix.as_deref(),
    )
    .await
    .map_err(workspace_cli_err)?;
    println!("created workspace {} ({})", row.slug, row.id);
    Ok(())
}

/// `hangar workspace list`: every workspace on this instance, in creation order.
///
/// Host-wide (no `--workspace` scope) — it is the surface that answers "did the
/// create land?", so it reads the `workspace` table directly rather than any
/// per-workspace config.
async fn run_workspace_list(
    store: &Store,
    _args: &WorkspaceListArgs,
    format: OutputFormat,
) -> Result<()> {
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT id, slug, name FROM workspace ORDER BY created_at")
            .fetch_all(store.pool())
            .await
            .context("list workspaces")?;

    match format {
        OutputFormat::Json => {
            let v: Vec<serde_json::Value> = rows
                .iter()
                .map(|(id, slug, name)| serde_json::json!({ "id": id, "slug": slug, "name": name }))
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&v).context("render workspace list json")?
            );
        }
        OutputFormat::Csv => {
            println!("id,slug,name");
            for (id, slug, name) in &rows {
                println!("{id},{slug},{name}");
            }
        }
        OutputFormat::Markdown => {
            println!("| id | slug | name |");
            println!("| --- | --- | --- |");
            for (id, slug, name) in &rows {
                println!("| {id} | {slug} | {name} |");
            }
        }
        OutputFormat::Text => {
            if rows.is_empty() {
                println!("no workspaces");
            } else {
                for (id, slug, name) in &rows {
                    println!("{id}  {slug}  {name}");
                }
            }
        }
    }
    Ok(())
}

/// `hangar workspace config`: overwrite one or more of the workspace's config
/// knobs. A value flag sets the knob; a `--clear-…` flag unsets it; a knob whose
/// flags are both absent keeps its stored value (a read-modify-write merge).
async fn run_workspace_config(store: &Store, args: WorkspaceConfigArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::workspace::WorkspaceRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;

    // Read-modify-write: start from the stored config so an unspecified knob is
    // preserved, then apply each flag.
    let mut config = WorkspaceRepo::get_config(store.pool(), &ws)
        .await
        .map_err(workspace_cli_err)?
        .context("workspace vanished")?;
    if args.clear_context_prompt {
        config.context_prompt = None;
    } else if let Some(prompt) = args.context_prompt {
        config.context_prompt = Some(prompt);
    }
    if args.clear_issue_prefix {
        config.issue_prefix = None;
    } else if let Some(prefix) = args.issue_prefix {
        config.issue_prefix = Some(prefix);
    }
    if args.clear_repo_whitelist {
        config.repo_whitelist = None;
    } else if let Some(repos) = args.repo_whitelist {
        config.repo_whitelist = Some(repos);
    }

    WorkspaceRepo::set_config(store.pool(), &ws, &config)
        .await
        .map_err(workspace_cli_err)?;
    println!("updated workspace config");
    Ok(())
}

/// `hangar workspace show`: render the workspace's current config. An
/// unconfigured workspace shows `(not set)` for each knob.
async fn run_workspace_show(
    store: &Store,
    args: WorkspaceShowArgs,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::workspace::WorkspaceRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let config = WorkspaceRepo::get_config(store.pool(), &ws)
        .await
        .map_err(workspace_cli_err)?
        .context("workspace vanished")?;

    match format {
        OutputFormat::Json => {
            let v = serde_json::json!({
                "context_prompt": config.context_prompt,
                "issue_prefix": config.issue_prefix,
                "repo_whitelist": config.repo_whitelist,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&v).context("render config json")?
            );
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            let not_set = "(not set)".to_string();
            println!(
                "context_prompt: {}",
                config.context_prompt.as_deref().unwrap_or(&not_set)
            );
            println!(
                "issue_prefix:   {}",
                config.issue_prefix.as_deref().unwrap_or(&not_set)
            );
            let whitelist = config
                .repo_whitelist
                .as_ref()
                .map_or_else(|| not_set.clone(), |list| list.join(", "));
            println!("repo_whitelist: {whitelist}");
        }
    }
    Ok(())
}

/// Map a [`WorkspaceRepoError`] onto a human CLI error.
///
/// [`WorkspaceRepoError`]: ainb_hangar_store::repo::workspace::WorkspaceRepoError
fn workspace_cli_err(e: ainb_hangar_store::repo::workspace::WorkspaceRepoError) -> anyhow::Error {
    use ainb_hangar_store::repo::workspace::WorkspaceRepoError;
    match e {
        WorkspaceRepoError::NotFound => anyhow::anyhow!("no such workspace"),
        WorkspaceRepoError::BadWhitelist { detail } => {
            anyhow::anyhow!("invalid repo whitelist: {detail}")
        }
        WorkspaceRepoError::BadSlug { detail } => anyhow::anyhow!("invalid slug: {detail}"),
        WorkspaceRepoError::SlugTaken => {
            anyhow::anyhow!("a workspace with that slug already exists")
        }
        WorkspaceRepoError::LastWorkspace => anyhow::anyhow!("cannot delete the last workspace"),
        WorkspaceRepoError::CreationDisabled => anyhow::anyhow!(
            "workspace creation is disabled for this instance \
             (unset with: ainb hangar daemon config set workspace.creation_disabled false)"
        ),
        db @ WorkspaceRepoError::Db(_) => {
            anyhow::Error::new(db).context("workspace config mutation failed")
        }
    }
}

/// Dispatch the `hangar squad` verbs against a local store (e38.17).
///
/// `list` renders the squad status view (each squad's leader + members); `create`
/// makes a squad with a leader actor-ref (resolve-or-reject on a duplicate name);
/// `add-member` / `remove-member` mutate membership, workspace-scoped. The leader
/// actor-ref is how a squad's work routes to a concrete actor — an `agent` leader
/// is the actor a squad-assigned task lands on.
async fn dispatch_squad(cmd: SquadCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        SquadCommand::List(args) => run_squad_list(&store, args, format).await,
        SquadCommand::Create(args) => run_squad_create(&store, args).await,
        SquadCommand::AddMember(args) => run_squad_member(&store, args, true).await,
        SquadCommand::RemoveMember(args) => run_squad_member(&store, args, false).await,
        SquadCommand::Assign(args) => run_squad_assign(&store, args).await,
        SquadCommand::Archive(args) => run_squad_set_archived(&store, args, true).await,
        SquadCommand::Unarchive(args) => run_squad_set_archived(&store, args, false).await,
        SquadCommand::MemberRole(args) => run_squad_member_role(&store, args).await,
        SquadCommand::Instructions(args) => run_squad_instructions(&store, args).await,
        SquadCommand::Briefing(args) => run_squad_briefing(&store, args).await,
    }
}

/// `hangar squad briefing`: print — verbatim, to stdout, with nothing else — the
/// squad-leader briefing the daemon would append to a leader run's `CLAUDE.md`.
///
/// This is the read-only PROMPT-INSPECTION surface for parity #7 / `7-rest`:
/// before this, the only way to see a leader's injected protocol + roster (with
/// each member's role and materialisable skills) + instructions was to run a
/// task and read the file off the task tree. It calls the very same
/// `build_squad_leader_briefing` the claim path calls, so what it prints is what
/// the leader gets — not a re-implementation that can drift.
///
/// A squad whose leader is a human `member` has no agent runtime to brief, so
/// there is no briefing to print: that exits non-zero with an explanation,
/// mirroring the builder's `None`.
async fn run_squad_briefing(store: &Store, args: SquadBriefingArgs) -> Result<()> {
    use ainb_hangar_core::actor::ActorKind;
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_daemon::squad_briefing::build_squad_leader_briefing;
    use ainb_hangar_store::repo::squad::SquadRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let squad = SquadRepo::get(store.pool(), &ws, &args.id)
        .await
        .context("read squad")?
        .with_context(|| format!("no squad {} in this workspace", args.id))?;
    if squad.leader.kind() != ActorKind::Agent {
        anyhow::bail!(
            "squad {} has a human leader; no agent briefing is built",
            args.id
        );
    }
    let briefing = build_squad_leader_briefing(store.pool(), &ws, &args.id, squad.leader.id())
        .await
        .with_context(|| format!("squad {} builds no leader briefing", args.id))?;
    print!("{briefing}");
    Ok(())
}

/// `hangar squad archive|unarchive`: flip the archived flag with its audit stamp,
/// workspace-scoped (parity #26).
async fn run_squad_set_archived(
    store: &Store,
    args: SquadArchiveArgs,
    archived: bool,
) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::squad::SquadRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let by = effective_archiver(store, &workspace_id, args.by.as_deref()).await?;
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    SquadRepo::set_archived(store.pool(), &ws, &args.id, archived, by.as_ref(), now)
        .await
        .map_err(squad_cli_err)?;
    if archived {
        match &by {
            Some(actor) => println!("archived squad {} by {actor} at {now}", args.id),
            None => println!("archived squad {} at {now} (unattributed)", args.id),
        }
    } else {
        println!("un-archived squad {}", args.id);
    }
    Ok(())
}

/// `hangar squad list`: render the workspace's squads (name, leader, members).
async fn run_squad_list(store: &Store, args: SquadListArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::squad::SquadRepo;

    // A missing/empty workspace lists as no squads, not an error (mirrors members).
    let workspace_id = match args.workspace.as_deref() {
        Some(slug) => sqlx::query_scalar::<_, String>("SELECT id FROM workspace WHERE slug = ?")
            .bind(slug)
            .fetch_optional(store.pool())
            .await
            .context("look up workspace by slug")?,
        None => find_default_workspace(store).await?,
    };
    let Some(workspace_id) = workspace_id else {
        render_squad_list(&[], format);
        return Ok(());
    };
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let squads = if args.all {
        SquadRepo::list_including_archived(store.pool(), &ws).await
    } else {
        SquadRepo::list(store.pool(), &ws).await
    }
    .context("list squads")?;
    render_squad_list(&squads, format);
    Ok(())
}

/// `hangar squad create`: create a squad with a leader actor-ref, workspace-scoped.
/// A duplicate squad name in the workspace is rejected (resolve-or-reject).
async fn run_squad_create(store: &Store, args: SquadCreateArgs) -> Result<()> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_core::idgen::IdGen;
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::squad::SquadRepo;

    let leader: ActorRef = args.leader.parse().with_context(|| {
        format!(
            "leader must be `agent:<id>` or `member:<id>`: {}",
            args.leader
        )
    })?;
    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let id = SystemIdGen.new_ulid();
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
    SquadRepo::create(store.pool(), &ws, &id, &args.name, &leader, now)
        .await
        .map_err(squad_cli_err)?;
    // Optional initial routing guidance (migration 0053). `create`'s signature
    // stays unchanged — the two writes are one logical unit here.
    if let Some(instructions) =
        args.instructions.as_deref().map(str::trim).filter(|t| !t.is_empty())
    {
        SquadRepo::set_instructions(store.pool(), &ws, &id, instructions)
            .await
            .map_err(squad_cli_err)?;
    }
    println!("created squad {} ({}) led by {}", args.name, id, leader);
    Ok(())
}

/// `hangar squad add-member` / `remove-member`: mutate one squad's membership,
/// workspace-scoped. A squad id outside the workspace is a not-found error.
async fn run_squad_member(store: &Store, args: SquadMemberArgs, add: bool) -> Result<()> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::squad::SquadRepo;

    let member: ActorRef = args.member.parse().with_context(|| {
        format!(
            "member must be `agent:<id>` or `member:<id>`: {}",
            args.member
        )
    })?;
    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    if add {
        // An explicit `--role` is explicit intent, so it OVERWRITES on a re-add;
        // omitting it keeps the idempotent `DO NOTHING` path, which never clears
        // a role an operator already set.
        match args.role.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            Some(role) => {
                SquadRepo::add_member_with_role(store.pool(), &ws, &args.squad_id, &member, role)
                    .await
                    .map_err(squad_cli_err)?;
                println!(
                    "added {member} to squad {} with role \"{role}\"",
                    args.squad_id
                );
            }
            None => {
                SquadRepo::add_member(store.pool(), &ws, &args.squad_id, &member)
                    .await
                    .map_err(squad_cli_err)?;
                println!("added {member} to squad {}", args.squad_id);
            }
        }
    } else {
        SquadRepo::remove_member(store.pool(), &ws, &args.squad_id, &member)
            .await
            .map_err(squad_cli_err)?;
        println!("removed {member} from squad {}", args.squad_id);
    }
    Ok(())
}

/// `hangar squad member-role`: set or clear an EXISTING membership's free-text
/// role, workspace-scoped (migration 0053).
///
/// An actor that is not already a member is a hard error with a non-zero exit —
/// never an "ok" on a no-op — mirroring the RPC handler's `INVALID_PARAMS`.
async fn run_squad_member_role(store: &Store, args: SquadMemberRoleArgs) -> Result<()> {
    use ainb_hangar_core::actor::ActorRef;
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::squad::SquadRepo;

    let member: ActorRef = args.member.parse().with_context(|| {
        format!(
            "member must be `agent:<id>` or `member:<id>`: {}",
            args.member
        )
    })?;
    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let updated =
        SquadRepo::set_member_role(store.pool(), &ws, &args.squad_id, &member, &args.role)
            .await
            .map_err(squad_cli_err)?;
    if !updated {
        anyhow::bail!(
            "{member} is not a member of squad {} — add it first",
            args.squad_id
        );
    }
    let role = args.role.trim();
    if role.is_empty() {
        println!("cleared the role of {member} on squad {}", args.squad_id);
    } else {
        println!(
            "set the role of {member} on squad {} to \"{role}\"",
            args.squad_id
        );
    }
    Ok(())
}

/// `hangar squad instructions`: show (no flag), set (`--set`), or clear
/// (`--clear`) a squad's user-authored routing guidance (migration 0053).
///
/// The text is stored VERBATIM — it reaches an agent's materialised `CLAUDE.md`
/// through the leader briefing. Clearing makes that briefing omit the
/// `## Squad Instructions` section entirely.
async fn run_squad_instructions(store: &Store, args: SquadInstructionsArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::squad::SquadRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;

    if args.set.is_some() || args.clear {
        let text = if args.clear {
            ""
        } else {
            args.set.as_deref().unwrap_or_default()
        };
        SquadRepo::set_instructions(store.pool(), &ws, &args.squad_id, text)
            .await
            .map_err(squad_cli_err)?;
        if text.trim().is_empty() {
            println!("cleared the instructions of squad {}", args.squad_id);
        } else {
            println!("set the instructions of squad {}", args.squad_id);
        }
        return Ok(());
    }

    let squad = SquadRepo::get(store.pool(), &ws, &args.squad_id)
        .await
        .context("read squad")?
        .with_context(|| format!("no squad {} in this workspace", args.squad_id))?;
    if squad.instructions.is_empty() {
        println!("squad {} has no instructions", args.squad_id);
    } else {
        println!("{}", squad.instructions);
    }
    Ok(())
}

/// `hangar squad assign`: route a task to the squad's LEADER — leader routing
/// taking effect. The daemon-free path the daemon RPC mirrors: resolve the
/// squad's leader agent, derive the leader's runtime, and enqueue the task keyed
/// to the leader so the claim path dispatches it to the leader. A human-member or
/// unknown-squad leader (no agent to dispatch to) is rejected.
///
/// `--fanout` fans across the WHOLE squad (leader brief + one task per distinct
/// `agent` member) — the daemon-free proof of the same service the RPC drives.
/// `--invoker` names the user the gap #8 invocation gate judges every dispatch
/// target by; omitted, the service resolves the workspace owner.
async fn run_squad_assign(store: &Store, args: SquadAssignArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::service::squad_assign::{SquadAssignRequest, SquadAssignService};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let invoker = match args.invoker.as_deref() {
        Some(token) => Some(resolve_user_id(store, token).await?),
        None => None,
    };
    let request = SquadAssignRequest {
        issue_id: args.issue.as_deref(),
        work_dir: args.work_dir.as_deref(),
        priority: args.priority,
        invoker: invoker.as_deref(),
        // The CLI assign carries no card repo/agent (tcp T4): the task runs in-tree,
        // exactly the pre-T4 behaviour.
        ..SquadAssignRequest::default()
    };
    if args.fanout {
        let fanout = SquadAssignService::assign_fanout(
            store.pool(),
            &ws,
            &args.squad_id,
            &request,
            &SystemIdGen,
            &SystemClock,
        )
        .await
        .map_err(squad_assign_cli_err)?;
        println!(
            "briefed leader {} with task {} (runtime {})",
            fanout.leader.leader_agent_id, fanout.leader.task_id, fanout.leader.runtime_id
        );
        for m in &fanout.members {
            println!(
                "fanned task {} to member {} (runtime {})",
                m.task_id, m.agent_id, m.runtime_id
            );
        }
        return Ok(());
    }
    let assignment = SquadAssignService::assign_to_leader(
        store.pool(),
        &ws,
        &args.squad_id,
        &request,
        &SystemIdGen,
        &SystemClock,
    )
    .await
    .map_err(squad_assign_cli_err)?;
    println!(
        "assigned task {} to squad {} leader {} (runtime {})",
        assignment.task_id, args.squad_id, assignment.leader_agent_id, assignment.runtime_id
    );
    Ok(())
}

/// Map a [`SquadAssignError`] onto a human CLI error: a no-agent-leader /
/// missing-leader rejection gets a clear message, a store fault is contextualised.
///
/// [`SquadAssignError`]: ainb_hangar_store::service::squad_assign::SquadAssignError
fn squad_assign_cli_err(
    e: ainb_hangar_store::service::squad_assign::SquadAssignError,
) -> anyhow::Error {
    use ainb_hangar_store::service::squad_assign::SquadAssignError;
    match e {
        SquadAssignError::NoAgentLeader => anyhow::anyhow!(
            "squad has no agent leader to route to (unknown squad or a human leader)"
        ),
        SquadAssignError::LeaderAgentMissing(id) => {
            anyhow::anyhow!("squad leader agent `{id}` not found")
        }
        SquadAssignError::MemberAgentMissing(id) => {
            anyhow::anyhow!("squad member agent `{id}` not found")
        }
        // Two pre-flight refusals that write NO task row: the gap-#8 invocation
        // gate, and the parity-#26 archived-squad guard. Both are surfaced
        // verbatim so the CLI exits non-zero with the store's own reason.
        e @ (SquadAssignError::NotInvocable { .. } | SquadAssignError::Archived(_)) => {
            anyhow::anyhow!("{e}")
        }
        db @ SquadAssignError::Db(_) => anyhow::Error::new(db).context("squad assign failed"),
    }
}

/// Map a [`SquadRepoError`] onto a human CLI error, surfacing the duplicate-name
/// and not-found rejections with their own clear messages.
///
/// [`SquadRepoError`]: ainb_hangar_store::repo::squad::SquadRepoError
fn squad_cli_err(e: ainb_hangar_store::repo::squad::SquadRepoError) -> anyhow::Error {
    use ainb_hangar_store::repo::squad::SquadRepoError;
    match e {
        SquadRepoError::DuplicateName => {
            anyhow::anyhow!("a squad with that name already exists in this workspace")
        }
        SquadRepoError::NotFound => {
            anyhow::anyhow!("no squad with that id in this workspace")
        }
        db @ SquadRepoError::Db(_) => anyhow::Error::new(db).context("squad mutation failed"),
    }
}

/// Dispatch the `hangar skills` verbs.
async fn dispatch_skills(cmd: SkillsCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        SkillsCommand::Sync(args) => run_skills_sync(&store, args).await,
        SkillsCommand::List(args) => run_skills_list(&store, args, format).await,
        SkillsCommand::Attach(args) => run_skills_link(&store, args, true).await,
        SkillsCommand::Detach(args) => run_skills_link(&store, args, false).await,
        SkillsCommand::Toggle(args) => run_skills_toggle(&store, args).await,
    }
}

/// Resolve a skill reference (an id, else a kebab-case name) within `ws`.
///
/// Id-first so an explicit ULID always wins; the name fallback exists because
/// nothing in the CLI surface prints skill ids.
async fn resolve_skill_ref(
    store: &Store,
    ws: &ainb_hangar_core::ids::WorkspaceId,
    reference: &str,
) -> Result<ainb_hangar_core::ids::SkillId> {
    use ainb_hangar_store::repo::skill::SkillRepo;

    let skills = SkillRepo::list(store.pool(), ws).await.context("list skills")?;
    if let Some(hit) = skills.iter().find(|s| s.id.as_str() == reference) {
        return Ok(hit.id.clone());
    }
    let normalised = ainb_hangar_core::skill::SkillName::new(reference)
        .with_context(|| format!("`{reference}` is not a usable skill name"))?;
    if let Some(hit) = skills.iter().find(|s| s.name == normalised) {
        return Ok(hit.id.clone());
    }
    let available: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    anyhow::bail!(
        "no skill `{reference}` in this workspace (available: {})",
        if available.is_empty() {
            "none — run `ainb hangar skills sync` first".to_string()
        } else {
            available.join(", ")
        }
    )
}

/// Resolve an agent reference (an id, else a name) within `ws`.
///
/// Name resolution is unambiguous: migration 0050's `agent_workspace_name_unique`
/// index means one agent per name per workspace.
async fn resolve_agent_ref(
    store: &Store,
    ws: &ainb_hangar_core::ids::WorkspaceId,
    reference: &str,
) -> Result<ainb_hangar_core::ids::AgentId> {
    use ainb_hangar_store::repo::agent::AgentRepo;

    let agents = AgentRepo::list_by_workspace_including_archived(store.pool(), ws.as_str())
        .await
        .context("list agents")?;
    let hit = agents
        .iter()
        .find(|a| a.id == reference)
        .or_else(|| agents.iter().find(|a| a.name == reference));
    let Some(agent) = hit else {
        let available: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        anyhow::bail!(
            "no agent `{reference}` in this workspace (available: {})",
            if available.is_empty() {
                "none — run `ainb hangar agent create --name <name>` first".to_string()
            } else {
                available.join(", ")
            }
        );
    };
    ainb_hangar_core::ids::AgentId::from_str(agent.id.clone()).context("agent id was empty")
}

/// `hangar skills attach|detach <skill> --agent <agent>`: mutate one junction row.
///
/// Attach is idempotent and — per parity #24 deviation D2 — never re-enables a
/// link an operator has deliberately disabled; use `skills toggle` for that.
async fn run_skills_link(store: &Store, args: SkillsLinkArgs, attach: bool) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::skill::SkillRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let skill = resolve_skill_ref(store, &ws, &args.skill).await?;
    let agent = resolve_agent_ref(store, &ws, &args.agent).await?;

    if attach {
        SkillRepo::attach_to_agent(store.pool(), &ws, &agent, &skill)
            .await
            .context("attach skill to agent")?;
        println!("attached {} to {}", args.skill, args.agent);
    } else {
        SkillRepo::detach_from_agent(store.pool(), &ws, &agent, &skill)
            .await
            .context("detach skill from agent")?;
        println!("detached {} from {}", args.skill, args.agent);
    }
    Ok(())
}

/// `hangar skills toggle <skill> --agent <agent> --enabled <bool>` (parity #24).
///
/// Keeps the attachment and flips only whether it materialises. A pair that is
/// not attached is reported as such rather than silently succeeding.
async fn run_skills_toggle(store: &Store, args: SkillsToggleArgs) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::skill::SkillRepo;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;
    let skill = resolve_skill_ref(store, &ws, &args.skill).await?;
    let agent = resolve_agent_ref(store, &ws, &args.agent).await?;

    let toggled = SkillRepo::set_enabled(store.pool(), &ws, &agent, &skill, args.enabled)
        .await
        .context("toggle skill enablement")?;
    if toggled {
        let state = if args.enabled { "enabled" } else { "disabled" };
        println!("{state} {} for {}", args.skill, args.agent);
    } else {
        println!(
            "{} is not attached to {} — nothing to toggle",
            args.skill, args.agent
        );
    }
    Ok(())
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
/// else a walk up to `ainb-toolkit/skills`), then either previews the
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

    // `--agent` switches to the ATTACHMENT listing (parity #24): every link on
    // that agent with its enabled/disabled state. Rendered by its own function
    // so the workspace listing's CSV header / markdown header stay untouched.
    if let Some(agent_ref) = args.agent.as_deref() {
        let agent = resolve_agent_ref(store, &ws, agent_ref).await?;
        let links = SkillRepo::agent_skill_links(store.pool(), &ws, &agent)
            .await
            .context("list agent skill links")?;
        render_agent_skill_links(&links, format);
        return Ok(());
    }

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
        IssueCommand::Search(args) => run_issue_search(&store, args, format).await,
        IssueCommand::Show(args) => run_issue_show(&store, args, format).await,
        IssueCommand::Update(args) => run_issue_update(&store, args).await,
        IssueCommand::BatchState(args) => run_issue_batch_state(&store, args).await,
        IssueCommand::Delete(args) => run_issue_delete(&store, args).await,
        IssueCommand::Label(cmd) => run_issue_label(&store, cmd).await,
        IssueCommand::Criteria(cmd) => run_issue_criteria(&store, cmd).await,
        IssueCommand::Link(cmd) => run_issue_link(&store, cmd).await,
        IssueCommand::Subscribe(args) => run_issue_subscribe(&store, args, true).await,
        IssueCommand::Unsubscribe(args) => run_issue_subscribe(&store, args, false).await,
        IssueCommand::Subscribers(args) => {
            let ws = resolve_skills_workspace(&store, args.workspace.as_deref()).await?;
            print_issue_subscribers(&store, &ws, &args.id).await
        }
        IssueCommand::React(cmd) => run_issue_react(&store, cmd).await,
        IssueCommand::Why(args) => run_issue_why(&store, args, format).await,
        IssueCommand::Timeline(args) => run_issue_timeline(&store, args, format).await,
        IssueCommand::Property(cmd) => run_issue_property(&store, cmd).await,
        IssueCommand::Meta(cmd) => run_issue_meta(&store, cmd, format).await,
    }
}

/// `hangar issue delete`: preview (default) or perform an issue delete,
/// workspace-scoped and daemon-less.
///
/// Resolves the workspace the same way the other verbs do (`--workspace`, else the
/// bootstrapped `default`). Without `--yes` it prints the [`IssueDeletePreview`]
/// (title + dependent counts + active-task warning) and exits without deleting;
/// with `--yes` it drives the store's single-transaction
/// [`IssueRepo::delete_cascade`], refusing on an active task exactly like the
/// daemon RPC. An unknown / foreign-tenant id is a not-found error, never a silent
/// no-op.
async fn run_issue_delete(store: &Store, args: IssueDeleteArgs) -> Result<()> {
    use ainb_hangar_store::repo::issue::{IssueDeleteError, IssueRepo};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;

    let Some(preview) = IssueRepo::delete_preview(store.pool(), &workspace_id, &args.id)
        .await
        .with_context(|| format!("preview delete of issue {}", args.id))?
    else {
        anyhow::bail!("no issue with id {} in this workspace", args.id);
    };

    if !args.yes {
        // DRY RUN: report what a real delete would remove and exit untouched.
        println!("would delete issue {} \"{}\"", args.id, preview.title);
        println!(
            "  removes: {} comment(s), {} task(s), {} board placement(s), {} activity row(s), plus label links, dependency edges, and usage rows",
            preview.summary.comments,
            preview.summary.tasks,
            preview.summary.placements,
            preview.summary.activities
        );
        if preview.active_tasks > 0 {
            println!(
                "  WARNING: {} active task(s) — cancel the run first, then re-run with --yes",
                preview.active_tasks
            );
        } else {
            println!("  re-run with --yes to perform the delete");
        }
        return Ok(());
    }

    match IssueRepo::delete_cascade(store.pool(), &workspace_id, &args.id).await {
        Ok(summary) => {
            println!(
                "deleted issue {} \"{}\" ({} comment(s), {} task(s), {} placement(s), {} activity row(s))",
                args.id,
                preview.title,
                summary.comments,
                summary.tasks,
                summary.placements,
                summary.activities
            );
            Ok(())
        }
        Err(IssueDeleteError::NotFound) => {
            anyhow::bail!("no issue with id {} in this workspace", args.id)
        }
        Err(e @ IssueDeleteError::ActiveTasks(_)) => anyhow::bail!("{e}"),
        Err(IssueDeleteError::Db(e)) => Err(e).with_context(|| format!("delete issue {}", args.id)),
    }
}

/// `hangar issue label attach|detach`: mutate one issue's labels,
/// workspace-scoped.
///
/// Resolves the workspace the same way the other verbs do (`--workspace`, else
/// the bootstrapped `default`), then drives the workspace-scoped
/// [`LabelRepo`](ainb_hangar_store::repo::label::LabelRepo) mutation. An issue id
/// that resolves to no row in the workspace (an unknown id or a foreign tenant's
/// issue) is reported as an error — never a silent no-op, mirroring the daemon
/// RPC. The label name is validated non-empty. The current label set is printed
/// after the mutation so the caller sees the result.
async fn run_issue_label(store: &Store, cmd: IssueLabelCommand) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_store::repo::label::{LabelRepo, LabelRepoError};

    let (args, attach) = match cmd {
        IssueLabelCommand::Attach(args) => (args, true),
        IssueLabelCommand::Detach(args) => (args, false),
    };
    if args.name.trim().is_empty() {
        anyhow::bail!("label name must not be empty");
    }
    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;

    let result = if attach {
        LabelRepo::attach(
            store.pool(),
            &ws,
            &args.id,
            args.name.trim(),
            args.color.as_deref(),
        )
        .await
    } else {
        LabelRepo::detach(store.pool(), &ws, &args.id, args.name.trim()).await
    };
    // A foreign / unknown issue id is a not-found error, never a silent no-op.
    match result {
        Ok(()) => {}
        Err(LabelRepoError::IssueNotFound) => {
            anyhow::bail!("no issue with id {} in this workspace", args.id)
        }
        Err(e) => {
            return Err(e).with_context(|| format!("update labels on issue {}", args.id));
        }
    }

    let labels = LabelRepo::labels_for_issue(store.pool(), &ws, &args.id)
        .await
        .with_context(|| format!("read labels for issue {}", args.id))?;
    let verb = if attach { "attached" } else { "detached" };
    println!(
        "{verb} `{}` on issue {} (labels: {})",
        args.name.trim(),
        args.id,
        if labels.is_empty() {
            "none".to_string()
        } else {
            labels.join(", ")
        }
    );
    Ok(())
}

/// Render one criterion as the stable `<ordinal>  <id>  <glyph>  <text>` line
/// `hangar issue criteria list` and `hangar issue show` both print.
fn criterion_line(ordinal: usize, criterion: &AcceptanceCriterion) -> String {
    let mut line = format!(
        "{ordinal}  {}  {}  {}",
        criterion.id,
        criterion.glyph(),
        criterion.text
    );
    if criterion.checked {
        let when = criterion
            .checked_at
            .map(fmt_epoch_ms_utc)
            .unwrap_or_else(|| "unknown".to_string());
        let who = criterion.checked_by.as_deref().unwrap_or("unknown");
        line.push_str(&format!("      (checked {when} by {who})"));
    }
    line
}

/// Format an epoch-millis instant as an RFC3339 UTC timestamp for the criteria
/// provenance suffix.
fn fmt_epoch_ms_utc(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map_or_else(
        || ms.to_string(),
        |dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    )
}

/// `hangar issue criteria <verb>`: list / check / uncheck one issue's acceptance
/// criteria (multica parity #11-rest).
///
/// `check` / `uncheck` route through the SAME
/// [`IssueRepo::set_criterion_checked`] store seam the daemon RPC uses, so there
/// is exactly one mutator rather than two divergent ones. A foreign / unknown
/// issue id, or a selector matching no criterion, exits NON-ZERO — never a
/// silent no-op.
async fn run_issue_criteria(store: &Store, cmd: IssueCriteriaCommand) -> Result<()> {
    use ainb_hangar_store::repo::issue::CriterionError;

    let (id, workspace, set) = match &cmd {
        IssueCriteriaCommand::List(args) => (args.id.clone(), args.workspace.clone(), None),
        IssueCriteriaCommand::Check(args) => (
            args.id.clone(),
            args.workspace.clone(),
            Some((args.criterion.clone(), true, args.actor.clone())),
        ),
        IssueCriteriaCommand::Uncheck(args) => (
            args.id.clone(),
            args.workspace.clone(),
            Some((args.criterion.clone(), false, args.actor.clone())),
        ),
    };
    let workspace_id = resolve_skills_workspace(store, workspace.as_deref()).await?;

    let issue = if let Some((criterion, checked, actor)) = set {
        if criterion.trim().is_empty() {
            anyhow::bail!("criterion must not be empty");
        }
        let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
        IssueRepo::set_criterion_checked(
            store.pool(),
            &SystemIdGen,
            &workspace_id,
            &id,
            criterion.trim(),
            checked,
            now,
            actor.as_deref(),
        )
        .await
        .map_err(|e| match e {
            CriterionError::IssueNotFound => {
                anyhow::anyhow!("no issue with id {id} in this workspace")
            }
            CriterionError::CriterionNotFound => anyhow::anyhow!(
                "no acceptance criterion `{}` on issue {id} (use `hangar issue criteria list {id}`)",
                criterion.trim()
            ),
            CriterionError::Conflict => {
                anyhow::anyhow!("criterion changed concurrently; re-read and retry")
            }
            CriterionError::Db(db) => anyhow::Error::new(db).context("set acceptance criterion"),
        })?
    } else {
        let issue = IssueRepo::get_by_id(store.pool(), &id)
            .await
            .context("fetch issue")?
            .with_context(|| format!("no issue with id {id}"))?;
        anyhow::ensure!(
            issue.workspace_id == workspace_id,
            "no issue with id {id} in this workspace"
        );
        issue
    };

    if issue.acceptance_criteria.is_empty() {
        println!("issue {id} has no acceptance criteria");
        return Ok(());
    }
    for (idx, criterion) in issue.acceptance_criteria.iter().enumerate() {
        println!("{}", criterion_line(idx + 1, criterion));
    }
    Ok(())
}

/// `hangar issue subscribe|unsubscribe` (multica parity #22): author an issue's
/// subscriber set.
///
/// Routes through the SAME [`IssueSubscriberRepo`] seam the daemon RPC uses, so
/// there is exactly one mutator. The actor defaults to the LOCAL HUMAN. An
/// unknown / foreign-tenant issue is a NON-ZERO exit with the daemon's own
/// phrasing, never a silent no-op. Prints the refreshed set afterwards.
///
/// [`IssueSubscriberRepo`]: ainb_hangar_store::repo::issue_subscriber::IssueSubscriberRepo
async fn run_issue_subscribe(
    store: &Store,
    args: IssueSubscribeArgs,
    subscribe: bool,
) -> Result<()> {
    use ainb_hangar_store::repo::issue_subscriber::{IssueSubscriberRepo, SubscribeReason};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let actor = parse_actor_arg(args.actor.as_deref())?;
    // The repo's tenant join makes a foreign / unknown issue a silent no-op, so
    // resolve the issue FIRST and fail loudly instead.
    require_issue_in_workspace(store, &workspace_id, &args.id).await?;

    let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
    if subscribe {
        IssueSubscriberRepo::add(
            store.pool(),
            &workspace_id,
            &args.id,
            &actor,
            SubscribeReason::Manual,
            now,
        )
        .await
        .context("subscribe to issue")?;
    } else {
        IssueSubscriberRepo::remove(store.pool(), &workspace_id, &args.id, &actor)
            .await
            .context("unsubscribe from issue")?;
    }
    print_issue_subscribers(store, &workspace_id, &args.id).await
}

/// `hangar issue react add|remove|list` (multica parity #22).
/// `hangar property <verb>` — the workspace's CUSTOM PROPERTY catalog
/// (multica parity #17).
///
/// Opens the store DIRECTLY (the `run_issue_label` precedent) and drives the
/// SAME [`IssuePropertyRepo`] the daemon's RPC handlers use, so the CLI and the
/// RPC can never validate differently.
async fn dispatch_property(cmd: PropertyCommand, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_core::properties::PropertyKind;
    use ainb_hangar_store::repo::issue_property::IssuePropertyRepo;

    let store = Store::open_default().await.context("open hangar database")?;
    let workspace = match &cmd {
        PropertyCommand::Define(a) => a.workspace.clone(),
        PropertyCommand::List(a) => a.workspace.clone(),
        PropertyCommand::Archive(a) => a.workspace.clone(),
    };
    let workspace_id = resolve_skills_workspace(&store, workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;

    match cmd {
        PropertyCommand::Define(args) => {
            // Absent flags KEEP whatever the stored definition has, so
            // `--name` alone is a rename and nothing else moves.
            let existing = IssuePropertyRepo::get_by_key(store.pool(), &ws, &args.key)
                .await
                .context("look up the definition")?;
            let kind = match args.kind.as_deref() {
                Some(raw) => PropertyKind::parse_strict(raw)?,
                None => existing.as_ref().map_or(PropertyKind::Text, |d| d.kind.clone()),
            };
            let name = args.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map_or_else(
                || {
                    existing
                        .as_ref()
                        .map_or_else(|| args.key.trim().to_string(), |d| d.name.clone())
                },
                ToString::to_string,
            );
            let options = if args.options.is_empty() {
                existing.as_ref().map(|d| d.options.clone()).unwrap_or_default()
            } else {
                args.options.clone()
            };
            let position = args.position.unwrap_or_else(|| existing.map_or(0, |d| d.position));
            let def = IssuePropertyRepo::define(
                store.pool(),
                &ws,
                &args.key,
                &name,
                &kind,
                &options,
                position,
                SystemClock.now_ms(),
            )
            .await
            .context("define custom property")?;
            println!(
                "defined `{}` ({}) as {} [{}] pos={}",
                def.key,
                def.name,
                def.kind.as_db_str(),
                def.options.join(", "),
                def.position
            );
            Ok(())
        }
        PropertyCommand::List(args) => {
            let defs = IssuePropertyRepo::list(store.pool(), &ws, args.include_archived)
                .await
                .context("list custom properties")?;
            match format {
                OutputFormat::Json => {
                    let rows: Vec<serde_json::Value> = defs
                        .iter()
                        .map(|d| {
                            serde_json::json!({
                                "key": d.key,
                                "name": d.name,
                                "kind": d.kind.as_db_str(),
                                "options": d.options,
                                "position": d.position,
                                "archived": d.archived_at.is_some(),
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&rows)?);
                }
                _ => {
                    if defs.is_empty() {
                        println!("no custom properties");
                    }
                    for d in &defs {
                        println!(
                            "{}\t{}\t{}\t[{}]\tpos={}\t{}",
                            d.key,
                            d.name,
                            d.kind.as_db_str(),
                            d.options.join(", "),
                            d.position,
                            if d.archived_at.is_some() {
                                "archived"
                            } else {
                                "active"
                            }
                        );
                    }
                }
            }
            Ok(())
        }
        PropertyCommand::Archive(args) => {
            let archived = !args.unarchive;
            let found = IssuePropertyRepo::set_archived(
                store.pool(),
                &ws,
                &args.key,
                archived,
                SystemClock.now_ms(),
            )
            .await
            .context("archive custom property")?;
            if !found {
                anyhow::bail!("no custom property `{}` in this workspace", args.key);
            }
            println!(
                "{} `{}` (stored values are never deleted)",
                if archived { "archived" } else { "un-archived" },
                args.key
            );
            Ok(())
        }
    }
}

/// `hangar issue property set|clear` (multica parity #17).
async fn run_issue_property(store: &Store, cmd: IssuePropertyCommand) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_core::properties::{coerce_value, render_value};
    use ainb_hangar_store::repo::issue_property::IssuePropertyRepo;

    let (id, key, values, workspace, set) = match cmd {
        IssuePropertyCommand::Set(a) => (a.id, a.key, a.values, a.workspace, true),
        IssuePropertyCommand::Clear(a) => (a.id, a.key, Vec::new(), a.workspace, false),
    };
    let workspace_id = resolve_skills_workspace(store, workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;

    if set {
        let def = IssuePropertyRepo::get_by_key(store.pool(), &ws, &key)
            .await
            .context("look up the definition")?
            .filter(|d| d.archived_at.is_none())
            .with_context(|| format!("no active custom property `{key}` in this workspace"))?;
        let value = coerce_value(&def.kind, &values)?;
        IssuePropertyRepo::set_value(store.pool(), &ws, &id, &key, &value)
            .await
            .with_context(|| format!("set property `{key}` on issue {id}"))?;
        println!("{}: {}", def.name, render_value(&value));
    } else {
        let cleared = IssuePropertyRepo::clear_value(store.pool(), &ws, &id, &key)
            .await
            .with_context(|| format!("clear property `{key}` on issue {id}"))?;
        println!(
            "{} `{key}` on issue {id}",
            if cleared { "cleared" } else { "already unset:" }
        );
    }
    Ok(())
}

/// `hangar issue meta list|get|set|delete` (multica parity #17).
async fn run_issue_meta(store: &Store, cmd: IssueMetaCommand, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::ids::WorkspaceId;
    use ainb_hangar_core::properties::{coerce_metadata_value, render_metadata};
    use ainb_hangar_store::repo::issue_metadata::IssueMetadataRepo;

    let workspace = match &cmd {
        IssueMetaCommand::List(a) => a.workspace.clone(),
        IssueMetaCommand::Get(a) | IssueMetaCommand::Delete(a) => a.workspace.clone(),
        IssueMetaCommand::Set(a) => a.workspace.clone(),
    };
    let workspace_id = resolve_skills_workspace(store, workspace.as_deref()).await?;
    let ws = WorkspaceId::from_str(workspace_id).context("workspace id was empty")?;

    match cmd {
        IssueMetaCommand::Set(args) => {
            let value = coerce_metadata_value(&args.value, args.value_type.as_deref())?;
            IssueMetadataRepo::set(store.pool(), &ws, &args.id, &args.key, &value)
                .await
                .with_context(|| format!("set metadata `{}` on issue {}", args.key, args.id))?;
            println!("{} = {}", args.key, render_metadata(&value));
            Ok(())
        }
        IssueMetaCommand::Delete(args) => {
            let removed = IssueMetadataRepo::delete(store.pool(), &ws, &args.id, &args.key)
                .await
                .with_context(|| {
                format!("delete metadata `{}` on issue {}", args.key, args.id)
            })?;
            println!(
                "{} `{}` on issue {}",
                if removed {
                    "deleted"
                } else {
                    "already absent:"
                },
                args.key,
                args.id
            );
            Ok(())
        }
        IssueMetaCommand::Get(args) => {
            let bag = IssueMetadataRepo::get(store.pool(), &ws, &args.id)
                .await
                .with_context(|| format!("read metadata on issue {}", args.id))?;
            let value = bag
                .get(&args.key)
                .with_context(|| format!("no metadata key `{}` on issue {}", args.key, args.id))?;
            println!("{}", render_metadata(value));
            Ok(())
        }
        IssueMetaCommand::List(args) => {
            let bag = IssueMetadataRepo::get(store.pool(), &ws, &args.id)
                .await
                .with_context(|| format!("read metadata on issue {}", args.id))?;
            match format {
                OutputFormat::Json => {
                    let rows: Vec<serde_json::Value> = bag
                        .iter()
                        .map(|(k, v)| {
                            serde_json::json!({
                                "key": k,
                                "value": render_metadata(v),
                                "value_json": ainb_hangar_core::properties::metadata_value_json(v),
                            })
                        })
                        .collect();
                    println!("{}", serde_json::to_string_pretty(&rows)?);
                }
                _ => {
                    if bag.is_empty() {
                        println!("no metadata");
                    }
                    for (k, v) in &bag {
                        println!("{k} = {}", render_metadata(v));
                    }
                }
            }
            Ok(())
        }
    }
}

async fn run_issue_react(store: &Store, cmd: IssueReactCommand) -> Result<()> {
    use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
    use ainb_hangar_store::repo::issue_reaction::{IssueReactionError, IssueReactionRepo};

    let (id, workspace) = match &cmd {
        IssueReactCommand::Add(a) | IssueReactCommand::Remove(a) => {
            (a.id.clone(), a.workspace.clone())
        }
        IssueReactCommand::List(a) => (a.id.clone(), a.workspace.clone()),
    };
    let workspace_id = resolve_skills_workspace(store, workspace.as_deref()).await?;
    require_issue_in_workspace(store, &workspace_id, &id).await?;

    let map = |e: IssueReactionError| match e {
        IssueReactionError::EmptyEmoji => anyhow::anyhow!("emoji is required"),
        IssueReactionError::Db(db) => anyhow::Error::new(db).context("issue reaction"),
    };
    match &cmd {
        IssueReactCommand::Add(args) => {
            let actor = parse_actor_arg(args.actor.as_deref())?;
            IssueReactionRepo::add(
                store.pool(),
                &workspace_id,
                &args.id,
                &actor,
                &args.emoji,
                &SystemIdGen.new_ulid(),
                ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock),
            )
            .await
            .map_err(map)?;
        }
        IssueReactCommand::Remove(args) => {
            let actor = parse_actor_arg(args.actor.as_deref())?;
            IssueReactionRepo::remove(store.pool(), &workspace_id, &args.id, &actor, &args.emoji)
                .await
                .map_err(map)?;
        }
        IssueReactCommand::List(_) => {}
    }
    print_issue_reactions(store, &id).await
}

/// Parse an `--actor` argument, defaulting to the LOCAL HUMAN (`member:me`).
fn parse_actor_arg(raw: Option<&str>) -> Result<ainb_hangar_core::actor::ActorRef> {
    match raw {
        None => Ok(ainb_hangar_core::actor::local_member()),
        Some(token) => <ainb_hangar_core::actor::ActorRef as std::str::FromStr>::from_str(token)
            .map_err(|e| anyhow::anyhow!("bad --actor `{token}`: {e}")),
    }
}

/// Fail loudly when `issue_id` does not live in `workspace_id` — the repos'
/// tenant join would otherwise turn it into a silent no-op.
async fn require_issue_in_workspace(
    store: &Store,
    workspace_id: &str,
    issue_id: &str,
) -> Result<()> {
    let found = IssueRepo::get_by_id(store.pool(), issue_id)
        .await
        .context("read issue")?
        .is_some_and(|i| i.workspace_id == workspace_id);
    if found {
        Ok(())
    } else {
        anyhow::bail!("no issue `{issue_id}` in this workspace")
    }
}

/// Print one issue's subscriber set, one `<actor>  (<reason>)` row each.
/// No subscribers ⇒ `no subscribers`.
async fn print_issue_subscribers(store: &Store, workspace_id: &str, issue_id: &str) -> Result<()> {
    use ainb_hangar_store::repo::issue_subscriber::IssueSubscriberRepo;

    require_issue_in_workspace(store, workspace_id, issue_id).await?;
    let subs = IssueSubscriberRepo::list(store.pool(), issue_id)
        .await
        .context("read subscribers")?;
    if subs.is_empty() {
        println!("no subscribers");
        return Ok(());
    }
    for s in subs {
        println!("{}  ({})", s.actor, s.reason_raw);
    }
    Ok(())
}

/// Print one issue's aggregated reactions as `<emoji> <count>`, most-used first.
/// No reactions ⇒ `no reactions`.
async fn print_issue_reactions(store: &Store, issue_id: &str) -> Result<()> {
    use ainb_hangar_store::repo::issue_reaction::IssueReactionRepo;

    let tallies = IssueReactionRepo::tallies(store.pool(), issue_id)
        .await
        .context("read reactions")?;
    if tallies.is_empty() {
        println!("no reactions");
        return Ok(());
    }
    let line = tallies
        .iter()
        .map(|t| format!("{} {}", t.emoji, t.count))
        .collect::<Vec<_>>()
        .join("  ");
    println!("{line}");
    Ok(())
}

/// `hangar issue link add|remove|list`: author and read an issue's TYPED links
/// (multica parity #20).
///
/// Routes through the SAME [`CardDependencyRepo`] seam the daemon RPCs use, so
/// there is exactly one mutator: a `blocks` link normalises into the reverse
/// `blocked_by` row, a `related` link is symmetric and never gates, and a
/// self-link / cycle / cross-tenant endpoint is a NON-ZERO exit rather than a
/// silent no-op.
///
/// [`CardDependencyRepo`]: ainb_hangar_store::repo::card_dependency::CardDependencyRepo
async fn run_issue_link(store: &Store, cmd: IssueLinkCommand) -> Result<()> {
    use ainb_hangar_store::repo::card_dependency::{CardDependencyError, CardDependencyRepo};

    let (id, workspace) = match &cmd {
        IssueLinkCommand::Add(a) | IssueLinkCommand::Remove(a) => {
            (a.id.clone(), a.workspace.clone())
        }
        IssueLinkCommand::List(a) => (a.id.clone(), a.workspace.clone()),
    };
    let workspace_id = resolve_skills_workspace(store, workspace.as_deref()).await?;

    match &cmd {
        IssueLinkCommand::Add(args) => {
            let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
            let ws = ainb_hangar_core::ids::WorkspaceId::from_str(workspace_id.clone())
                .map_err(|e| anyhow::anyhow!("bad workspace id: {e}"))?;
            CardDependencyRepo::add_link(
                store.pool(),
                &ws,
                &args.id,
                &args.other,
                args.kind.to_kind(),
                now,
            )
            .await
            .map_err(|e| match e {
                CardDependencyError::SelfDependency => {
                    anyhow::anyhow!("a card cannot link to itself")
                }
                CardDependencyError::Cycle => {
                    anyhow::anyhow!("that link would create a dependency cycle")
                }
                CardDependencyError::NotFound => anyhow::anyhow!(
                    "both issues must exist in this workspace ({} / {})",
                    args.id,
                    args.other
                ),
                CardDependencyError::Db(db) => anyhow::Error::new(db).context("add issue link"),
            })?;
        }
        IssueLinkCommand::Remove(args) => {
            let ws = ainb_hangar_core::ids::WorkspaceId::from_str(workspace_id.clone())
                .map_err(|e| anyhow::anyhow!("bad workspace id: {e}"))?;
            CardDependencyRepo::remove_link(
                store.pool(),
                &ws,
                &args.id,
                &args.other,
                args.kind.to_kind(),
            )
            .await
            .context("remove issue link")?;
        }
        IssueLinkCommand::List(_) => {}
    }

    print_issue_links(store, &workspace_id, &id).await
}

/// Print one issue's typed links, one row per link:
/// `<glyph>  <kind>  <display-id>  <title>`. `🔒` marks a blocker that is still
/// UNFINISHED (so it gates), `✓` one that has finished, `→` what this issue
/// blocks, and `~` a related issue. No links ⇒ `no links`.
async fn print_issue_links(store: &Store, workspace_id: &str, issue_id: &str) -> Result<()> {
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;

    let blockers = CardDependencyRepo::blockers_of(store.pool(), issue_id)
        .await
        .context("read blockers")?;
    let unfinished = CardDependencyRepo::unfinished_blockers_of(store.pool(), issue_id)
        .await
        .context("read unfinished blockers")?;
    let blocks = CardDependencyRepo::blocks_of(store.pool(), issue_id)
        .await
        .context("read blocked cards")?;
    let related = CardDependencyRepo::related_of(store.pool(), issue_id)
        .await
        .context("read related cards")?;

    if blockers.is_empty() && blocks.is_empty() && related.is_empty() {
        println!("no links");
        return Ok(());
    }

    for (kind, ids) in [
        ("blocked-by", &blockers),
        ("blocks", &blocks),
        ("related", &related),
    ] {
        for other in ids {
            let glyph = match kind {
                "blocked-by" if unfinished.iter().any(|u| u == other) => "🔒",
                "blocked-by" => "✓",
                "blocks" => "→",
                _ => "~",
            };
            let row =
                IssueRepo::get_by_id(store.pool(), other).await.context("read linked issue")?;
            let title = row.as_ref().map_or_else(String::new, |r| r.title.clone());
            let reference = match &row {
                Some(_) => issue_display_ref(store, workspace_id, other).await?,
                None => other.clone(),
            };
            println!("{glyph}  {kind:<10}  {reference:<10}  {title}");
        }
    }
    Ok(())
}

/// The human display id (`HGR-<n>`) of an issue, falling back to its raw id.
async fn issue_display_ref(store: &Store, workspace_id: &str, issue_id: &str) -> Result<String> {
    let seq = IssueRepo::workspace_seq(store.pool(), workspace_id, issue_id)
        .await
        .context("read issue ordinal")?;
    Ok(seq.map_or_else(
        || issue_id.to_string(),
        |n| ainb_hangar_store::repo::workspace::issue_display_id(None, n),
    ))
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
/// `hangar issue batch-state --state <S> <ID>…` — apply ONE lifecycle state to
/// several issues, then run the child-done cascade ONCE over the whole batch
/// (multica parity #3-rest, MUL-4155).
///
/// The verb exists for the cascade. Completing two siblings of the same stage
/// one-at-a-time through `issue update` used to post a roll-up comment on the
/// parent EACH TIME; here the whole batch closes the barrier once and posts a
/// SINGLE comment naming every child that closed it. Store-direct like
/// `issue update` — no daemon needed, so there is no agent wake; the comment is
/// the observable side.
async fn run_issue_batch_state(store: &Store, args: IssueBatchStateArgs) -> Result<()> {
    use ainb_hangar_store::repo::issue::IssueRepo;
    use ainb_hangar_store::service::child_done::{ChildTransition, cascade_children_done};

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;

    // 0049: reject a non-canonical state BEFORE any write — a typo must never
    // half-apply a batch.
    if ainb_hangar_proto::lifecycle::IssueLifecycle::parse_canonical(&args.state).is_none() {
        anyhow::bail!(
            "invalid --state {:?}; valid values: {}",
            args.state,
            ainb_hangar_proto::lifecycle::IssueLifecycle::canonical_list()
        );
    }

    // Dedupe preserving caller order — a repeated id must not be counted twice.
    let mut ids: Vec<String> = Vec::new();
    for id in &args.ids {
        if !ids.iter().any(|seen| seen == id) {
            ids.push(id.clone());
        }
    }

    let changed = IssueRepo::set_state_batch(store.pool(), &workspace_id, &ids, &args.state)
        .await
        .context("apply batch state")?;
    println!(
        "updated {} of {} issue(s) to {}",
        changed.len(),
        ids.len(),
        args.state
    );

    let transitions: Vec<ChildTransition> = changed
        .iter()
        .map(|(id, prev)| ChildTransition {
            child_id: id.clone(),
            prev_state: prev.clone(),
            new_state: args.state.clone(),
        })
        .collect();
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
    let cascades =
        cascade_children_done(store.pool(), &workspace_id, &transitions, now, &SystemIdGen)
            .await
            .context("child-done cascade")?;
    for c in &cascades {
        println!(
            "posted sub-issue roll-up on parent {} ({}/{}) covering {} sub-issues",
            c.parent_id,
            c.children_done,
            c.children_total,
            c.children.len()
        );
    }
    Ok(())
}

async fn run_issue_update(store: &Store, args: IssueUpdateArgs) -> Result<()> {
    use ainb_hangar_store::repo::issue::IssueFieldUpdate;

    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;

    // 0049: reject a state outside the canonical lifecycle vocabulary BEFORE any
    // write, so a typo is a clear CLI error naming the seven valid tokens rather
    // than a migration-0049 trigger ABORT surfacing as an opaque sqlx failure.
    if let Some(state) = args.state.as_deref() {
        if ainb_hangar_proto::lifecycle::IssueLifecycle::parse_canonical(state).is_none() {
            anyhow::bail!(
                "invalid --state {state:?}; valid values: {}",
                ainb_hangar_proto::lifecycle::IssueLifecycle::canonical_list()
            );
        }
    }

    // Map the present flags onto the partial edit. The two nullable fields use
    // the clear-flag to distinguish "clear to none" from "leave unchanged". The
    // assign token is polymorphic: `member:<id>`/`agent:<id>`, or a bare id
    // (back-compat: a bare id is an agent).
    let assignee = if args.unassign {
        Some(None)
    } else {
        args.assign.as_deref().map(parse_assignee).transpose()?.map(Some)
    };
    let due_date = if args.clear_due {
        Some(None)
    } else {
        args.due.map(Some)
    };
    let update = IssueFieldUpdate {
        // Title editing is a board card-edit affordance (F6, TUI); the CLI
        // `issue update` keeps its existing state/assignee/priority/due surface.
        title: None,
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

    // 0046: capture the pre-update state so a `--state done/cancelled` edit that
    // completes a sub-issue can fire the child-done → parent cascade below. Read
    // only when a state edit is requested (the cascade only fires on a transition).
    let prev_state: Option<String> = if update.state.is_some() {
        IssueRepo::get_by_id(store.pool(), &args.id)
            .await
            .with_context(|| format!("read issue {} before update", args.id))?
            .filter(|i| i.workspace_id == workspace_id)
            .map(|i| i.state)
    } else {
        None
    };

    // multica parity #13: the FULL pre-edit row, so the post-edit diff can write
    // one activity row per changed field. Deliberately separate from
    // `prev_state` above, which only carries the state token for the cascade.
    let before_issue = IssueRepo::get_by_id(store.pool(), &args.id)
        .await
        .with_context(|| format!("read issue {} before update", args.id))?
        .filter(|i| i.workspace_id == workspace_id);

    let touched = IssueRepo::update_fields(store.pool(), &workspace_id, &args.id, &update)
        .await
        .with_context(|| format!("update issue {}", args.id))?;
    if !touched {
        anyhow::bail!("no issue with id {} in this workspace", args.id);
    }
    println!("updated issue {}", args.id);

    // multica parity #13: diff the pre-edit row against the committed one and
    // record one activity row per changed field. The CLI shares the daemon's
    // diff service so the two writers cannot drift. Best-effort throughout.
    if let Some(before) = before_issue.as_ref() {
        if let Ok(Some(after)) = IssueRepo::get_by_id(store.pool(), &args.id).await {
            let owner = ainb_hangar_store::bootstrap::default_owner_id(store.pool())
                .await
                .ok()
                .flatten();
            let actor =
                ainb_hangar_core::activity::ActivityActor::member_or_system(owner.as_deref());
            ainb_hangar_store::service::activity::ActivityService::record_issue_diff(
                store.pool(),
                &SystemIdGen,
                &SystemClock,
                &workspace_id,
                &actor,
                before,
                &after,
            )
            .await;
        }
    }

    // 0046: a CLI-driven completion also posts the parent roll-up comment, so
    // CLI and TUI behaviour stay aligned (the CLI has no daemon, so there is no
    // agent wake — the comment is the observable side). Best-effort: a cascade
    // fault must not fail an already-committed state edit.
    if let (Some(prev), Some(new_state)) = (prev_state.as_deref(), update.state.as_deref()) {
        let idgen = SystemIdGen;
        let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
        match ainb_hangar_store::service::child_done::cascade_child_done(
            store.pool(),
            &workspace_id,
            &args.id,
            prev,
            new_state,
            now,
            idgen.new_ulid(),
        )
        .await
        {
            Ok(Some(c)) => println!(
                "posted sub-issue roll-up on parent {} ({}/{})",
                c.parent_id, c.children_done, c.children_total
            ),
            Ok(None) => {}
            Err(e) => eprintln!("warning: child-done cascade skipped: {e}"),
        }
    }

    // In-product recovery from a dead end: a post-creation assignment that names an
    // AGENT re-dispatches the issue, mirroring the create-time enqueue. Without
    // this an issue stuck in `agent_error` (terminal, non-retryable) had no
    // in-product path back to work short of filing a brand-new issue. The task
    // reads the issue card's persisted repo/branch/agent, and the one-active-run
    // guard means a re-assign while a run is in flight never double-dispatches.
    // Only an AGENT assignee dispatches a run — a member assignee just commits
    // (agents-are-team-members symmetry: same column, but a human does no work).
    if let Some(raw) = args.assign.as_deref() {
        let assignee = parse_assignee(raw)?;
        if assignee.kind() == ActorKind::Agent {
            if let Some(task_id) =
                enqueue_assigned_task(store.pool(), &workspace_id, &args.id, assignee.id()).await?
            {
                println!("queued task {task_id}");
            }
        }
    }
    Ok(())
}

/// Parse a CLI `--assign` token into a polymorphic [`ActorRef`].
///
/// A token containing `:` is parsed as the canonical `member:<id>`/`agent:<id>`
/// form. A bare token (no `:`) stays an **agent** for back-compat, so every
/// existing script and tripwire that passes a raw agent id is byte-unchanged.
fn parse_assignee(raw: &str) -> Result<ActorRef> {
    if raw.contains(':') {
        raw.parse::<ActorRef>().with_context(|| {
            format!("invalid assignee `{raw}` (expected member:<id> or agent:<id>)")
        })
    } else {
        ActorRef::new(ActorKind::Agent, raw).context("assignee agent id was empty")
    }
}

/// Enqueue one `queued` task for `agent_id` on `issue_id`, mirroring the daemon's
/// [`run_card`] single-agent launch: read the issue card's persisted repo (the
/// path dispatch provisions from), source branch, and provider, then insert the
/// task keyed to the agent's runtime in one transaction.
///
/// Best-effort recovery: returns `Ok(None)` (no task, the assignee edit still
/// stands) when the issue already has a live run — the one-active-run guard, so a
/// re-assign never double-dispatches — or when the card carries no repo (nothing
/// for dispatch to provision). Returns the new task id when a task was enqueued.
///
/// [`run_card`]: ainb_hangar_daemon::rpc::run_card
async fn enqueue_assigned_task(
    pool: &sqlx::SqlitePool,
    workspace_id: &str,
    issue_id: &str,
    agent_id: &str,
) -> Result<Option<String>> {
    use ainb_hangar_core::dispatch_reason::DispatchReason;
    use ainb_hangar_store::repo::card_parity::CardParityRepo;
    use ainb_hangar_store::repo::task::NewTask;

    // One active run per card: an in-flight (queued/dispatched/running) task keeps
    // the issue; recovery only re-dispatches once the prior run is terminal.
    if let Some(active) = TaskRepo::active_task_for_issue(pool, workspace_id, issue_id)
        .await
        .context("check for an active run before re-dispatch")?
    {
        // multica parity #12: this used to be a bare `Ok(None)` — no record, no
        // message, no way for the user to learn why assigning an agent did not
        // start anything. Record it.
        record_cli_dispatch_attempt(
            pool,
            workspace_id,
            issue_id,
            Some(agent_id),
            None,
            DispatchReason::AlreadyActive,
            Some(&format!("a run is already active ({})", active.status)),
        )
        .await;
        return Ok(None);
    }

    // The runtime the task must key to (resolved from the agent, not supplied) —
    // rejects an unknown / foreign-tenant agent id before any write.
    let assignment = resolve_agent_runtime(pool, workspace_id, agent_id).await?;

    // The card's persisted repo is what dispatch provisions from. No repo → nothing
    // to run: leave the assignee committed without a task rather than enqueue a row
    // that would run a useless in-tree prompt.
    let (card_repo, card_agent) = CardParityRepo::get_issue_repo_agent(pool, issue_id)
        .await
        .context("read issue repo/agent for re-dispatch")?
        .unwrap_or((None, None));
    let Some(repo_ref) = card_repo else {
        // multica parity #12: the other silent `Ok(None)`.
        record_cli_dispatch_attempt(
            pool,
            workspace_id,
            issue_id,
            Some(agent_id),
            None,
            DispatchReason::TargetUnavailable,
            Some("no repo pinned on this card"),
        )
        .await;
        return Ok(None);
    };
    let source_branch = CardParityRepo::get_issue_branches(pool, issue_id)
        .await
        .context("read issue source branch for re-dispatch")?
        .and_then(|(source, _target)| source);

    // The task's agent_kind mirrors the assignee's provider (a codex agent's task
    // must not read back as the claude column default), preferring the card's
    // persisted agent when set.
    let provider: Option<Option<String>> =
        sqlx::query_scalar("SELECT provider FROM agent WHERE id = ?")
            .bind(&assignment.agent_id)
            .fetch_optional(pool)
            .await
            .context("read agent provider for re-dispatch")?;
    let agent_kind = card_agent
        .or_else(|| {
            provider
                .flatten()
                .as_deref()
                .and_then(ainb_hangar_core::agent_kind::AgentKind::parse)
        })
        .unwrap_or(ainb_hangar_core::agent_kind::AgentKind::DEFAULT);

    // Scope the task to the issue's NEXT run generation (migration 0039), matching
    // the create + daemon launch seams so a prior run's terminal rows never poison
    // this recovery run.
    let generation = TaskRepo::next_generation_for_issue(pool, issue_id)
        .await
        .context("compute run generation for re-dispatched task")?;
    let task_id = SystemIdGen.new_ulid();
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock);
    let mut tx = pool.begin().await.context("begin re-dispatch tx")?;
    TaskRepo::insert_in_tx(
        &mut tx,
        &NewTask {
            id: task_id.clone(),
            workspace_id: workspace_id.to_string(),
            runtime_id: assignment.runtime_id,
            agent_id: assignment.agent_id,
            issue_id: Some(issue_id.to_string()),
            work_dir: None,
            priority: 0,
            created_at: now,
            autopilot_run_id: None,
            generation,
        },
    )
    .await
    .context("enqueue re-dispatched task for assigned agent")?;
    CardParityRepo::set_task_repo_agent_in_tx(&mut tx, &task_id, Some(&repo_ref), agent_kind)
        .await
        .context("persist re-dispatched task repo")?;
    CardParityRepo::set_task_source_branch_in_tx(&mut tx, &task_id, source_branch.as_deref())
        .await
        .context("persist re-dispatched task source branch")?;
    tx.commit().await.context("commit re-dispatch tx")?;
    record_cli_dispatch_attempt(
        pool,
        workspace_id,
        issue_id,
        Some(agent_id),
        Some(&task_id),
        DispatchReason::Queued,
        Some(&format!("task {task_id}")),
    )
    .await;
    Ok(Some(task_id))
}

/// Record one `dispatch_attempt` row from a CLI path (multica parity #12).
///
/// The CLI talks to the store DIRECTLY (no daemon), so it cannot ride the
/// daemon's `run_card` recording seam — this is its equivalent. Best-effort by
/// the same contract: a record fault warns and never changes the caller's
/// outcome, because an audit write must not be able to break an assignment that
/// otherwise succeeded.
///
/// `source` is always [`DispatchSource::Assign`]: every CLI producer today is the
/// assignee re-dispatch.
async fn record_cli_dispatch_attempt(
    pool: &sqlx::SqlitePool,
    workspace_id: &str,
    issue_id: &str,
    agent_id: Option<&str>,
    task_id: Option<&str>,
    reason: ainb_hangar_core::dispatch_reason::DispatchReason,
    detail: Option<&str>,
) {
    use ainb_hangar_store::repo::dispatch_attempt::{DispatchAttemptRepo, NewDispatchAttempt};

    let record = NewDispatchAttempt {
        workspace_id,
        issue_id: Some(issue_id),
        agent_id,
        runtime_id: None,
        task_id,
        reason,
        detail,
        source: ainb_hangar_core::dispatch_reason::DispatchSource::Assign,
        created_at: ainb_hangar_core::clock::HangarClock::now_ms(&SystemClock),
    };
    if let Err(e) = DispatchAttemptRepo::record(pool, &SystemIdGen.new_ulid(), &record).await {
        tracing::warn!(
            error = %e,
            issue_id,
            "dispatch attempt record failed (audit only; the assignment is unchanged)"
        );
    }
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
    // A second handle for the parity-#13 `created` activity row: `creator` is
    // moved into `NewIssue` below, and the audit write happens after the insert.
    let activity_creator = creator.clone();

    // Resolve the assignee (if any). The token is polymorphic: an AGENT
    // (`agent:<id>` or a bare id) must exist in the workspace and its runtime is
    // the queue the task lands on — resolved BEFORE the issue insert so a bad
    // agent id fails before any write. A MEMBER (`member:<id>`) is stored as-is
    // and enqueues NO task (agents-are-team-members symmetry: a human does no
    // work).
    let parsed_assignee = args.assign.as_deref().map(parse_assignee).transpose()?;
    let (assignment, member_assignee) = match parsed_assignee {
        Some(actor) if actor.kind() == ActorKind::Agent => (
            Some(resolve_agent_runtime(pool, &workspace_id, actor.id()).await?),
            None,
        ),
        Some(actor) => (None, Some(actor)),
        None => (None, None),
    };
    let now = ainb_hangar_core::clock::HangarClock::now_ms(&clock);

    // 0046: an optional parent makes this a sub-issue. Validate it resolves in the
    // same workspace BEFORE the insert (mirrors the assignee-resolve contract) — a
    // foreign / unknown parent is a hard error, never a silent cross-tenant link.
    let parent_issue_id = args.parent.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(parent) = parent_issue_id {
        let ok = IssueRepo::get_by_id(pool, parent)
            .await
            .context("resolve parent issue")?
            .is_some_and(|p| p.workspace_id == workspace_id);
        anyhow::ensure!(ok, "parent issue `{parent}` not found in this workspace");
    }

    // 0056: resolve the ORIGIN PROVENANCE BEFORE any write (like the assignee and
    // parent resolves above) — a bad origin must fail the command before the
    // insert, never leave a half-provenanced issue behind.
    let origin = resolve_cli_origin(
        args.origin_type.as_deref(),
        args.origin_id.as_deref(),
        std::env::var("HANGAR_ORIGIN_TYPE").ok().as_deref(),
        std::env::var("HANGAR_ORIGIN_ID").ok().as_deref(),
    )?;

    // e38.21: apply the workspace's issue_prefix to the new title so the prefix
    // actually takes effect on a created issue. An unconfigured workspace leaves
    // the title verbatim (the v1 behaviour). Read after the assignee resolve so a
    // bad agent id still fails first.
    let issue_prefix = workspace_issue_prefix(pool, &workspace_id).await?;
    let title = ainb_hangar_store::repo::workspace::apply_issue_prefix(
        issue_prefix.as_deref(),
        &args.title,
    );

    let new = NewIssue {
        id: id.clone(),
        workspace_id: workspace_id.clone(),
        title,
        description: args.description,
        state: args.state,
        assignee: assignment
            .as_ref()
            .map(|a| ActorRef::new(ActorKind::Agent, &a.agent_id).expect("agent id non-empty"))
            .or_else(|| member_assignee.clone()),
        creator,
        created_at: now,
        priority: args.priority,
        due_date: args.due,
        // 0016: labels go through the `label` / `issue_label` join below (the
        // source of truth), never straight into this JSON read-cache — writing
        // the cache alone left `hangar issue create --label` invisible to every
        // label query and diverged the CLI from the daemon's create.
        labels: Vec::new(),
        // 0048: trim-drop blank elements — an empty criterion / ref is not data.
        // #11-rest: mint a stable per-criterion id at create so an agent can tick
        // one off by id (the constructor does the trim-drop).
        acceptance_criteria: args
            .acceptance_criteria
            .iter()
            .filter_map(|s| AcceptanceCriterion::new(&idgen, s))
            .collect(),
        context_refs: args
            .context_refs
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect(),
        parent_issue_id: parent_issue_id.map(ToString::to_string),
        // #3-rest: the authored stage barrier (clap range-checks >= 1). Only
        // meaningful with a parent; without one it is stored and inert.
        stage: args.stage,
    };
    IssueRepo::insert(pool, &new).await.context("insert issue")?;
    // multica parity #13: open the card's narrative. Best-effort — an audit
    // failure never fails the create.
    ainb_hangar_store::service::activity::ActivityService::record(
        pool,
        &idgen,
        &clock,
        &workspace_id,
        &id,
        &ainb_hangar_core::activity::ActivityActor::Actor(activity_creator),
        ainb_hangar_core::activity::ActivityAction::Created,
        serde_json::json!({}),
    )
    .await;
    // 0056: stamp the resolved provenance post-insert, the same pattern the
    // daemon's create uses, so a CLI-created and a TUI-created issue read back
    // identically.
    IssueRepo::set_origin(pool, &workspace_id, &id, &origin)
        .await
        .context("stamp issue origin")?;

    // 0016: attach each `--label` through the join — `LabelRepo::attach` resolves
    // or creates the label in the workspace, writes the `issue_label` row
    // (ON CONFLICT DO NOTHING, so a repeated flag is one row) and re-derives the
    // `issue.labels` cache from the join. Same path the daemon's create takes, so
    // a CLI-created and a TUI-created issue read back identically.
    if !args.labels.is_empty() {
        use ainb_hangar_store::repo::label::LabelRepo;
        let ws = ainb_hangar_core::ids::WorkspaceId::from_str(workspace_id.clone())
            .context("workspace id")?;
        for name in args.labels.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            LabelRepo::attach(pool, &ws, &id, name, None)
                .await
                .with_context(|| format!("attach label `{name}`"))?;
        }
    }

    // Resolve + persist the repo / branches (0032/0042). A remote repo token is
    // cloned once into the shared clone cache (the board card-create parity),
    // and the LOCAL path is what dispatch provisions from.
    let repo_ref = match args.repo.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => Some(resolve_cli_repo_ref(raw).await?),
        None => None,
    };
    use ainb_hangar_store::repo::card_parity::CardParityRepo;
    if repo_ref.is_some() {
        CardParityRepo::set_issue_repo_agent(pool, &workspace_id, &id, repo_ref.as_deref(), None)
            .await
            .context("persist issue repo")?;
    }
    if args.source_branch.is_some() || args.target_branch.is_some() {
        CardParityRepo::set_issue_branches(
            pool,
            &workspace_id,
            &id,
            args.source_branch.as_deref(),
            args.target_branch.as_deref(),
        )
        .await
        .context("persist issue branches")?;
    }

    // When assigned, enqueue a task for the agent's runtime so the daemon claims
    // + dispatches it (materialising the agent's skills first). One transaction
    // covers the insert + its dispatch inputs (repo / source branch), so the
    // claim loop can never observe a half-written task.
    if let Some(a) = assignment {
        let task_id = idgen.new_ulid();
        // Scope the task to the issue's next run generation (migration 0039). A
        // freshly created issue has no prior tasks, so this is 0; computing it
        // keeps this path consistent with the daemon's assign seam.
        let generation = TaskRepo::next_generation_for_issue(pool, &id)
            .await
            .context("compute run generation for assigned task")?;
        // The task's agent_kind mirrors the assignee agent's provider (so a
        // codex agent's task doesn't read back as the claude column default).
        let provider: Option<Option<String>> =
            sqlx::query_scalar("SELECT provider FROM agent WHERE id = ?")
                .bind(&a.agent_id)
                .fetch_optional(pool)
                .await
                .context("read agent provider")?;
        let agent_kind = provider
            .flatten()
            .as_deref()
            .and_then(ainb_hangar_core::agent_kind::AgentKind::parse)
            .unwrap_or(ainb_hangar_core::agent_kind::AgentKind::DEFAULT);
        let mut tx = pool.begin().await.context("begin enqueue tx")?;
        TaskRepo::insert_in_tx(
            &mut tx,
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
                generation,
            },
        )
        .await
        .context("enqueue task for assigned agent")?;
        if repo_ref.is_some() {
            CardParityRepo::set_task_repo_agent_in_tx(
                &mut tx,
                &task_id,
                repo_ref.as_deref(),
                agent_kind,
            )
            .await
            .context("persist task repo")?;
        }
        if args.source_branch.is_some() {
            CardParityRepo::set_task_source_branch_in_tx(
                &mut tx,
                &task_id,
                args.source_branch.as_deref(),
            )
            .await
            .context("persist task source branch")?;
        }
        tx.commit().await.context("commit enqueue tx")?;
        println!("created issue {id}");
        println!("queued task {task_id}");
    } else {
        println!("created issue {id}");
    }
    Ok(())
}

/// Resolve a CLI `--repo` token to the local path dispatch provisions from:
/// `scratch` and absolute paths pass through; anything else is treated as a
/// REMOTE and cloned once into the shared clone cache (blocking git work off
/// the async thread), mirroring the daemon's card-create resolve.
async fn resolve_cli_repo_ref(raw: &str) -> Result<String> {
    if raw == "scratch" || std::path::Path::new(raw).is_absolute() {
        return Ok(raw.to_string());
    }
    let ainb_dir = ainb_hangar_core::hangar_home().context("resolve hangar home")?;
    let remote = raw.to_string();
    let cloned = tokio::task::spawn_blocking(move || {
        ainb_fleet_core::repo_clone::ensure_clone(&ainb_dir, &remote)
    })
    .await
    .context("join clone task")?
    .with_context(|| format!("clone remote repo `{raw}`"))?;
    Ok(cloned.display().to_string())
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

/// `hangar issue search`: ranked title + description + comment substring search,
/// workspace-scoped.
///
/// Mirrors the `hangar/issues_search` daemon RPC over the CLI: a row matches when
/// the case-insensitive `query` substring appears in the issue title,
/// description, OR any comment body, ranked title > description > comment. A read
/// only — when no `--workspace` is given and no workspace exists yet, the result
/// is empty (no bootstrap side effect, unlike the create/update verbs). Results
/// are rendered in rank order through the same `render_issue_list` formatter the
/// `list` verb uses, so every output format is consistent.
async fn run_issue_search(
    store: &Store,
    args: IssueSearchArgs,
    format: OutputFormat,
) -> Result<()> {
    let pool = store.pool();
    // Read-only workspace resolution: an explicit `--workspace` resolves by slug
    // (a typo is an error); without it, fall back to the default workspace if one
    // exists. Searching must never bootstrap a workspace, so the no-workspace case
    // is an empty result, not a side effect.
    let workspace_id = match args.workspace.as_deref() {
        Some(slug) => {
            let id: Option<String> = sqlx::query_scalar("SELECT id FROM workspace WHERE slug = ?")
                .bind(slug)
                .fetch_optional(pool)
                .await
                .context("look up workspace by slug")?;
            Some(id.with_context(|| format!("no workspace with slug `{slug}`"))?)
        }
        None => find_default_workspace(store).await?,
    };
    let Some(workspace_id) = workspace_id else {
        // No workspace yet -> no issues. Empty result, not an error.
        render_issue_list(&[], format);
        return Ok(());
    };
    let issues = IssueRepo::search_ranked(pool, &workspace_id, &args.query)
        .await
        .context("search issues")?;
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
        OutputFormat::Text => {
            println!("{}", issue_line(&issue));
            // 0056 / multica parity #21: the provenance, so the "an issue created
            // by autopilot or by a comment mention records its origin" proof needs
            // no daemon and no TUI. Omitted entirely for a pre-0056 row, whose
            // provenance is genuinely unknown.
            if let Some(origin) = issue.origin.as_ref() {
                match origin.id() {
                    Some(id) => println!("Origin: {} ({id})", origin.kind_db_str()),
                    None => println!("Origin: {}", origin.kind_db_str()),
                }
            }
            // #11-rest: the definition-of-done, with its per-criterion ☑/☐ state,
            // so the CLI proof does not depend on the TUI.
            if !issue.acceptance_criteria.is_empty() {
                println!(
                    "Acceptance: {}/{}",
                    ainb_hangar_core::acceptance::checked_count(&issue.acceptance_criteria),
                    issue.acceptance_criteria.len()
                );
                for (idx, criterion) in issue.acceptance_criteria.iter().enumerate() {
                    println!("  {}", criterion_line(idx + 1, criterion));
                }
            }
            // multica parity #20: the issue's typed links, so the CLI proof of a
            // blocked_by / blocks / related relation needs no daemon and no TUI.
            // Silent when the issue has none (`print_issue_links` prints
            // `no links`, which we suppress here by checking first).
            if issue_has_links(store, &issue.id).await? {
                println!("Links:");
                print_issue_links(store, &issue.workspace_id, &issue.id).await?;
            }
            // multica parity #22: who watches this issue and how it was reacted
            // to — the daemon-free, TUI-free acceptance read. Silent when there
            // are none, so existing output is unchanged.
            {
                use ainb_hangar_store::repo::issue_reaction::IssueReactionRepo;
                use ainb_hangar_store::repo::issue_subscriber::IssueSubscriberRepo;

                let subs = IssueSubscriberRepo::list(store.pool(), &issue.id)
                    .await
                    .context("read subscribers")?;
                if !subs.is_empty() {
                    println!("Subscribers: {}", subs.len());
                    for s in &subs {
                        println!("  {}  ({})", s.actor, s.reason_raw);
                    }
                }
                let tallies = IssueReactionRepo::tallies(store.pool(), &issue.id)
                    .await
                    .context("read reactions")?;
                if !tallies.is_empty() {
                    let line = tallies
                        .iter()
                        .map(|t| format!("{} {}", t.emoji, t.count))
                        .collect::<Vec<_>>()
                        .join("  ");
                    println!("Reactions: {line}");
                }
            }
            // multica parity #17: this issue's RESOLVED custom properties and
            // its agent metadata scratch bag, rendered through the SAME
            // `render_value` / `render_metadata` the wire uses. Both silent
            // when empty, so existing output is unchanged.
            {
                use ainb_hangar_core::ids::WorkspaceId;
                use ainb_hangar_core::properties::{render_metadata, render_value};
                use ainb_hangar_store::repo::issue_property::IssuePropertyRepo;

                let ws = WorkspaceId::from_str(issue.workspace_id.clone())
                    .context("issue carries an empty workspace id")?;
                let resolved = IssuePropertyRepo::values_for(store.pool(), &ws, &issue.id)
                    .await
                    .context("resolve custom properties")?;
                if !resolved.is_empty() {
                    println!("Properties:");
                    for (def, value) in &resolved {
                        println!("  {}: {}", def.name, render_value(value));
                    }
                }
                if !issue.metadata.is_empty() {
                    println!("Metadata:");
                    for (key, value) in &issue.metadata {
                        println!("  {key} = {}", render_metadata(value));
                    }
                }
            }
            // multica parity #12: WHY this card is not running, when its newest
            // admission decision was a decline. Silent on a healthy card, so
            // existing output is unchanged. `issue why` shows the full history.
            if let Some((code, detail)) = latest_dispatch_decline(store, &issue.id).await? {
                match detail {
                    Some(d) => println!("Not dispatched: {code} — {d}"),
                    None => println!("Not dispatched: {code}"),
                }
            }
        }
    }
    Ok(())
}

/// `hangar issue why <id>` (multica parity #12): the card's ADMISSION history,
/// newest first — the persisted answer to "why is this not running".
///
/// Reads the store directly (like `issue criteria` / `issue link`), so it needs
/// no daemon: the whole dispatch-reason proof is runnable against nothing but
/// sqlite. Honours the shared `--format json`, which emits the
/// `DispatchAttemptRow` wire shape verbatim.
async fn run_issue_why(store: &Store, args: IssueWhyArgs, format: OutputFormat) -> Result<()> {
    use ainb_hangar_store::repo::dispatch_attempt::DispatchAttemptRepo;

    // Resolve the workspace so a typo'd `--workspace` is an error, not a silently
    // empty list; the attempts themselves are keyed on the issue id.
    let _workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let attempts = DispatchAttemptRepo::list_for_issue(store.pool(), &args.id, args.limit.max(1))
        .await
        .context("read dispatch attempts")?;

    match format {
        OutputFormat::Json => {
            let rows: Vec<serde_json::Value> = attempts.iter().map(dispatch_attempt_json).collect();
            println!(
                "{}",
                serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())
            );
        }
        OutputFormat::Csv => {
            println!("id,reason,detail,source,task_id,created_at");
            for a in &attempts {
                println!(
                    "{},{},{},{},{},{}",
                    a.id,
                    a.reason,
                    csv_field(a.detail.as_deref().unwrap_or_default()),
                    a.source,
                    a.task_id.as_deref().unwrap_or_default(),
                    a.created_at
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| reason | detail | source | when |");
            println!("|---|---|---|---|");
            for a in &attempts {
                println!(
                    "| {} | {} | {} | {} |",
                    a.reason,
                    a.detail.as_deref().unwrap_or("—"),
                    a.source,
                    fmt_epoch_ms_utc(a.created_at)
                );
            }
        }
        OutputFormat::Text => {
            if attempts.is_empty() {
                println!("no dispatch attempts recorded for {}", args.id);
            } else {
                println!("{:<22} {:<8} {:<22} detail", "reason", "source", "when");
                for a in &attempts {
                    println!(
                        "{:<22} {:<8} {:<22} {}",
                        a.reason,
                        a.source,
                        fmt_epoch_ms_utc(a.created_at),
                        a.detail.as_deref().unwrap_or("—")
                    );
                }
            }
        }
    }
    Ok(())
}

/// `hangar issue timeline <id>` (multica parity #13): the card's NARRATIVE,
/// oldest first — creation, state moves, re-assignments, priority/title/due-date
/// edits, task outcomes, and the comments merged in by timestamp.
///
/// Reads the store directly (like `issue why`), so the whole activity-log proof
/// is runnable against nothing but sqlite with no daemon. `--format json` emits
/// the `TimelineEntryRow` wire shape verbatim, so the CLI and
/// `hangar/issue_timeline` agree.
async fn run_issue_timeline(
    store: &Store,
    args: IssueTimelineArgs,
    format: OutputFormat,
) -> Result<()> {
    // Resolve the workspace so a typo'd `--workspace` is an error, not a silently
    // empty timeline.
    let workspace_id = resolve_skills_workspace(store, args.workspace.as_deref()).await?;
    let entries = read_issue_timeline(store, &workspace_id, &args.id, args.limit.max(1)).await?;

    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
            );
        }
        OutputFormat::Csv => {
            println!("kind,actor,action,detail,created_at");
            for e in &entries {
                println!(
                    "{},{},{},{},{}",
                    e.kind,
                    timeline_actor(e),
                    e.action.as_deref().unwrap_or_default(),
                    csv_field(&timeline_detail(e)),
                    e.created_at
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| when | who | what | detail |");
            println!("|---|---|---|---|");
            for e in &entries {
                println!(
                    "| {} | {} | {} | {} |",
                    fmt_epoch_ms_utc(e.created_at),
                    timeline_actor(e),
                    e.action.as_deref().unwrap_or("comment"),
                    timeline_detail(e)
                );
            }
        }
        OutputFormat::Text => {
            if entries.is_empty() {
                println!("no activity recorded for {}", args.id);
            } else {
                for e in &entries {
                    println!(
                        "{}  {:<18} {:<17} {}",
                        fmt_epoch_ms_utc(e.created_at),
                        timeline_actor(e),
                        e.action.as_deref().unwrap_or("comment"),
                        timeline_detail(e)
                    );
                }
            }
        }
    }
    Ok(())
}

/// Merge the card's activity rows with its comments into the wire
/// [`TimelineEntryRow`] shape, oldest first — the store-side twin of the
/// daemon's `snapshots::issue_timeline` (parity #13).
async fn read_issue_timeline(
    store: &Store,
    workspace_id: &str,
    issue_id: &str,
    limit: i64,
) -> Result<Vec<ainb_hangar_proto::snapshots::TimelineEntryRow>> {
    use ainb_hangar_proto::snapshots::{
        TIMELINE_KIND_ACTIVITY, TIMELINE_KIND_COMMENT, TimelineEntryRow,
    };
    use ainb_hangar_store::repo::activity::ActivityRepo;
    use ainb_hangar_store::repo::comment::CommentRepo;

    let activities = ActivityRepo::list_for_issue(store.pool(), issue_id, limit)
        .await
        .context("read issue activity")?;
    let comments = CommentRepo::list_by_issue(store.pool(), workspace_id, issue_id)
        .await
        .context("read issue comments")?;

    let mut entries: Vec<TimelineEntryRow> = Vec::with_capacity(activities.len() + comments.len());
    for a in activities {
        if a.workspace_id != workspace_id {
            continue;
        }
        let details = a.details_json();
        entries.push(TimelineEntryRow {
            kind: TIMELINE_KIND_ACTIVITY.to_string(),
            id: a.id,
            actor_type: a.actor_type,
            actor_id: a.actor_id,
            created_at: a.created_at,
            action: Some(a.action),
            details: (!details.as_object().is_some_and(serde_json::Map::is_empty))
                .then_some(details),
            body: None,
        });
    }
    for c in comments {
        entries.push(TimelineEntryRow {
            kind: TIMELINE_KIND_COMMENT.to_string(),
            id: c.id,
            actor_type: Some(c.author.kind().as_str().to_string()),
            actor_id: Some(c.author.id().to_string()),
            created_at: c.created_at,
            action: None,
            details: None,
            body: Some(c.body),
        });
    }
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
    let cap = usize::try_from(limit.max(0)).unwrap_or(usize::MAX);
    if entries.len() > cap {
        entries.drain(..entries.len() - cap);
    }
    Ok(entries)
}

/// `member:<id>` / `agent:<id>` / `system` for one timeline entry.
fn timeline_actor(e: &ainb_hangar_proto::snapshots::TimelineEntryRow) -> String {
    match (e.actor_type.as_deref(), e.actor_id.as_deref()) {
        (Some("system") | None, _) => "system".to_string(),
        (Some(kind), Some(id)) => format!("{kind}:{id}"),
        (Some(kind), None) => kind.to_string(),
    }
}

/// The human-readable right-hand column: the comment body, or the change the
/// activity's details describe (`open → in_progress`).
fn timeline_detail(e: &ainb_hangar_proto::snapshots::TimelineEntryRow) -> String {
    if let Some(body) = e.body.as_deref() {
        return body.replace('\n', " ");
    }
    let Some(details) = e.details.as_ref() else {
        return String::new();
    };
    let render = |v: Option<&serde_json::Value>| -> String {
        match v {
            None | Some(serde_json::Value::Null) => "—".to_string(),
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        }
    };
    // The assignee shape carries `*_type` + `*_id` halves, each side omitted when
    // absent; every other shape is a plain `from`/`to` pair.
    if details.get("from_type").is_some() || details.get("to_type").is_some() {
        let side = |t: &str, i: &str| match (details.get(t), details.get(i)) {
            (Some(serde_json::Value::String(t)), Some(serde_json::Value::String(i))) => {
                format!("{t}:{i}")
            }
            _ => "—".to_string(),
        };
        return format!(
            "{} → {}",
            side("from_type", "from_id"),
            side("to_type", "to_id")
        );
    }
    if details.get("from").is_some() || details.get("to").is_some() {
        let via = details
            .get("via")
            .and_then(serde_json::Value::as_str)
            .map(|v| format!(" (via {v})"))
            .unwrap_or_default();
        return format!(
            "{} → {}{via}",
            render(details.get("from")),
            render(details.get("to"))
        );
    }
    details.to_string()
}

/// One dispatch attempt as the `DispatchAttemptRow` wire shape, so
/// `issue why --format json` and `hangar/dispatch_attempts_list` agree.
fn dispatch_attempt_json(
    a: &ainb_hangar_store::repo::dispatch_attempt::DispatchAttempt,
) -> serde_json::Value {
    serde_json::json!({
        "id": a.id,
        "issue_id": a.issue_id,
        "agent_id": a.agent_id,
        "runtime_id": a.runtime_id,
        "task_id": a.task_id,
        "reason": a.reason,
        "detail": a.detail,
        "source": a.source,
        "created_at": a.created_at,
    })
}

/// The newest DECLINED dispatch attempt for an issue, as
/// `(code, detail)` — what `issue show` prints as its `Not dispatched:` line.
/// `None` when the card never tried to dispatch, or its newest attempt succeeded.
async fn latest_dispatch_decline(
    store: &Store,
    issue_id: &str,
) -> Result<Option<(String, Option<String>)>> {
    use ainb_hangar_store::repo::dispatch_attempt::DispatchAttemptRepo;
    Ok(
        DispatchAttemptRepo::latest_for_issue(store.pool(), issue_id)
            .await
            .context("read latest dispatch attempt")?
            .filter(|a| !a.is_dispatched())
            .map(|a| (a.reason, a.detail)),
    )
}

/// Whether an issue carries ANY typed link (multica parity #20) — so
/// `issue show` can omit the `Links:` section entirely rather than printing an
/// empty header.
async fn issue_has_links(store: &Store, issue_id: &str) -> Result<bool> {
    use ainb_hangar_store::repo::card_dependency::CardDependencyRepo;
    Ok(!CardDependencyRepo::blockers_of(store.pool(), issue_id)
        .await
        .context("read blockers")?
        .is_empty()
        || !CardDependencyRepo::blocks_of(store.pool(), issue_id)
            .await
            .context("read blocked cards")?
            .is_empty()
        || !CardDependencyRepo::related_of(store.pool(), issue_id)
            .await
            .context("read related cards")?
            .is_empty())
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

// ──────────────────────────────────────────────────────────────────────────
// Daemon lifecycle (e38.20): run / start / stop / restart / setup / status.
//
// `start` spawns the `ainb-hangar-daemon` binary as a detached background child
// and records its EXACT pid in `<hangar_home>/hangar/daemon.pid`; `stop` reads
// that pid back and signals it directly (never by name). The pid file is the
// single source of truth for liveness, cross-checked against the bound socket.
// ──────────────────────────────────────────────────────────────────────────

/// Env override pointing at the `ainb-hangar-daemon` binary to spawn.
///
/// Production resolves the binary as a sibling of the running `ainb` executable
/// (then falls back to `$PATH`); the integration test sets this to the
/// cargo-built daemon binary so `start` spawns the test artifact, not whatever
/// `ainb-hangar-daemon` happens to be installed.
const DAEMON_BIN_ENV: &str = "AINB_HANGAR_DAEMON_BIN";

/// Resolve the path to the daemon's PID file: `<hangar_home>/hangar/daemon.pid`.
fn daemon_pid_path() -> Result<std::path::PathBuf> {
    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;
    // One source of truth with the daemon's own boot-time self-registration.
    Ok(ainb_hangar_daemon::pid_path_in(&home))
}

/// Path to the file recording the version of the binary that started the
/// running daemon, written beside the pid file at launch.
///
/// A running daemon is never auto-restarted, so after `brew upgrade` (or any
/// rebuild) the OLD daemon keeps serving while the CLI/TUI is new. This file
/// lets `status` name the running daemon's version and flag the skew — without
/// a socket dial (the CLI opens the store directly and never RPCs the daemon).
fn daemon_version_path() -> Result<std::path::PathBuf> {
    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;
    Ok(home.join("hangar").join("daemon.version"))
}

/// Compare the recorded running-daemon version to `mine`, returning
/// `Some(running)` when they disagree (the skew to warn about), else `None`.
///
/// `None` also when the running version is unknown (no file / empty) — an
/// absent record is not proof of skew, so it degrades to "no warning" rather
/// than a false positive. A pure function so the precedence is unit-testable.
fn daemon_version_skew(running: Option<&str>, mine: &str) -> Option<String> {
    match running {
        Some(v) if !v.is_empty() && v != mine => Some(v.to_string()),
        _ => None,
    }
}

/// Read the recorded daemon pid, or `None` if the file is absent/empty/garbage.
fn read_daemon_pid(path: &std::path::Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse().ok()
}

/// Is `pid` a live process? `kill(pid, 0)` succeeds iff it exists (and we may
/// signal it), so this is a non-destructive liveness probe.
fn pid_is_running(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    matches!(kill(Pid::from_raw(pid as i32), None), Ok(()))
}

/// Resolve how `start` launches the daemon: the dedicated
/// `ainb-hangar-daemon` binary when one can be found ([`DAEMON_BIN_ENV`]
/// override → sibling of the current executable → `$PATH`), else re-exec this
/// very `ainb` binary with `hangar daemon run`. The daemon library is
/// compiled into `ainb`, and installed layouts (e.g. Homebrew) ship no
/// sidecar binary — without the fallback, `start` failed with an error nobody
/// saw (the TUI plugin spawns this CLI with discarded stdio) and the offline
/// panel sat there forever.
fn resolve_daemon_launch() -> (std::path::PathBuf, Vec<&'static str>) {
    // An explicit override is honoured verbatim — if it points at nothing,
    // fail loudly rather than silently running something else.
    if let Some(p) = std::env::var_os(DAEMON_BIN_ENV).filter(|p| !p.is_empty()) {
        return (std::path::PathBuf::from(p), Vec::new());
    }
    let exe = std::env::current_exe().ok();
    resolve_daemon_launch_for(exe.as_deref())
}

/// Modification time of `path`, or `None` if it can't be read.
fn file_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Is a sibling `ainb-hangar-daemon` fresh enough to prefer over the self-exec
/// fallback, given the sibling's and this `ainb`'s modification times?
///
/// The daemon library is compiled INTO `ainb`, so the self-exec fallback
/// (`ainb hangar daemon run`) always executes code exactly as fresh as this
/// binary. The standalone sibling is therefore only a nicety — and a *stale*
/// one is actively harmful: a plain `cargo build` (default-members) historically
/// rebuilt only `ainb`, leaving an older `ainb-hangar-daemon` beside it, so
/// `start` silently launched pre-fix daemon code. We consequently prefer the
/// sibling ONLY when it is at least as new as `ainb`; an older sibling is
/// treated as a stale build and skipped in favour of the fresh embedded daemon.
/// When either mtime is unreadable we degrade to trusting the sibling, preserving
/// the prior behaviour for layouts where file times are unavailable.
fn sibling_daemon_is_fresh(
    sibling_mtime: Option<std::time::SystemTime>,
    exe_mtime: Option<std::time::SystemTime>,
) -> bool {
    match (sibling_mtime, exe_mtime) {
        (Some(sibling), Some(exe)) => sibling >= exe,
        _ => true,
    }
}

/// The `current_exe`-parameterised core of [`resolve_daemon_launch`], split out
/// so the sibling-vs-self-exec decision is unit-testable without depending on
/// the test runner's own executable path. `exe` is the resolved path of the
/// running `ainb` (`None` iff `current_exe` failed).
fn resolve_daemon_launch_for(
    exe: Option<&std::path::Path>,
) -> (std::path::PathBuf, Vec<&'static str>) {
    if let Some(exe) = exe {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("ainb-hangar-daemon");
            if sibling.exists() {
                if sibling_daemon_is_fresh(file_mtime(&sibling), file_mtime(exe)) {
                    return (sibling, Vec::new());
                }
                // Stale sibling (older than this `ainb`): don't silently launch
                // pre-fix daemon code. Self-exec the fresh embedded daemon.
                return (exe.to_path_buf(), vec!["hangar", "daemon", "run"]);
            }
        }
    }
    if let Some(on_path) = find_on_path("ainb-hangar-daemon") {
        return (on_path, Vec::new());
    }
    // Self-exec fallback. `current_exe` failing is effectively unreachable;
    // degrade to the bare `ainb` name resolved by the OS if it does.
    let me = exe.map_or_else(
        || std::path::PathBuf::from("ainb"),
        std::path::Path::to_path_buf,
    );
    (me, vec!["hangar", "daemon", "run"])
}

/// First `$PATH` entry containing a file named `name`.
fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|c| c.is_file())
}

/// Dispatch a `hangar daemon <verb>`.
async fn dispatch_daemon(cmd: DaemonCommand, format: OutputFormat) -> Result<()> {
    match cmd {
        DaemonCommand::Status => run_daemon_status().await,
        DaemonCommand::Run => run_daemon_run().await,
        DaemonCommand::Start => run_daemon_start(),
        DaemonCommand::Stop => run_daemon_stop(),
        DaemonCommand::Restart => run_daemon_restart(),
        DaemonCommand::Setup => run_daemon_setup().await,
        DaemonCommand::Config(c) => dispatch_daemon_config(c, format).await,
        DaemonCommand::Cred(c) => dispatch_daemon_cred(c).await,
    }
}

/// Dispatch the `hangar daemon cred` verbs against the platform secret store.
///
/// Unlike `daemon config`, this touches no `daemon_config` row — a credential is
/// a secret, resolved and stored via `ainb_hangar_daemon::claude_cred`. The
/// daemon injects whatever is stored on its next dispatch, so no restart is
/// needed.
async fn dispatch_daemon_cred(cmd: DaemonCredCommand) -> Result<()> {
    use ainb_hangar_daemon::claude_cred;

    match cmd {
        DaemonCredCommand::Status => {
            let src = claude_cred::default::source();
            // Never prints the value — only the source label.
            println!("claude credential: {}", src.label());
            Ok(())
        }
        DaemonCredCommand::Clear => {
            claude_cred::default::clear_token().context("clear claude credential")?;
            println!("claude credential cleared");
            Ok(())
        }
        DaemonCredCommand::Set(args) => run_daemon_cred_set(args),
    }
}

/// Capture a token (from `claude setup-token` or STDIN) and store it. The token
/// is held only as bytes and never echoed. Synchronous: the subprocess and stdin
/// read are blocking, and this is a one-shot operator command, not on any hot path.
fn run_daemon_cred_set(args: DaemonCredSetArgs) -> Result<()> {
    use ainb_hangar_daemon::claude_cred::{self, TokenBytes};

    // Held as `TokenBytes` (zeroize-on-drop), never a plain `String`, so the token
    // material is not left lingering in freed heap after the store write.
    let token: TokenBytes = if args.setup_token {
        // Drive the interactive browser flow. stderr/stdin are inherited so the
        // user sees the prompt and completes OAuth; stdout is captured for the
        // minted token. Verified shape: the token is an `sk-ant-oat…` word.
        let out = std::process::Command::new("claude")
            .arg("setup-token")
            .stderr(std::process::Stdio::inherit())
            .stdin(std::process::Stdio::inherit())
            .output()
            .context("run `claude setup-token` (is the claude CLI on PATH?)")?;
        if !out.status.success() {
            anyhow::bail!("`claude setup-token` exited without minting a token");
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let minted = claude_cred::extract_setup_token(&stdout)
            .context("no token found in `claude setup-token` output")?;
        TokenBytes::from(minted.as_bytes())
    } else {
        // Read from STDIN so the token never lands on argv or in shell history.
        use std::io::Read as _;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf).context("read token from stdin")?;
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            anyhow::bail!("no token on stdin (pipe the token, or use --setup-token)");
        }
        TokenBytes::from(trimmed.as_bytes())
    };

    claude_cred::default::store_token(token.as_bytes()).context("store claude credential")?;
    // Fixed confirmation: never echo the value, and never disclose its length.
    println!("claude credential stored");
    Ok(())
}

/// Dispatch the `hangar daemon config` verbs against the `daemon_config` table.
///
/// Opens the store directly (matching the other store-backed `hangar` verbs) and
/// drives [`DaemonConfigRepo`] + the registry. Writes take effect on the daemon's
/// next scan tick (it reloads config each tick), so no restart is needed.
async fn dispatch_daemon_config(cmd: DaemonConfigCommand, format: OutputFormat) -> Result<()> {
    let store = Store::open_default().await.context("open hangar database")?;
    match cmd {
        DaemonConfigCommand::List => run_daemon_config_list(&store, format).await,
        DaemonConfigCommand::Get(args) => run_daemon_config_get(&store, args, format).await,
        DaemonConfigCommand::Set(args) => run_daemon_config_set(&store, args).await,
    }
}

/// `hangar daemon config list`: every configurable's key, current value (or
/// `(default)`), default, and type/range. Iterates the registry so a new knob is
/// listed automatically, in every output format.
async fn run_daemon_config_list(store: &Store, format: OutputFormat) -> Result<()> {
    use ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY;
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    // Read each knob's stored value once, preserving registry order.
    let mut rows = Vec::with_capacity(DAEMON_CONFIG_REGISTRY.len());
    for desc in DAEMON_CONFIG_REGISTRY {
        let stored = DaemonConfigRepo::get(store.pool(), desc.key)
            .await
            .with_context(|| format!("read daemon_config `{}`", desc.key))?;
        rows.push((desc, stored));
    }
    print!("{}", render_daemon_config_list(&rows, format)?);
    Ok(())
}

/// One `config list` row: the knob's descriptor and its stored value (`None` when
/// the key has no row, i.e. the coded default is in force).
type ConfigRow = (
    &'static ainb_hangar_core::daemon_config::ConfigDescriptor,
    Option<String>,
);

/// Render the `config list` rows in `format`, returning the exact text to print.
///
/// Split from the IO so the rendering is directly testable: the parity test counts
/// the rows this emits against the registry length, which a test that merely
/// re-queried the registry could never do.
///
/// # Errors
///
/// Returns an error only when the JSON form fails to serialize.
fn render_daemon_config_list(rows: &[ConfigRow], format: OutputFormat) -> Result<String> {
    use std::fmt::Write as _;
    let mut out = String::new();
    match format {
        OutputFormat::Json => {
            let arr: Vec<_> = rows
                .iter()
                .map(|(desc, stored)| {
                    serde_json::json!({
                        "key": desc.key,
                        "value": stored,
                        "is_default": stored.is_none(),
                        "default": desc.default,
                        "type": desc.type_hint(),
                        "help": desc.help,
                    })
                })
                .collect();
            let _ = writeln!(
                out,
                "{}",
                serde_json::to_string_pretty(&arr).context("render config json")?
            );
        }
        OutputFormat::Csv => {
            let _ = writeln!(out, "key,value,is_default,default,type,help");
            for (desc, stored) in rows {
                let _ = writeln!(
                    out,
                    "{},{},{},{},{},{}",
                    csv_field(desc.key),
                    csv_field(stored.as_deref().unwrap_or(desc.default)),
                    csv_field(&stored.is_none().to_string()),
                    csv_field(desc.default),
                    csv_field(&desc.type_hint()),
                    csv_field(desc.help),
                );
            }
        }
        OutputFormat::Markdown => {
            let _ = writeln!(out, "| key | value | default | type | help |");
            let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
            for (desc, stored) in rows {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} | {} |",
                    md_cell(desc.key),
                    md_cell(&config_shown_value(desc, stored.as_deref())),
                    md_cell(desc.default),
                    md_cell(&desc.type_hint()),
                    md_cell(desc.help),
                );
            }
        }
        OutputFormat::Text => {
            for (desc, stored) in rows {
                let _ = writeln!(
                    out,
                    "{:<28} {:<20} [{}]  {}",
                    desc.key,
                    config_shown_value(desc, stored.as_deref()),
                    desc.type_hint(),
                    desc.help
                );
            }
        }
    }
    Ok(out)
}

/// A knob's display value: the stored string, or the coded default marked as such.
fn config_shown_value(
    desc: &ainb_hangar_core::daemon_config::ConfigDescriptor,
    stored: Option<&str>,
) -> String {
    stored.map_or_else(
        || format!("{} (default)", desc.default),
        ToString::to_string,
    )
}

/// `hangar daemon config get <key>`: print one knob's current value, or its
/// coded default when unset. Rejects an unknown key.
async fn run_daemon_config_get(
    store: &Store,
    args: DaemonConfigGetArgs,
    format: OutputFormat,
) -> Result<()> {
    use ainb_hangar_core::daemon_config::descriptor;
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    // Trim the key: `validate` already trims the VALUE, so a shell-quoted
    // `get " autostandup.enabled"` failing with "unknown config key" while the
    // matching set succeeded was pure asymmetry.
    let key = args.key.trim();
    let desc = descriptor(key).with_context(|| format!("unknown config key `{key}`"))?;
    let stored = DaemonConfigRepo::get(store.pool(), desc.key)
        .await
        .with_context(|| format!("read daemon_config `{}`", desc.key))?;
    let value = stored.clone().unwrap_or_else(|| desc.default.to_string());

    match format {
        OutputFormat::Json => {
            let v = serde_json::json!({
                "key": desc.key,
                "value": value,
                "is_default": stored.is_none(),
                "default": desc.default,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&v).context("render config json")?
            );
        }
        OutputFormat::Csv => {
            println!("key,value,is_default,default");
            println!(
                "{},{},{},{}",
                csv_field(desc.key),
                csv_field(&value),
                csv_field(&stored.is_none().to_string()),
                csv_field(desc.default),
            );
        }
        OutputFormat::Markdown => {
            println!("| key | value | default |");
            println!("| --- | --- | --- |");
            println!(
                "| {} | {} | {} |",
                md_cell(desc.key),
                md_cell(&value),
                md_cell(desc.default),
            );
        }
        // Text stays the bare value: `get` is the scriptable read.
        OutputFormat::Text => {
            println!("{value}");
        }
    }
    Ok(())
}

/// `hangar daemon config set <key> <value>`: validate the value against the
/// knob's descriptor (rejecting an unknown key, out-of-range int, bad bool, or
/// bad enum with a clear message) and persist the normalized form.
async fn run_daemon_config_set(store: &Store, args: DaemonConfigSetArgs) -> Result<()> {
    use ainb_hangar_core::daemon_config::descriptor;
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    // Trim the key, matching `validate`'s treatment of the value.
    let key = args.key.trim();
    let desc = descriptor(key).with_context(|| format!("unknown config key `{key}`"))?;
    // Validation is the registry's single gate — the same one the RPC uses — so
    // the CLI and TUI reject identical bad input and store an identical form.
    let value = desc
        .validate(&args.value)
        .map_err(|e| anyhow::anyhow!(e))
        .context("invalid config value")?;
    DaemonConfigRepo::set(store.pool(), desc.key, &value)
        .await
        .with_context(|| format!("write daemon_config `{}`", desc.key))?;
    println!("set {} = {value}", desc.key);
    Ok(())
}

/// `hangar daemon status`: report the daemon's run state from the PID file +
/// socket, and the database reachability.
///
/// "running" requires the recorded pid to be alive; a pid file naming a dead
/// process is reported as a stale "stopped". The control socket's presence is
/// surfaced alongside as the second liveness signal.
async fn run_daemon_status() -> Result<()> {
    let pid_path = daemon_pid_path()?;
    let socket = ainb_hangar_daemon::hangar_dir()
        .context("resolve hangar home")?
        .join("hangar.sock");

    match read_daemon_pid(&pid_path) {
        Some(pid) if pid_is_running(pid) => {
            let sock = if socket.exists() {
                "socket bound"
            } else {
                "socket not yet bound"
            };
            println!("hangar daemon: running (pid {pid}, {sock})");
            // Version-skew check: a running daemon is never auto-restarted, so
            // after an upgrade an OLD daemon can still be serving. Compare the
            // recorded running-daemon version to THIS binary's and flag it.
            let running = daemon_version_path()
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|s| s.trim().to_string());
            match daemon_version_skew(running.as_deref(), env!("CARGO_PKG_VERSION")) {
                Some(running_v) => {
                    println!(
                        "  ⚠ version skew: daemon {running_v} vs this binary {} — \
                         run `ainb hangar daemon restart`",
                        env!("CARGO_PKG_VERSION")
                    );
                }
                None => println!("  version: {}", env!("CARGO_PKG_VERSION")),
            }
        }
        Some(pid) => {
            println!("hangar daemon: stopped (stale pid {pid}, process gone)");
        }
        None => {
            println!("hangar daemon: stopped (no pid file)");
        }
    }

    // The database-reachability line stays as a secondary signal: a stopped
    // daemon with a migrated db is still a healthy, bootable install.
    match Store::open_default().await {
        Ok(_) => println!("  database: reachable (migrations applied)"),
        Err(e) => println!("  database: unreachable: {e}"),
    }
    Ok(())
}

/// `hangar daemon run`: boot the daemon in the FOREGROUND.
///
/// Equivalent to launching the `ainb-hangar-daemon` binary directly — installs
/// the daemon's own observability sink, boots, binds the socket, self-registers
/// the runtime, and runs the claim loop until interrupted. Blocks; use `start`
/// for the background variant.
async fn run_daemon_run() -> Result<()> {
    // Mirror the standalone binary's bootstrap: the rolling `daemon.<date>`
    // JSONL under `<hangar_home>/hangar/logs` (+ optional OTLP). `main`'s
    // logging setup deliberately installs nothing for this invocation, but the
    // install is still guarded on the global dispatcher so an unexpected
    // pre-installed subscriber downgrades to reuse instead of a double-init
    // panic.
    let guard = if tracing::dispatcher::has_been_set() {
        None
    } else {
        let mut opts = ainb_hangar_daemon::observability::ObservabilityOpts::new(
            ainb_hangar_daemon::log_dir().context("resolve daemon log dir")?,
        );
        opts.otlp = ainb_hangar_daemon::observability::OtlpOpts::from_env();
        Some(
            ainb_hangar_daemon::observability::install(opts)
                .context("install daemon observability sink")?,
        )
    };

    let result = ainb_hangar_daemon::boot(false).await.context("run hangar daemon (foreground)");
    // Explicit flush/teardown on both paths (drop inside a live tokio runtime
    // is the trap the standalone binary documents).
    if let Some(guard) = guard {
        guard.shutdown();
    }
    result
}

/// `hangar daemon start`: spawn the daemon as a detached background child and
/// record its EXACT pid.
///
/// Idempotent: if the recorded pid is already alive, this is a no-op with a
/// notice (never a second daemon). The child is spawned with the same
/// `$AINB_HANGAR_HOME` this process resolved, so it shares one home; its stdout/
/// stderr go to the daemon's own rolling log, not this terminal.
fn run_daemon_start() -> Result<()> {
    start_daemon_if_stopped(true)
}

/// Best-effort autostart of the Hangar daemon before the TUI connects.
///
/// Idempotent (a live pid is a no-op) and non-fatal: a spawn failure is logged
/// and swallowed so the TUI still launches (it shows the offline panel until the
/// daemon comes up). Quiet — no stdout, since the TUI owns the terminal. Mirrors
/// `mcp_pool`'s `ensure_daemon` warn-and-continue.
pub fn ensure_hangar_daemon() {
    if let Err(e) = start_daemon_if_stopped(false) {
        tracing::warn!(error = %e, "hangar daemon autostart failed (TUI continues)");
    }
}

/// Spawn the daemon as a detached background child unless it is already running,
/// recording its EXACT pid. When `announce` is true the outcome is printed
/// (the `hangar daemon start` CLI verb); the TUI autostart passes `false`.
fn start_daemon_if_stopped(announce: bool) -> Result<()> {
    let pid_path = daemon_pid_path()?;

    // Already running? Bail out cleanly rather than spawning a duplicate.
    if let Some(pid) = read_daemon_pid(&pid_path) {
        if pid_is_running(pid) {
            if announce {
                println!("hangar daemon: already running (pid {pid})");
            }
            return Ok(());
        }
        // Stale pid file from a crashed daemon: drop it before re-spawning.
        std::fs::remove_file(&pid_path).ok();
    }

    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).context("create hangar home dir")?;
    }

    let (bin, args) = resolve_daemon_launch();
    let launched = if args.is_empty() {
        bin.display().to_string()
    } else {
        format!("{} {}", bin.display(), args.join(" "))
    };
    let mut child = std::process::Command::new(&bin)
        .args(&args)
        // The child must not inherit this process's controlling terminal's
        // stdio; the daemon writes its own rolling JSONL log under the hangar
        // home, so discard the std streams.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawn daemon `{launched}`"))?;

    // A daemon that dies instantly (unbootable db, broken binary) used to look
    // identical to a clean start — the TUI's `[s]` action reported success and
    // the offline panel sat there forever. Give the child a beat, then
    // reap-check: `try_wait` returns the exit status iff it already died.
    std::thread::sleep(std::time::Duration::from_millis(400));
    if let Some(status) = child.try_wait().context("probe daemon child")? {
        anyhow::bail!(
            "daemon exited immediately ({status}) — launched `{launched}`; \
             run `ainb hangar daemon run` in a terminal to see why"
        );
    }

    let pid = child.id();
    // Write the EXACT child pid (the one we just spawned) so `stop` signals this
    // process and no other. `child` is intentionally dropped without `wait` —
    // the daemon is meant to outlive this CLI invocation.
    std::fs::write(&pid_path, format!("{pid}\n"))
        .with_context(|| format!("write pid file {}", pid_path.display()))?;

    // Record the version of the binary now serving, beside the pid. The launcher
    // and the daemon it spawns share the workspace version, so this is the
    // running daemon's version. Best-effort: a write failure must not fail the
    // start (the skew check just degrades to "unknown").
    if let Ok(vpath) = daemon_version_path() {
        std::fs::write(&vpath, format!("{}\n", env!("CARGO_PKG_VERSION"))).ok();
    }

    if announce {
        println!("hangar daemon: started (pid {pid})");
    }
    Ok(())
}

/// `hangar daemon stop`: signal the EXACT recorded pid, then remove the file.
///
/// Reads the pid back from the PID file and sends `SIGTERM` to that exact
/// process (never a name-based `pkill`). A stale pid file (process already gone)
/// is cleaned up; an absent file is reported as "not running".
fn run_daemon_stop() -> Result<()> {
    let pid_path = daemon_pid_path()?;
    // Drop the version record too — a stopped daemon has no running version, and
    // a lingering file would make `status` compare against a dead daemon.
    if let Ok(vpath) = daemon_version_path() {
        std::fs::remove_file(&vpath).ok();
    }
    match read_daemon_pid(&pid_path) {
        Some(pid) if pid_is_running(pid) => {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
                .with_context(|| format!("send SIGTERM to pid {pid}"))?;
            std::fs::remove_file(&pid_path).ok();
            println!("hangar daemon: stopped (signalled pid {pid})");
        }
        Some(pid) => {
            std::fs::remove_file(&pid_path).ok();
            println!("hangar daemon: not running (cleaned up stale pid {pid})");
        }
        None => {
            println!("hangar daemon: not running");
        }
    }
    Ok(())
}

/// `hangar daemon restart`: `stop` (if running) then `start`.
fn run_daemon_restart() -> Result<()> {
    run_daemon_stop()?;
    run_daemon_start()
}

/// `hangar daemon setup`: one-command bring-up.
///
/// Ensures the store exists + is migrated (`Store::open_default`), mints/ensures
/// the socket-auth token (e38.1 `ensure_socket_token`) so the control plane
/// comes up authenticated, then `start`s the daemon. Idempotent: a second
/// `setup` reuses the existing token + db and is a no-op start if the daemon is
/// already up.
async fn run_daemon_setup() -> Result<()> {
    // Ensure the database + migrations, and resolve the home the token lives in.
    let store = Store::open_default().await.context("open hangar database")?;
    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;

    // Mint (or reuse) the socket-auth credential before the daemon binds, so a
    // client can present it on first frame. Reuses e38.1's ensure path.
    let token_path = ainb_hangar_daemon::rpc::auth::ensure_socket_token(store.pool(), &home)
        .await
        .context("ensure socket auth token")?;
    println!(
        "hangar setup: database ready, socket token at {}",
        token_path.display()
    );

    run_daemon_start()
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
    ainb_hangar_store::bootstrap::find_default_workspace(store.pool())
        .await
        .context("query default workspace")
}

/// Return the default workspace id, lazily bootstrapping a workspace + owner
/// user + member row when none exists.
///
/// The `issue` table's `workspace_id` FK requires a `workspace` row; the
/// `member:stevie` creator references a `member` row only at the service layer
/// (FK-less by design, per the actor module), so the member row is informational
/// but kept consistent. Idempotent: a second call returns the existing id.
async fn ensure_default_workspace(store: &Store) -> Result<String> {
    ainb_hangar_store::bootstrap::ensure_default_workspace(store.pool())
        .await
        .context("bootstrap default workspace")
}

/// Read a workspace's configured `issue_prefix` by id (e38.21).
///
/// `None` when the workspace has no prefix configured (the migration-0020 NULL
/// default) — the issue title is then used verbatim. Mirrors the daemon's
/// RPC-side `workspace_issue_prefix` so the CLI and RPC create paths agree.
async fn workspace_issue_prefix(
    pool: &sqlx::SqlitePool,
    workspace_id: &str,
) -> Result<Option<String>> {
    let prefix: Option<String> =
        sqlx::query_scalar("SELECT issue_prefix FROM workspace WHERE id = ?")
            .bind(workspace_id)
            .fetch_optional(pool)
            .await
            .context("read workspace issue prefix")?
            .flatten();
    Ok(prefix)
}

/// Return the default owner user id (the first user, oldest first), or `None`
/// if no user exists yet.
async fn default_owner_id(store: &Store) -> Result<Option<String>> {
    ainb_hangar_store::bootstrap::default_owner_id(store.pool())
        .await
        .context("query default owner user")
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
    // The assignee's canonical `member:<id>`/`agent:<id>` form surfaces the actor
    // KIND (a human member vs an agent), shown only when the issue is assigned.
    let assignee = i.assignee.as_ref().map_or_else(String::new, |a| format!("  assignee={a}"));
    format!(
        "{}  [{}]  priority={}  {}{assignee}{due}{labels}",
        i.id, i.state, i.priority, i.title
    )
}

/// Minimal stable JSON object for one issue (hand-rolled to avoid pulling a
/// serde derive onto the store's `Issue` type from this crate).
fn issue_to_json(i: &Issue) -> String {
    let desc = i.description.as_deref().map_or_else(|| "null".to_string(), json_string);
    let due = i.due_date.map_or_else(|| "null".to_string(), |d| d.to_string());
    // Assignee + creator render as their canonical `member:<id>`/`agent:<id>`
    // string (the polymorphic actor kind), `null` when the issue is unassigned.
    let assignee = i
        .assignee
        .as_ref()
        .map_or_else(|| "null".to_string(), |a| json_string(&a.to_string()));
    let creator = json_string(&i.creator.to_string());
    format!(
        "{{\"id\":{},\"workspace_id\":{},\"title\":{},\"description\":{},\"state\":{},\"assignee\":{},\"creator\":{},\"created_at\":{},\"priority\":{},\"due_date\":{},\"labels\":{}}}",
        json_string(&i.id),
        json_string(&i.workspace_id),
        json_string(&i.title),
        desc,
        json_string(&i.state),
        assignee,
        creator,
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

/// Render an autopilot's run history (`hangar autopilot runs <id>`).
///
/// Carries the two columns migration 0057 added: `SOURCE` (which trigger fired
/// the run) and `REASON` (why a `skipped` dispatch was declined).
fn render_autopilot_runs(
    rows: &[ainb_hangar_store::repo::autopilot::AutopilotRun],
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let body = rows.iter().map(autopilot_run_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("id,status,source,started_at,completed_at,failure_reason,by,attribution");
            for r in rows {
                println!(
                    "{},{},{},{},{},{},{},{}",
                    csv_field(&r.id),
                    csv_field(&r.status),
                    csv_field(&r.source),
                    r.started_at,
                    r.completed_at.map_or_else(String::new, |v| v.to_string()),
                    csv_field(r.failure_reason.as_deref().unwrap_or("")),
                    csv_field(r.accountable_actor.as_deref().unwrap_or("")),
                    csv_field(r.attribution.as_deref().unwrap_or("")),
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| run id | status | source | started | reason | by |");
            println!("| --- | --- | --- | --- | --- | --- |");
            for r in rows {
                println!(
                    "| {} | {} | {} | {} | {} | {} |",
                    md_cell(&r.id),
                    md_cell(&r.status),
                    md_cell(&r.source),
                    r.started_at,
                    md_cell(r.failure_reason.as_deref().unwrap_or("-")),
                    md_cell(&run_attribution_cell(r)),
                );
            }
        }
        OutputFormat::Text => {
            if rows.is_empty() {
                println!("no autopilot runs");
            } else {
                for r in rows {
                    println!(
                        "{}  {}  source={}  started={}  reason={}  by={}",
                        r.id,
                        r.status,
                        r.source,
                        r.started_at,
                        r.failure_reason.as_deref().unwrap_or("-"),
                        run_attribution_cell(r),
                    );
                }
            }
        }
    }
}

/// The `BY` cell for one run: the accountable human plus HOW it was resolved
/// (multica parity #14).
///
/// `-` for an unattributed run — a pre-0061 row, or an unattended fire of an
/// unversioned rule. An honest unknown, never a fabricated actor.
fn run_attribution_cell(r: &ainb_hangar_store::repo::autopilot::AutopilotRun) -> String {
    match (r.accountable_actor.as_deref(), r.attribution.as_deref()) {
        (Some(actor), Some(how)) => format!("{actor} ({how})"),
        (Some(actor), None) => actor.to_string(),
        _ => "-".to_string(),
    }
}

/// Render an autopilot's rule-version ledger (`hangar autopilot versions <id>`).
///
/// Newest-first. An empty ledger means the rule is UNVERSIONED — created before
/// migration 0061 and never edited since; the ledger was deliberately not
/// backfilled rather than fabricating a v1.
fn render_autopilot_versions(
    rows: &[ainb_hangar_store::repo::autopilot_rule_version::RuleVersion],
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let body = rows.iter().map(rule_version_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("version,change_kind,published_by,created_at");
            for v in rows {
                println!(
                    "{},{},{},{}",
                    v.version,
                    csv_field(&v.change_kind),
                    csv_field(v.published_by.as_deref().unwrap_or("")),
                    v.created_at,
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| version | change | published by | at |");
            println!("| --- | --- | --- | --- |");
            for v in rows {
                println!(
                    "| v{} | {} | {} | {} |",
                    v.version,
                    md_cell(&v.change_kind),
                    md_cell(v.published_by.as_deref().unwrap_or("-")),
                    v.created_at,
                );
            }
        }
        OutputFormat::Text => {
            if rows.is_empty() {
                println!("no rule versions (unversioned autopilot)");
            } else {
                for v in rows {
                    println!(
                        "v{}  {}  by={}  at={}",
                        v.version,
                        v.change_kind,
                        v.published_by.as_deref().unwrap_or("-"),
                        v.created_at,
                    );
                }
            }
        }
    }
}

/// One rule-version ledger row as a JSON object (the `--format json` surface).
fn rule_version_to_json(
    v: &ainb_hangar_store::repo::autopilot_rule_version::RuleVersion,
) -> String {
    serde_json::json!({
        "id": v.id,
        "autopilot_id": v.autopilot_id,
        "version": v.version,
        "change_kind": v.change_kind,
        "published_by": v.published_by,
        "config_summary": v.config_summary,
        "created_at": v.created_at,
    })
    .to_string()
}

/// One autopilot run as a JSON object (for the `--format json` surface).
fn autopilot_run_to_json(r: &ainb_hangar_store::repo::autopilot::AutopilotRun) -> String {
    serde_json::json!({
        "id": r.id,
        "autopilot_id": r.autopilot_id,
        "status": r.status,
        "source": r.source,
        "started_at": r.started_at,
        "completed_at": r.completed_at,
        "failure_reason": r.failure_reason,
        "accountable_actor": r.accountable_actor,
        "attribution": r.attribution,
    })
    .to_string()
}

/// Render the webhook delivery audit log (`hangar autopilot deliveries <id>`).
fn render_webhook_deliveries(
    rows: &[ainb_hangar_store::repo::autopilot_webhook::WebhookDelivery],
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let body = rows.iter().map(webhook_delivery_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("received_at,outcome,event,http_status,run_id");
            for d in rows {
                println!(
                    "{},{},{},{},{}",
                    d.received_at,
                    csv_field(&d.outcome),
                    csv_field(d.event.as_deref().unwrap_or("")),
                    d.http_status,
                    csv_field(d.run_id.as_deref().unwrap_or("")),
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| received_at | outcome | event | status | run |");
            println!("| --- | --- | --- | --- | --- |");
            for d in rows {
                println!(
                    "| {} | {} | {} | {} | {} |",
                    d.received_at,
                    d.outcome,
                    d.event.as_deref().unwrap_or("-"),
                    d.http_status,
                    d.run_id.as_deref().unwrap_or("-"),
                );
            }
        }
        OutputFormat::Text => {
            if rows.is_empty() {
                println!("no webhook deliveries");
            } else {
                for d in rows {
                    println!(
                        "{}  {}  event={}  status={}  run={}",
                        d.received_at,
                        d.outcome,
                        d.event.as_deref().unwrap_or("-"),
                        d.http_status,
                        d.run_id.as_deref().unwrap_or("-"),
                    );
                }
            }
        }
    }
}

/// One webhook delivery as a JSON object (for the `--format json` surface).
fn webhook_delivery_to_json(
    d: &ainb_hangar_store::repo::autopilot_webhook::WebhookDelivery,
) -> String {
    serde_json::json!({
        "id": d.id,
        "autopilot_id": d.autopilot_id,
        "received_at": d.received_at,
        "outcome": d.outcome,
        "event": d.event,
        "http_status": d.http_status,
        "run_id": d.run_id,
        "detail": d.detail,
    })
    .to_string()
}

/// One-line text summary of an autopilot.
fn autopilot_line(a: &Autopilot, last_run: Option<&str>) -> String {
    format!(
        "{}  {}  cron={}  mode={}  policy={}  next_tick={}  last_run={}  [{}]",
        a.id,
        a.name,
        a.cron_expr,
        a.execution_mode.as_str(),
        a.concurrency_policy.as_str(),
        a.next_tick_at.map_or_else(|| "-".to_string(), |v| v.to_string()),
        last_run.unwrap_or("-"),
        autopilot_badge(a.enabled),
    ) + if a.api_trigger_enabled { " [api]" } else { "" }
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
          \"cron_expr\":{},\"max_concurrent_runs\":{},\"execution_mode\":{},\
          \"concurrency_policy\":{},\"next_tick_at\":{},\"enabled\":{},\
          \"api_trigger_enabled\":{},\"last_run\":{}}}",
        json_string(&a.id),
        json_string(&a.workspace_id),
        json_string(&a.agent_id),
        json_string(&a.name),
        instructions,
        json_string(&a.cron_expr),
        a.max_concurrent_runs,
        json_string(a.execution_mode.as_str()),
        json_string(a.concurrency_policy.as_str()),
        next_tick,
        a.enabled,
        a.api_trigger_enabled,
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

/// Render `hangar skills list --agent <agent>`: one agent's attachments with
/// their per-agent enablement (parity #24).
///
/// A SEPARATE renderer from [`render_skill_list`] on purpose — this listing has
/// different columns, and folding it into the workspace listing's shared CSV /
/// markdown headers would break every fixture pinned to those headers.
fn render_agent_skill_links(
    links: &[ainb_hangar_store::repo::skill::AgentSkillLink],
    format: OutputFormat,
) {
    let state = |enabled: bool| if enabled { "enabled" } else { "disabled" };
    match format {
        OutputFormat::Json => {
            let body = links
                .iter()
                .map(|l| {
                    format!(
                        r#"{{"skill_id":{},"name":{},"enabled":{}}}"#,
                        json_string(l.skill_id.as_str()),
                        json_string(l.name.as_str()),
                        l.enabled
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("skill_id,name,enabled");
            for l in links {
                println!(
                    "{},{},{}",
                    csv_field(l.skill_id.as_str()),
                    csv_field(l.name.as_str()),
                    l.enabled
                );
            }
        }
        OutputFormat::Markdown => {
            println!("| name | enabled |");
            println!("| --- | --- |");
            for l in links {
                println!("| {} | {} |", l.name, l.enabled);
            }
        }
        OutputFormat::Text => {
            if links.is_empty() {
                println!("no skills attached");
            } else {
                for l in links {
                    println!("{}  {}", l.name, state(l.enabled));
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

/// One-line text summary of an agent (id, name, archived badge, model, blurb).
fn agent_line(a: &ainb_hangar_store::repo::agent::Agent) -> String {
    // The blurb (migration 0050) is appended only when set, so a metadata-less
    // agent's line is byte-identical to the pre-0050 rendering.
    let blurb = if a.description.is_empty() {
        String::new()
    } else {
        format!("  — {}", a.description)
    };
    // The archive audit (migration 0052) is appended only when the agent actually
    // carries a stamp, so an ACTIVE agent — and one archived before 0052 existed —
    // renders byte-identically to the pre-0052 line.
    let audit = archive_audit_suffix(a.archived_at, a.archived_by.as_ref());
    format!(
        "{}  {}{}  model={}  args={}  env={}{}{}",
        a.id,
        a.name,
        if a.archived { "  [archived]" } else { "" },
        a.model.as_deref().unwrap_or("-"),
        a.cli_args.len(),
        a.agent_env.len(),
        blurb,
        audit,
    )
}

/// Render the archive audit as a `  archived_by=<ref>@<ms>` suffix, or the EMPTY
/// string when the row carries no timestamp (active, or archived before migration
/// 0052). Shared by the agent and squad text lines so the two read the same.
fn archive_audit_suffix(
    archived_at: Option<i64>,
    archived_by: Option<&ainb_hangar_core::actor::ActorRef>,
) -> String {
    match archived_at {
        None => String::new(),
        Some(ms) => match archived_by {
            Some(actor) => format!("  archived_by={actor}@{ms}"),
            None => format!("  archived_at={ms}"),
        },
    }
}
const fn agent_csv_header() -> &'static str {
    "id,name,archived,archived_at,archived_by,model,thinking,args,env,description"
}
fn agent_csv_row(a: &ainb_hangar_store::repo::agent::Agent) -> String {
    format!(
        "{},{},{},{},{},{},{},{},{},{}",
        csv_field(a.id.as_str()),
        csv_field(a.name.as_str()),
        a.archived,
        a.archived_at.map(|ms| ms.to_string()).unwrap_or_default(),
        csv_field(&a.archived_by.as_ref().map(ToString::to_string).unwrap_or_default()),
        csv_field(a.model.as_deref().unwrap_or("")),
        csv_field(a.thinking.as_deref().unwrap_or("")),
        a.cli_args.len(),
        a.agent_env.len(),
        csv_field(a.description.as_str()),
    )
}
const fn agent_md_header() -> &'static str {
    "| id | name | archived | archived_at | archived_by | model | thinking | args | env | description |\n\
     | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n"
}
fn agent_md_row(a: &ainb_hangar_store::repo::agent::Agent) -> String {
    format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
        md_cell(a.id.as_str()),
        md_cell(a.name.as_str()),
        a.archived,
        md_cell(&a.archived_at.map(|ms| ms.to_string()).unwrap_or_else(|| "-".to_string())),
        md_cell(&a.archived_by.as_ref().map_or_else(|| "-".to_string(), ToString::to_string)),
        md_cell(a.model.as_deref().unwrap_or("-")),
        md_cell(a.thinking.as_deref().unwrap_or("-")),
        a.cli_args.len(),
        a.agent_env.len(),
        md_cell(a.description.as_str()),
    )
}
/// Minimal stable JSON object for one agent (id, name, archived + config knobs).
fn agent_to_json(a: &ainb_hangar_store::repo::agent::Agent) -> String {
    let model = a.model.as_deref().map_or_else(|| "null".to_string(), json_string);
    let thinking = a.thinking.as_deref().map_or_else(|| "null".to_string(), json_string);
    // The audit pair is `null` (not `0` / `""`) when unstamped — an honest
    // "unknown", distinguishable from an epoch-0 archive by an unattributed actor.
    let archived_at = a.archived_at.map_or_else(|| "null".to_string(), |ms| ms.to_string());
    let archived_by = a.archived_by.as_ref().map_or_else(
        || "null".to_string(),
        |actor| json_string(&actor.to_string()),
    );
    let args = json_string_array(a.cli_args.iter().map(String::as_str));
    // Parity #30 / multica `redactEnv` (`agent.go:552-562`): KEYS are preserved,
    // every VALUE becomes the `****` mask, and `env_redacted` says so. This used
    // to print `"env":{"SECRET_TOKEN":"sk-live-…"}` — full plaintext on stdout.
    let env = a
        .agent_env
        .redacted_pairs()
        .into_iter()
        .map(|(k, mask)| format!("{}:{}", json_string(k), json_string(mask)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"id\":{},\"name\":{},\"archived\":{},\"archived_at\":{},\"archived_by\":{},\"model\":{},\"thinking\":{},\"args\":{},\"env\":{{{}}},\"env_key_count\":{},\"env_redacted\":{},\"description\":{}}}",
        json_string(a.id.as_str()),
        json_string(a.name.as_str()),
        a.archived,
        archived_at,
        archived_by,
        model,
        thinking,
        args,
        env,
        a.agent_env.len(),
        !a.agent_env.is_empty(),
        json_string(a.description.as_str()),
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Member render helpers (pure, over the store's Member row — e38.11).
// ──────────────────────────────────────────────────────────────────────────

/// Render a slice of members as a list in the chosen format (user id, email, role).
fn render_member_list(members: &[ainb_hangar_store::repo::member::Member], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let body = members.iter().map(member_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", member_csv_header());
            for m in members {
                println!("{}", member_csv_row(m));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", member_md_header());
            for m in members {
                println!("{}", member_md_row(m));
            }
        }
        OutputFormat::Text => {
            if members.is_empty() {
                println!("no members");
            } else {
                for m in members {
                    println!("{}", member_line(m));
                }
            }
        }
    }
}

/// One-line text summary of a member (user id, email, role).
fn member_line(m: &ainb_hangar_store::repo::member::Member) -> String {
    format!("{}  {}  role={}", m.user_id, m.email, m.role)
}
const fn member_csv_header() -> &'static str {
    "user_id,email,role"
}
fn member_csv_row(m: &ainb_hangar_store::repo::member::Member) -> String {
    format!(
        "{},{},{}",
        csv_field(&m.user_id),
        csv_field(&m.email),
        csv_field(&m.role),
    )
}
const fn member_md_header() -> &'static str {
    "| user_id | email | role |\n| --- | --- | --- |\n"
}
fn member_md_row(m: &ainb_hangar_store::repo::member::Member) -> String {
    format!(
        "| {} | {} | {} |",
        md_cell(&m.user_id),
        md_cell(&m.email),
        md_cell(&m.role),
    )
}
/// Minimal stable JSON object for one member (user id, email, role).
fn member_to_json(m: &ainb_hangar_store::repo::member::Member) -> String {
    format!(
        "{{\"user_id\":{},\"email\":{},\"role\":{}}}",
        json_string(&m.user_id),
        json_string(&m.email),
        json_string(&m.role),
    )
}

// ──────────────────────────────────────────────────────────────────────────
// Invitation render helpers (pure, over the store's Invitation row — #18).
// ──────────────────────────────────────────────────────────────────────────

/// Render a slice of pending invitations in the chosen format.
fn render_invitation_list(
    invites: &[ainb_hangar_store::repo::invitation::Invitation],
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let body = invites.iter().map(invitation_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("{}", invitation_csv_header());
            for i in invites {
                println!("{}", invitation_csv_row(i));
            }
        }
        OutputFormat::Markdown => {
            print!("{}", invitation_md_header());
            for i in invites {
                println!("{}", invitation_md_row(i));
            }
        }
        OutputFormat::Text => {
            if invites.is_empty() {
                println!("no pending invitations");
            } else {
                for i in invites {
                    println!("{}", invitation_line(i));
                }
            }
        }
    }
}

/// One-line text summary of an invitation (id, email, role, status, expiry).
fn invitation_line(i: &ainb_hangar_store::repo::invitation::Invitation) -> String {
    format!(
        "{}  {}  role={}  {}  expires {}",
        i.id,
        i.invitee_email,
        i.role,
        i.status,
        fmt_epoch_ms_utc(i.expires_at)
    )
}
const fn invitation_csv_header() -> &'static str {
    "id,invitee_email,role,status,expires_at"
}
fn invitation_csv_row(i: &ainb_hangar_store::repo::invitation::Invitation) -> String {
    format!(
        "{},{},{},{},{}",
        csv_field(&i.id),
        csv_field(&i.invitee_email),
        csv_field(&i.role),
        csv_field(&i.status),
        i.expires_at,
    )
}
const fn invitation_md_header() -> &'static str {
    "| id | invitee_email | role | status | expires_at |\n| --- | --- | --- | --- | --- |\n"
}
fn invitation_md_row(i: &ainb_hangar_store::repo::invitation::Invitation) -> String {
    format!(
        "| {} | {} | {} | {} | {} |",
        md_cell(&i.id),
        md_cell(&i.invitee_email),
        md_cell(&i.role),
        md_cell(&i.status),
        fmt_epoch_ms_utc(i.expires_at),
    )
}
/// Minimal stable JSON object for one invitation.
fn invitation_to_json(i: &ainb_hangar_store::repo::invitation::Invitation) -> String {
    format!(
        "{{\"id\":{},\"invitee_email\":{},\"role\":{},\"status\":{},\"created_at\":{},\"expires_at\":{}}}",
        json_string(&i.id),
        json_string(&i.invitee_email),
        json_string(&i.role),
        json_string(&i.status),
        i.created_at,
        i.expires_at,
    )
}

/// Render the squad status view in the chosen format (e38.17). Each row carries
/// the squad's id, name, leader actor-ref, and its members (joined with `,` in the
/// flat text/csv/markdown surfaces, a JSON array in the JSON surface).
fn render_squad_list(squads: &[ainb_hangar_store::repo::squad::Squad], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let body = squads.iter().map(squad_to_json).collect::<Vec<_>>().join(",");
            println!("[{body}]");
        }
        OutputFormat::Csv => {
            println!("id,name,leader,members,archived,archived_at,archived_by");
            for s in squads {
                println!("{}", squad_csv_row(s));
            }
        }
        OutputFormat::Markdown => {
            print!(
                "| id | name | leader | members | archived | archived_at | archived_by |\n\
                 | --- | --- | --- | --- | --- | --- | --- |\n"
            );
            for s in squads {
                println!("{}", squad_md_row(s));
            }
        }
        OutputFormat::Text => {
            if squads.is_empty() {
                println!("no squads");
            } else {
                for s in squads {
                    println!("{}", squad_line(s));
                }
            }
        }
    }
}

/// One-line text summary of a squad (id, name, leader, member actor-refs).
fn squad_line(s: &ainb_hangar_store::repo::squad::Squad) -> String {
    format!(
        "{}  {}  leader={}  members=[{}]{}{}",
        s.id,
        s.name,
        s.leader,
        squad_members_joined(s),
        if s.archived { "  [archived]" } else { "" },
        archive_audit_suffix(s.archived_at, s.archived_by.as_ref()),
    ) + &squad_instructions_suffix(s)
}

/// The squad's routing guidance as an indented follow-on line for the TEXT
/// surface, or `""` when blank (migration 0053) — a squad with no instructions
/// prints exactly the one line it printed before the column existed.
fn squad_instructions_suffix(s: &ainb_hangar_store::repo::squad::Squad) -> String {
    if s.instructions.is_empty() {
        String::new()
    } else {
        format!("\n  instructions: {}", s.instructions)
    }
}
fn squad_csv_row(s: &ainb_hangar_store::repo::squad::Squad) -> String {
    format!(
        "{},{},{},{},{},{},{}",
        csv_field(&s.id),
        csv_field(&s.name),
        csv_field(&s.leader.to_string()),
        csv_field(&squad_members_joined(s)),
        s.archived,
        s.archived_at.map(|ms| ms.to_string()).unwrap_or_default(),
        csv_field(&s.archived_by.as_ref().map(ToString::to_string).unwrap_or_default()),
    )
}
fn squad_md_row(s: &ainb_hangar_store::repo::squad::Squad) -> String {
    format!(
        "| {} | {} | {} | {} | {} | {} | {} |",
        md_cell(&s.id),
        md_cell(&s.name),
        md_cell(&s.leader.to_string()),
        md_cell(&squad_members_joined(s)),
        s.archived,
        md_cell(&s.archived_at.map(|ms| ms.to_string()).unwrap_or_else(|| "-".to_string())),
        md_cell(&s.archived_by.as_ref().map_or_else(|| "-".to_string(), ToString::to_string)),
    )
}
/// Minimal stable JSON object for one squad (id, name, leader, members array,
/// plus the parity-#25 `instructions` string and `member_roles` array).
///
/// APPEND-ONLY, mirroring the wire row: `members` stays a flat array of
/// actor-refs so an existing consumer keeps parsing it, and roles ride a parallel
/// `member_roles` array of `{member, role}` objects keyed by the actor-ref (join
/// by `member`, never by index).
fn squad_to_json(s: &ainb_hangar_store::repo::squad::Squad) -> String {
    let member_roles = s
        .members
        .iter()
        .filter(|m| !m.role.is_empty())
        .map(|m| {
            format!(
                "{{\"member\":{},\"role\":{}}}",
                json_string(&m.actor.to_string()),
                json_string(&m.role)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"id\":{},\"name\":{},\"leader\":{},\"members\":{},\"archived\":{},\"archived_at\":{},\"archived_by\":{},\"instructions\":{},\"member_roles\":[{member_roles}]}}",
        json_string(&s.id),
        json_string(&s.name),
        json_string(&s.leader.to_string()),
        json_string_array(
            s.members
                .iter()
                .map(|m| m.actor.to_string())
                .collect::<Vec<_>>()
                .iter()
                .map(String::as_str)
        ),
        s.archived,
        s.archived_at.map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        s.archived_by.as_ref().map_or_else(
            || "null".to_string(),
            |actor| json_string(&actor.to_string())
        ),
        json_string(&s.instructions),
    )
}
/// Join a squad's member actor-refs with `, ` for the flat text surfaces, each
/// suffixed with ` (role: …)` when the membership carries one (migration 0053).
/// A roleless member renders exactly as it did before the column existed.
fn squad_members_joined(s: &ainb_hangar_store::repo::squad::Squad) -> String {
    s.members
        .iter()
        .map(|m| {
            if m.role.is_empty() {
                m.actor.to_string()
            } else {
                format!("{} (role: {})", m.actor, m.role)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
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
    "id,state,title,description,assignee,created_at,priority,due_date,labels"
}
fn issue_csv_row(i: &Issue) -> String {
    let due = i.due_date.map_or_else(String::new, |d| d.to_string());
    let assignee = i.assignee.as_ref().map(ToString::to_string).unwrap_or_default();
    format!(
        "{},{},{},{},{},{},{},{},{}",
        csv_field(&i.id),
        csv_field(&i.state),
        csv_field(&i.title),
        csv_field(i.description.as_deref().unwrap_or("")),
        csv_field(&assignee),
        i.created_at,
        i.priority,
        csv_field(&due),
        csv_field(&i.labels.join(" ")),
    )
}
const fn issue_md_header() -> &'static str {
    "| id | state | title | description | assignee | priority | due_date | labels |\n\
     | --- | --- | --- | --- | --- | --- | --- | --- |\n"
}
fn issue_md_row(i: &Issue) -> String {
    let due = i.due_date.map_or_else(String::new, |d| d.to_string());
    let assignee = i.assignee.as_ref().map(ToString::to_string).unwrap_or_default();
    format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} |",
        md_cell(&i.id),
        md_cell(&i.state),
        md_cell(&i.title),
        md_cell(i.description.as_deref().unwrap_or("")),
        md_cell(&assignee),
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

    /// The skew helper flags a differing recorded version, and stays quiet for
    /// a match, an absent record, or an empty one (absence is not skew).
    #[test]
    fn daemon_version_skew_flags_only_a_known_mismatch() {
        assert_eq!(
            daemon_version_skew(Some("1.15.0"), "1.16.0"),
            Some("1.15.0".to_string())
        );
        assert_eq!(daemon_version_skew(Some("1.16.0"), "1.16.0"), None);
        assert_eq!(daemon_version_skew(None, "1.16.0"), None);
        assert_eq!(daemon_version_skew(Some(""), "1.16.0"), None);
    }

    /// The freshness predicate: a sibling at least as new as `ainb` is fresh; an
    /// older sibling is stale; an unreadable mtime degrades to trusting it.
    #[test]
    fn sibling_daemon_freshness_prefers_a_sibling_no_older_than_ainb() {
        use std::time::{Duration, SystemTime};
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let newer = base + Duration::from_secs(3600);
        // Sibling newer or equal → fresh.
        assert!(sibling_daemon_is_fresh(Some(newer), Some(base)));
        assert!(sibling_daemon_is_fresh(Some(base), Some(base)));
        // Sibling older than ainb → stale.
        assert!(!sibling_daemon_is_fresh(Some(base), Some(newer)));
        // Either mtime unknown → degrade to trusting the sibling.
        assert!(sibling_daemon_is_fresh(None, Some(base)));
        assert!(sibling_daemon_is_fresh(Some(base), None));
    }

    /// Set a file's mtime to an absolute instant (Rust 1.75+ `set_modified`).
    fn set_mtime(path: &std::path::Path, when: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// The build-skew guard: with a sibling `ainb-hangar-daemon` OLDER than the
    /// spawning `ainb` present, `start`'s launch resolution must NOT run it — it
    /// falls back to self-exec (`ainb hangar daemon run`, the fresh embedded
    /// daemon) instead of the stale sibling. This is the regression that let
    /// pre-#441 daemon code keep serving after a plain `cargo build`.
    #[test]
    fn resolve_daemon_launch_skips_a_sibling_older_than_ainb() {
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ainb");
        let sibling = dir.path().join("ainb-hangar-daemon");
        std::fs::write(&exe, b"ainb").unwrap();
        std::fs::write(&sibling, b"daemon").unwrap();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        // Sibling built BEFORE ainb — the stale-build signature.
        set_mtime(&sibling, base);
        set_mtime(&exe, base + Duration::from_secs(3600));

        let (bin, args) = resolve_daemon_launch_for(Some(&exe));
        assert_eq!(
            args,
            vec!["hangar", "daemon", "run"],
            "a sibling older than ainb must not be launched"
        );
        assert_eq!(bin, exe, "self-exec must run this very ainb binary");
    }

    /// The companion: a sibling at least as new as `ainb` IS launched directly
    /// (empty argv), so the freshness guard doesn't defeat the standalone binary
    /// on a clean co-build.
    #[test]
    fn resolve_daemon_launch_uses_a_sibling_no_older_than_ainb() {
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ainb");
        let sibling = dir.path().join("ainb-hangar-daemon");
        std::fs::write(&exe, b"ainb").unwrap();
        std::fs::write(&sibling, b"daemon").unwrap();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        set_mtime(&exe, base);
        set_mtime(&sibling, base + Duration::from_secs(3600));

        let (bin, args) = resolve_daemon_launch_for(Some(&exe));
        assert!(
            args.is_empty(),
            "a fresh sibling should be launched directly"
        );
        assert_eq!(bin, sibling);
    }

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

    // ──────────────────────────────────────────────────────────────────
    // Parity #30 — the per-agent env redaction contract at the CLI boundary.
    // ──────────────────────────────────────────────────────────────────

    /// The canary. Any appearance of this literal in rendered output is a leak.
    const ENV_SECRET: &str = "sk-live-DEADBEEF01";

    /// An agent row carrying one secret env var.
    fn agent_with_secret_env() -> ainb_hangar_store::repo::agent::Agent {
        ainb_hangar_store::repo::agent::Agent {
            id: "agent-1".to_string(),
            name: "secretive".to_string(),
            agent_env: vec![("SECRET_TOKEN".to_string(), ENV_SECRET.to_string())].into(),
            ..ainb_hangar_store::repo::agent::Agent::default()
        }
    }

    /// `hangar agent list --format json` used to print `"env":{"K":"<plaintext>"}`.
    /// Multica's contract is keep-keys / mask-values plus a `redacted` flag.
    #[test]
    fn agent_json_masks_env_values() {
        let out = agent_to_json(&agent_with_secret_env());
        assert!(
            !out.contains(ENV_SECRET),
            "agent JSON leaked the env value: {out}"
        );
        assert!(out.contains(r#""SECRET_TOKEN":"****""#), "{out}");
        assert!(out.contains(r#""env_key_count":1"#), "{out}");
        assert!(out.contains(r#""env_redacted":true"#), "{out}");
    }

    /// An env-less agent reports the honest zero/false, not a redacted-looking
    /// empty map with `env_redacted: true`.
    #[test]
    fn agent_json_env_less_agent_is_not_marked_redacted() {
        let out = agent_to_json(&ainb_hangar_store::repo::agent::Agent::default());
        assert!(out.contains(r#""env_key_count":0"#), "{out}");
        assert!(out.contains(r#""env_redacted":false"#), "{out}");
    }

    /// A malformed `--env` used to echo the WHOLE raw argument (the secret) into
    /// stderr via clap's `value_parser` error.
    #[test]
    fn parse_env_kv_error_never_echoes_the_value() {
        let no_eq = parse_env_kv(ENV_SECRET).expect_err("missing '=' must error");
        assert!(
            !no_eq.contains(ENV_SECRET),
            "error echoed the value: {no_eq}"
        );
        let empty_key = parse_env_kv(&format!("={ENV_SECRET}")).expect_err("empty key must error");
        assert!(
            !empty_key.contains("sk-live-"),
            "error echoed the value: {empty_key}"
        );
    }

    /// Clap must refuse any two of the three env write channels together, so a
    /// secret-bearing file can never be silently overridden by an argv value.
    #[test]
    fn env_file_and_env_stdin_and_env_are_mutually_exclusive() {
        let registry = CommandRegistry::built_ins();
        for extra in [
            vec!["--env", "A=b", "--env-stdin"],
            vec!["--env", "A=b", "--env-file", "/tmp/e.json"],
            vec!["--env-stdin", "--env-file", "/tmp/e.json"],
        ] {
            let mut argv = vec!["ainb", "hangar", "agent", "edit", "agent-1"];
            argv.extend(extra.iter().copied());
            let app = registry.build_clap(crate::cli::root_clap_command());
            assert!(
                app.try_get_matches_from(&argv).is_err(),
                "clap accepted mutually exclusive env channels: {argv:?}"
            );
        }
    }

    /// Multica's empty-input rule (`cmd_agent.go:750-762`): a blank payload is an
    /// ERROR (it almost always means a broken upstream pipe, and treating it as a
    /// clear silently wipes secrets); only the explicit `{}` clears.
    #[test]
    fn env_file_empty_is_an_error_but_brace_brace_clears() {
        let err = parse_env_json("   \n ", "--env-file").expect_err("blank input must error");
        assert!(err.contains("pass '{}' to clear"), "{err}");
        assert_eq!(
            parse_env_json("{}", "--env-file").expect("{} clears"),
            Vec::new()
        );
        assert_eq!(
            parse_env_json(
                &format!(r#"{{"SECRET_TOKEN":"{ENV_SECRET}"}}"#),
                "--env-file"
            )
            .expect("valid object"),
            vec![("SECRET_TOKEN".to_string(), ENV_SECRET.to_string())]
        );
    }

    /// A JSON parse failure must not reflect the payload back — `serde_json`
    /// messages quote the offending scalar, which here IS the secret.
    #[test]
    fn env_json_parse_error_never_echoes_the_payload() {
        let err = parse_env_json(&format!(r#"{{"K":"{ENV_SECRET}"#), "--env-stdin")
            .expect_err("truncated JSON must error");
        assert!(!err.contains("sk-live-"), "{err}");
        let err = parse_env_json(r#"{"K":31337}"#, "--env-stdin")
            .expect_err("non-string value must error");
        assert!(!err.contains("31337"), "{err}");
    }

    /// The redacted read verb parses, and carries NO `--reveal` (deviation D-1).
    #[test]
    fn parses_agent_env_verb_and_has_no_reveal_flag() {
        let cmd = parse_hangar(&["ainb", "hangar", "agent", "env", "agent-1"]);
        let HangarCommand::Agent(AgentCommand::Env(args)) = cmd else {
            panic!("expected agent env, got {cmd:?}");
        };
        assert_eq!(args.id, "agent-1");

        let registry = CommandRegistry::built_ins();
        let app = registry.build_clap(crate::cli::root_clap_command());
        assert!(
            app.try_get_matches_from(["ainb", "hangar", "agent", "env", "agent-1", "--reveal"])
                .is_err(),
            "there must be no plaintext escape hatch on the CLI"
        );
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
    fn parses_issue_create_acceptance_and_context_refs() {
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Ship gap 11",
            "--acceptance",
            "cargo build is green",
            "--acceptance",
            "detail card shows criteria",
            "--context-ref",
            "acme/api#42",
        ]);
        let HangarCommand::Issue(IssueCommand::Create(args)) = cmd else {
            panic!("expected issue create, got {cmd:?}");
        };
        assert_eq!(
            args.acceptance_criteria,
            vec![
                "cargo build is green".to_string(),
                "detail card shows criteria".to_string()
            ],
            "--acceptance is repeatable and order-preserving"
        );
        assert_eq!(
            args.context_refs,
            vec!["acme/api#42".to_string()],
            "--context-ref is repeatable and order-preserving"
        );

        // Omitted -> both lists empty.
        let cmd = parse_hangar(&["ainb", "hangar", "issue", "create", "--title", "Plain"]);
        let HangarCommand::Issue(IssueCommand::Create(args)) = cmd else {
            panic!("expected issue create, got {cmd:?}");
        };
        assert!(
            args.acceptance_criteria.is_empty(),
            "no --acceptance means no criteria"
        );
        assert!(
            args.context_refs.is_empty(),
            "no --context-ref means no context refs"
        );
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

    /// In-product recovery: `issue update --assign <agent>` on an unassigned issue
    /// that already carries a repo enqueues exactly ONE task for that agent — the
    /// CLI counterpart of the daemon assign seam, so a stuck issue is recoverable
    /// without filing a brand-new one.
    ///
    /// Mutation-provable: the issue starts with zero tasks and the only mutation is
    /// the assign edit. Strip the `enqueue_assigned_task` call from
    /// `run_issue_update` → zero task rows → this test goes red.
    #[tokio::test]
    async fn issue_update_assign_enqueues_recovery_task() {
        use ainb_hangar_store::bootstrap;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        let ws = bootstrap::ensure_default_workspace(store.pool())
            .await
            .expect("bootstrap workspace");
        bootstrap::ensure_runtime(store.pool(), &bootstrap::default_runtime_id(), 1)
            .await
            .expect("ensure runtime");
        let agent = bootstrap::create_agent(store.pool(), &ws, "worker", "claude", None)
            .await
            .expect("create agent");

        // An UNASSIGNED issue that carries a repo (create persists it) — the shape
        // of a failed/agent_error issue a user re-assigns to recover.
        let HangarCommand::Issue(IssueCommand::Create(cargs)) = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "recover me",
            "--repo",
            "scratch",
        ]) else {
            panic!("expected issue create");
        };
        run_issue_create(&store, cargs).await.expect("create issue");
        let issue = IssueRepo::list_by_workspace_state(store.pool(), &ws, DEFAULT_ISSUE_STATE)
            .await
            .expect("list issues")
            .into_iter()
            .find(|i| i.title == "recover me")
            .expect("created issue present");

        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(store.pool())
            .await
            .expect("count before");
        assert_eq!(before, 0, "no task exists before the assignment");

        // Post-creation assign — the seam the bug said only wrote the assignee.
        let HangarCommand::Issue(IssueCommand::Update(uargs)) = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "update",
            issue.id.as_str(),
            "--assign",
            agent.id.as_str(),
        ]) else {
            panic!("expected issue update");
        };
        run_issue_update(&store, uargs).await.expect("assign agent");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(store.pool())
            .await
            .expect("count after");
        assert_eq!(
            count, 1,
            "assigning an agent enqueues exactly one recovery task"
        );
        let task_agent: String =
            sqlx::query_scalar("SELECT agent_id FROM agent_task_queue LIMIT 1")
                .fetch_one(store.pool())
                .await
                .expect("read task agent");
        assert_eq!(
            task_agent, agent.id,
            "the task routes to the assigned agent"
        );
    }

    /// `parse_assignee` is polymorphic: a `member:`/`agent:` token keeps its
    /// kind, a bare id stays an agent (back-compat). Mutation-provable: flip the
    /// bare-id branch to Member and the last two assertions go red.
    #[test]
    fn parse_assignee_is_polymorphic_bare_id_is_agent() {
        let m = parse_assignee("member:u-1").expect("member ref");
        assert_eq!(m.kind(), ActorKind::Member);
        assert_eq!(m.id(), "u-1");
        let a = parse_assignee("agent:a-1").expect("agent ref");
        assert_eq!(a.kind(), ActorKind::Agent);
        assert_eq!(a.id(), "a-1");
        // Back-compat: a bare id is an agent, unchanged from before.
        let bare = parse_assignee("a-1").expect("bare id");
        assert_eq!(bare.kind(), ActorKind::Agent);
        assert_eq!(bare.id(), "a-1");
    }

    /// `issue create --assign member:<id>` persists `(member, id)` on the issue
    /// AND enqueues NO agent task — a human assignee does no work.
    ///
    /// Mutation-provable: if the create path silently coerced the member into an
    /// agent, the assignee kind would read `agent` (first assert) and a task row
    /// would appear (last assert). Both would go red.
    #[tokio::test]
    async fn issue_create_assign_member_persists_and_enqueues_no_task() {
        use ainb_hangar_core::ids::WorkspaceId;
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::member::{MemberRepo, MemberRole};

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        let ws_id = bootstrap::ensure_default_workspace(store.pool())
            .await
            .expect("bootstrap workspace");
        let ws = WorkspaceId::from_str(ws_id.clone()).unwrap();
        let member = MemberRepo::add(store.pool(), &ws, "dana@example.com", MemberRole::Member)
            .await
            .expect("add member");

        let HangarCommand::Issue(IssueCommand::Create(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "human task",
            "--assign",
            &format!("member:{}", member.user_id),
        ]) else {
            panic!("expected issue create");
        };
        run_issue_create(&store, args).await.expect("create issue");

        let issue = IssueRepo::list_by_workspace_state(store.pool(), &ws_id, DEFAULT_ISSUE_STATE)
            .await
            .expect("list issues")
            .into_iter()
            .find(|i| i.title == "human task")
            .expect("created issue present");
        let assignee = issue.assignee.as_ref().expect("member issue is assigned");
        assert_eq!(assignee.kind(), ActorKind::Member, "stored as a MEMBER");
        assert_eq!(assignee.id(), member.user_id, "the member's user id");

        let tasks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue")
            .fetch_one(store.pool())
            .await
            .expect("count tasks");
        assert_eq!(tasks, 0, "a member assignee enqueues NO agent task");

        // And the read surface shows the polymorphic kind.
        assert!(
            issue_to_json(&issue).contains(&format!("\"assignee\":\"member:{}\"", member.user_id)),
            "json surfaces the member actor-ref"
        );
        assert!(
            issue_line(&issue).contains(&format!("assignee=member:{}", member.user_id)),
            "text line surfaces the member actor-ref"
        );
    }

    /// `hangar member invite` then `hangar member accept` drives the whole
    /// parity-#18 lifecycle through the REAL clap surface: the member count goes
    /// 1 → 2, and only on accept.
    ///
    /// This mutation-tests the ARGV SHAPE, not just the repo — a repo-only test
    /// would stay green while the clap wiring (`--email`, `--role`, the
    /// positional invitation id, `--as`) was wrong or missing.
    #[tokio::test]
    async fn member_invite_then_accept_adds_member_via_cli() {
        use ainb_hangar_core::clock::SystemClock;
        use ainb_hangar_core::ids::WorkspaceId;
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::invitation::InvitationRepo;
        use ainb_hangar_store::repo::member::MemberRepo;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        let ws_id = bootstrap::ensure_default_workspace(store.pool())
            .await
            .expect("bootstrap workspace");
        let ws = WorkspaceId::from_str(ws_id).unwrap();
        assert_eq!(
            MemberRepo::list(store.pool(), &ws).await.unwrap().len(),
            1,
            "bootstrap starts with the sole owner"
        );

        let HangarCommand::Member(MemberCommand::Invite(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "member",
            "invite",
            "--email",
            "Dana@Example.com",
            "--role",
            "member",
        ]) else {
            panic!("expected member invite");
        };
        run_member_invite(&store, args).await.expect("invite");

        // An invite adds NO member — it only creates the pending row.
        assert_eq!(
            MemberRepo::list(store.pool(), &ws).await.unwrap().len(),
            1,
            "an invite is not a membership"
        );
        let pending = InvitationRepo::list_pending(store.pool(), &SystemClock, &ws)
            .await
            .expect("list pending");
        assert_eq!(pending.len(), 1, "one pending invitation");
        assert_eq!(
            pending[0].invitee_email, "dana@example.com",
            "the CLI email is normalised"
        );
        let invitation_id = pending[0].id.clone();

        // Accepting is what joins the member.
        let HangarCommand::Member(MemberCommand::Accept(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "member",
            "accept",
            &invitation_id,
            "--as",
            "dana@example.com",
        ]) else {
            panic!("expected member accept");
        };
        run_member_invite_accept(&store, args).await.expect("accept");

        let members = MemberRepo::list(store.pool(), &ws).await.unwrap();
        assert_eq!(members.len(), 2, "accept added the member");
        let dana = members.iter().find(|m| m.email == "dana@example.com").expect("dana joined");
        assert_eq!(dana.role, "member");
        assert!(
            InvitationRepo::list_pending(store.pool(), &SystemClock, &ws)
                .await
                .unwrap()
                .is_empty(),
            "the accepted invite is no longer pending"
        );
    }

    /// `--as` is REQUIRED on accept / decline: without an explicit identity the
    /// ownership gate would be theatre, so clap must refuse the argv outright.
    #[test]
    fn member_accept_requires_an_explicit_actor_email() {
        let registry = CommandRegistry::built_ins();
        let app = registry.build_clap(crate::cli::root_clap_command());
        let err = app
            .try_get_matches_from(["ainb", "hangar", "member", "accept", "inv-1"])
            .expect_err("accept without --as must be rejected");
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "got {err}"
        );
    }

    /// A foreign accept never creates a member, and the CLI says so in multica's
    /// wording.
    #[tokio::test]
    async fn member_accept_by_a_stranger_is_rejected_via_cli() {
        use ainb_hangar_core::clock::SystemClock;
        use ainb_hangar_core::ids::WorkspaceId;
        use ainb_hangar_store::bootstrap;
        use ainb_hangar_store::repo::invitation::InvitationRepo;
        use ainb_hangar_store::repo::member::MemberRepo;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        let ws_id = bootstrap::ensure_default_workspace(store.pool())
            .await
            .expect("bootstrap workspace");
        let ws = WorkspaceId::from_str(ws_id).unwrap();

        let HangarCommand::Member(MemberCommand::Invite(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "member",
            "invite",
            "--email",
            "dana@example.com",
        ]) else {
            panic!("expected member invite");
        };
        run_member_invite(&store, args).await.expect("invite");
        let invitation_id =
            InvitationRepo::list_pending(store.pool(), &SystemClock, &ws).await.unwrap()[0]
                .id
                .clone();

        let HangarCommand::Member(MemberCommand::Accept(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "member",
            "accept",
            &invitation_id,
            "--as",
            "eve@example.com",
        ]) else {
            panic!("expected member accept");
        };
        let err = run_member_invite_accept(&store, args).await.unwrap_err();
        assert!(
            err.to_string().contains("does not belong to you"),
            "got {err}"
        );
        assert_eq!(
            MemberRepo::list(store.pool(), &ws).await.unwrap().len(),
            1,
            "no member created by a stranger's accept"
        );
    }

    /// `issue create --assign agent:<id>` persists `(agent, id)` AND enqueues one
    /// task — the symmetric agent path still dispatches a run.
    ///
    /// Mutation-provable pair to the member test above: this asserts the task IS
    /// enqueued for an agent, so a change that skipped agent enqueue goes red.
    #[tokio::test]
    async fn issue_create_assign_agent_persists_and_enqueues_task() {
        use ainb_hangar_store::bootstrap;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        let ws = bootstrap::ensure_default_workspace(store.pool())
            .await
            .expect("bootstrap workspace");
        bootstrap::ensure_runtime(store.pool(), &bootstrap::default_runtime_id(), 1)
            .await
            .expect("ensure runtime");
        let agent = bootstrap::create_agent(store.pool(), &ws, "robot", "claude", None)
            .await
            .expect("create agent");

        let HangarCommand::Issue(IssueCommand::Create(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "robot task",
            "--repo",
            "scratch",
            "--assign",
            &format!("agent:{}", agent.id),
        ]) else {
            panic!("expected issue create");
        };
        run_issue_create(&store, args).await.expect("create issue");

        let issue = IssueRepo::list_by_workspace_state(store.pool(), &ws, DEFAULT_ISSUE_STATE)
            .await
            .expect("list issues")
            .into_iter()
            .find(|i| i.title == "robot task")
            .expect("created issue present");
        let assignee = issue.assignee.as_ref().expect("agent issue is assigned");
        assert_eq!(assignee.kind(), ActorKind::Agent, "stored as an AGENT");
        assert_eq!(assignee.id(), agent.id);

        let tasks: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM agent_task_queue WHERE agent_id = ?")
                .bind(&agent.id)
                .fetch_one(store.pool())
                .await
                .expect("count tasks");
        assert_eq!(tasks, 1, "an agent assignee enqueues exactly one run");
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
    fn parses_daemon_lifecycle_verbs() {
        for (verb, want) in [
            ("run", DaemonCommand::Run),
            ("start", DaemonCommand::Start),
            ("stop", DaemonCommand::Stop),
            ("restart", DaemonCommand::Restart),
            ("setup", DaemonCommand::Setup),
        ] {
            let cmd = parse_hangar(&["ainb", "hangar", "daemon", verb]);
            let HangarCommand::Daemon(got) = cmd else {
                panic!("`daemon {verb}` did not parse to a Daemon command");
            };
            // Compare via the debug repr — DaemonCommand has no PartialEq.
            assert_eq!(
                format!("{got:?}"),
                format!("{want:?}"),
                "`daemon {verb}` parsed to the wrong verb"
            );
        }
    }

    #[test]
    fn parses_daemon_config_verbs() {
        // list
        let cmd = parse_hangar(&["ainb", "hangar", "daemon", "config", "list"]);
        assert!(matches!(
            cmd,
            HangarCommand::Daemon(DaemonCommand::Config(DaemonConfigCommand::List))
        ));
        // get <key>
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "daemon",
            "config",
            "get",
            "autostandup.enabled",
        ]);
        let HangarCommand::Daemon(DaemonCommand::Config(DaemonConfigCommand::Get(args))) = cmd
        else {
            panic!("expected daemon config get");
        };
        assert_eq!(args.key, "autostandup.enabled");
        // set <key> <value>
        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "daemon",
            "config",
            "set",
            "autostandup.stagnant_min",
            "30",
        ]);
        let HangarCommand::Daemon(DaemonCommand::Config(DaemonConfigCommand::Set(args))) = cmd
        else {
            panic!("expected daemon config set");
        };
        assert_eq!(args.key, "autostandup.stagnant_min");
        assert_eq!(args.value, "30");
    }

    /// The CLI `list` must EMIT one row per registry knob, in every format — a
    /// knob added to the registry can never silently miss the CLI surface.
    ///
    /// This counts the rendered OUTPUT. The test it replaces asserted
    /// `descriptor(desc.key).is_some()` for each registry entry, which is true by
    /// definition (the registry is what `descriptor` searches) and never touched
    /// the rendering at all — it would have passed against a `list` that printed
    /// nothing.
    #[test]
    fn cli_list_emits_one_row_per_registry_knob_in_every_format() {
        use ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY;

        let rows: Vec<ConfigRow> = DAEMON_CONFIG_REGISTRY.iter().map(|d| (d, None)).collect();
        let n = DAEMON_CONFIG_REGISTRY.len();
        assert!(n > 0, "registry must not be empty");

        // Text: exactly one line per knob, each naming its key.
        let text = render_daemon_config_list(&rows, OutputFormat::Text).unwrap();
        let text_lines: Vec<&str> = text.lines().collect();
        assert_eq!(text_lines.len(), n, "text rows:\n{text}");
        for desc in DAEMON_CONFIG_REGISTRY {
            assert!(
                text_lines.iter().any(|l| l.contains(desc.key)),
                "`{}` missing from text output:\n{text}",
                desc.key
            );
        }

        // CSV: a header plus one row per knob.
        let csv = render_daemon_config_list(&rows, OutputFormat::Csv).unwrap();
        assert_eq!(csv.lines().count(), n + 1, "csv rows:\n{csv}");
        assert!(csv.starts_with("key,value,is_default,default,type,help"));

        // Markdown: a header + separator plus one row per knob.
        let md = render_daemon_config_list(&rows, OutputFormat::Markdown).unwrap();
        let md_rows = md.lines().filter(|l| l.starts_with("| ") && !l.contains("---")).count();
        assert_eq!(md_rows - 1, n, "markdown body rows:\n{md}");

        // JSON: one object per knob.
        let json = render_daemon_config_list(&rows, OutputFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed.as_array().expect("json array").len(), n);
    }

    /// `csv` and `markdown` used to fall through to the plain-text arm and emit
    /// neither format — the flag was silently ignored.
    #[test]
    fn cli_list_csv_and_markdown_are_not_plain_text() {
        use ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY;
        let rows: Vec<ConfigRow> = DAEMON_CONFIG_REGISTRY.iter().map(|d| (d, None)).collect();

        let text = render_daemon_config_list(&rows, OutputFormat::Text).unwrap();
        let csv = render_daemon_config_list(&rows, OutputFormat::Csv).unwrap();
        let md = render_daemon_config_list(&rows, OutputFormat::Markdown).unwrap();

        assert_ne!(csv, text, "csv must not be the plain-text arm");
        assert_ne!(md, text, "markdown must not be the plain-text arm");
        assert!(csv.contains(','), "csv must be comma-separated");
        assert!(md.contains('|'), "markdown must be a pipe table");
    }

    #[tokio::test]
    async fn config_set_get_round_trips_and_rejects_bad_input() {
        use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");

        // set autostandup.stagnant_min 30 → persists the normalized value.
        run_daemon_config_set(
            &store,
            DaemonConfigSetArgs {
                key: "autostandup.stagnant_min".to_string(),
                value: "30".to_string(),
            },
        )
        .await
        .expect("set stagnant_min");
        assert_eq!(
            DaemonConfigRepo::get(store.pool(), "autostandup.stagnant_min")
                .await
                .unwrap()
                .as_deref(),
            Some("30"),
            "set then read returns the written value"
        );

        // An out-of-range int is rejected and leaves the stored value untouched.
        let err = run_daemon_config_set(
            &store,
            DaemonConfigSetArgs {
                key: "autostandup.stagnant_min".to_string(),
                value: "99999".to_string(),
            },
        )
        .await
        .expect_err("out-of-range must be rejected");
        assert!(format!("{err:#}").contains("between 1 and 1440"));
        assert_eq!(
            DaemonConfigRepo::get(store.pool(), "autostandup.stagnant_min")
                .await
                .unwrap()
                .as_deref(),
            Some("30"),
            "a rejected write must not overwrite the prior value"
        );

        // An unknown key is rejected before any write.
        run_daemon_config_set(
            &store,
            DaemonConfigSetArgs {
                key: "autostandup.bogus".to_string(),
                value: "1".to_string(),
            },
        )
        .await
        .expect_err("unknown key must be rejected");

        // Enum normalization: `CODEX` stores as `codex`.
        run_daemon_config_set(
            &store,
            DaemonConfigSetArgs {
                key: "card_agent.default".to_string(),
                value: "CODEX".to_string(),
            },
        )
        .await
        .expect("set enum");
        assert_eq!(
            DaemonConfigRepo::get(store.pool(), "card_agent.default")
                .await
                .unwrap()
                .as_deref(),
            Some("codex"),
        );

        // get on an unset key errors on an unknown key but not on a known-unset one.
        run_daemon_config_get(
            &store,
            DaemonConfigGetArgs {
                key: "autostandup.cooldown_min".to_string(),
            },
            OutputFormat::Text,
        )
        .await
        .expect("get a known but unset key falls back to the default");
        run_daemon_config_get(
            &store,
            DaemonConfigGetArgs {
                key: "nope.nope".to_string(),
            },
            OutputFormat::Text,
        )
        .await
        .expect_err("get on an unknown key is rejected");
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
        for (verb, is) in [("disable", "disable"), ("enable", "enable")] {
            let cmd = parse_hangar(&["ainb", "hangar", "autopilot", verb, "ap-1"]);
            match (is, cmd) {
                ("disable", HangarCommand::Autopilot(AutopilotCommand::Disable(a)))
                | ("enable", HangarCommand::Autopilot(AutopilotCommand::Enable(a))) => {
                    assert_eq!(a.id, "ap-1");
                }
                (_, other) => panic!("expected autopilot {verb}, got {other:?}"),
            }
        }
        let cmd = parse_hangar(&["ainb", "hangar", "autopilot", "run", "ap-1"]);
        let HangarCommand::Autopilot(AutopilotCommand::Run(a)) = cmd else {
            panic!("expected autopilot run, got {cmd:?}");
        };
        assert_eq!(a.id, "ap-1");
        assert_eq!(
            a.source,
            RunSourceArg::Manual,
            "an unqualified `run` is the operator's MANUAL fire"
        );
    }

    /// The `api`-trigger verbs parse (migration 0057 / parity item 15): the
    /// arm/disarm toggle, the `--source api` fire, and the run-history read.
    #[test]
    fn parses_autopilot_api_trigger_verbs() {
        let cmd = parse_hangar(&["ainb", "hangar", "autopilot", "api-trigger", "ap-1"]);
        let HangarCommand::Autopilot(AutopilotCommand::ApiTrigger(a)) = cmd else {
            panic!("expected autopilot api-trigger, got {cmd:?}");
        };
        assert_eq!(a.id, "ap-1");
        assert!(!a.disable, "the bare verb ARMS the trigger");

        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "autopilot",
            "api-trigger",
            "ap-1",
            "--disable",
        ]);
        let HangarCommand::Autopilot(AutopilotCommand::ApiTrigger(a)) = cmd else {
            panic!("expected autopilot api-trigger, got {cmd:?}");
        };
        assert!(a.disable);

        let cmd = parse_hangar(&[
            "ainb",
            "hangar",
            "autopilot",
            "run",
            "ap-1",
            "--source",
            "api",
        ]);
        let HangarCommand::Autopilot(AutopilotCommand::Run(a)) = cmd else {
            panic!("expected autopilot run, got {cmd:?}");
        };
        assert_eq!(a.source, RunSourceArg::Api);

        let cmd = parse_hangar(&["ainb", "hangar", "autopilot", "runs", "ap-1"]);
        let HangarCommand::Autopilot(AutopilotCommand::Runs(a)) = cmd else {
            panic!("expected autopilot runs, got {cmd:?}");
        };
        assert_eq!(a.id, "ap-1");
        assert_eq!(a.limit, 20, "the history read defaults to a bounded window");
    }

    /// The list surface makes an armed api trigger visible, and the JSON
    /// surface carries the flag.
    #[test]
    fn autopilot_renderers_surface_the_api_trigger() {
        let mut ap = sample_autopilot(true);
        assert!(
            !autopilot_line(&ap, None).contains("[api]"),
            "an unarmed trigger shows no badge"
        );
        assert!(autopilot_to_json(&ap, None).contains("\"api_trigger_enabled\":false"));

        ap.api_trigger_enabled = true;
        assert!(
            autopilot_line(&ap, None).contains("[api]"),
            "an armed api trigger must be visible: {}",
            autopilot_line(&ap, None)
        );
        assert!(autopilot_to_json(&ap, None).contains("\"api_trigger_enabled\":true"));
    }

    /// The run-history renderers carry the trigger source and the admission
    /// reason — without them a recorded skip is unreadable from the CLI.
    #[test]
    fn autopilot_run_renderers_carry_source_and_reason() {
        let run = ainb_hangar_store::repo::autopilot::AutopilotRun {
            id: "01RUN".into(),
            autopilot_id: "01AP".into(),
            started_at: 1_767_258_000_000,
            completed_at: Some(1_767_258_000_000),
            status: "skipped".into(),
            source: "api".into(),
            failure_reason: Some("concurrency limit: 1/1 in flight".into()),
            ..Default::default()
        };
        let json = autopilot_run_to_json(&run);
        assert!(json.contains("\"status\":\"skipped\""), "{json}");
        assert!(json.contains("\"source\":\"api\""), "{json}");
        assert!(json.contains("concurrency limit"), "{json}");
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
            execution_mode: ainb_hangar_store::repo::autopilot::ExecutionMode::RunOnly,
            concurrency_policy: ainb_hangar_store::repo::autopilot::ConcurrencyPolicy::Skip,
            next_tick_at: Some(1_767_258_000_000),
            enabled,
            api_trigger_enabled: false,
            access_mode: ainb_hangar_store::repo::autopilot::AccessMode::Open,
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

    /// The key is trimmed like the value is. `validate` trims the VALUE, so a
    /// padded key failing with "unknown config key" while the same padding on the
    /// value was silently accepted was pure asymmetry — and easy to hit from a
    /// shell quoting mistake.
    #[tokio::test]
    async fn config_get_and_set_trim_the_key() {
        use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");

        run_daemon_config_set(
            &store,
            DaemonConfigSetArgs {
                key: " autostandup.enabled ".to_string(),
                value: "true".to_string(),
            },
        )
        .await
        .expect("a padded key must be accepted, not reported unknown");
        assert_eq!(
            DaemonConfigRepo::get(store.pool(), "autostandup.enabled")
                .await
                .unwrap()
                .as_deref(),
            Some("true"),
            "the trimmed key is what gets written"
        );

        run_daemon_config_get(
            &store,
            DaemonConfigGetArgs {
                key: " autostandup.enabled".to_string(),
            },
            OutputFormat::Text,
        )
        .await
        .expect("a padded key must resolve on get too");
    }

    // ---- ORIGIN PROVENANCE (0056, multica parity #21) ----------------------

    /// The precedence rule: flags win, else the daemon-injected env pair, else
    /// `manual`. An explicit `--origin-type` suppresses the env pair ENTIRELY, so
    /// a flag kind is never married to an inherited env id.
    #[test]
    fn cli_origin_precedence_is_flags_then_env_then_manual() {
        // Neither: manual.
        let o = resolve_cli_origin(None, None, None, None).unwrap();
        assert_eq!(o.kind_db_str(), "manual");
        assert_eq!(o.id(), None);

        // Env only: the daemon's injection is the default (the mention chain).
        let o = resolve_cli_origin(None, None, Some("comment_mention"), Some("c-1")).unwrap();
        assert_eq!(o.kind_db_str(), "comment_mention");
        assert_eq!(o.id(), Some("c-1"));

        // Flags beat env, and the env ID does NOT leak into a flag kind.
        let o =
            resolve_cli_origin(Some("manual"), None, Some("comment_mention"), Some("c-1")).unwrap();
        assert_eq!(o.kind_db_str(), "manual");
        assert_eq!(
            o.id(),
            None,
            "an explicit flag kind never inherits the env id"
        );
    }

    /// A bad origin is a hard error at resolve time, with the same wording the
    /// RPC handler produces.
    #[test]
    fn cli_origin_rejects_a_bad_pair() {
        let err = resolve_cli_origin(Some("quick_create"), Some("x"), None, None).unwrap_err();
        assert!(err.to_string().contains("unsupported origin_type"));

        let err = resolve_cli_origin(None, Some("c-1"), None, None).unwrap_err();
        assert!(err.to_string().contains("must be provided together"));

        let err = resolve_cli_origin(Some("autopilot"), None, None, None).unwrap_err();
        assert!(err.to_string().contains("requires an origin_id"));
    }

    /// End-to-end: `hangar issue create --origin-type … --origin-id …` stamps the
    /// pair onto the created row, and `issue show`'s read surface reads it back.
    #[tokio::test]
    async fn issue_create_stamps_the_authored_origin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");

        let HangarCommand::Issue(IssueCommand::Create(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Split this out",
            "--origin-type",
            "comment_mention",
            "--origin-id",
            "c-42",
        ]) else {
            panic!("expected issue create");
        };
        run_issue_create(&store, args).await.expect("create issue");

        let (kind, id): (String, Option<String>) =
            sqlx::query_as("SELECT origin_type, origin_id FROM issue LIMIT 1")
                .fetch_one(store.pool())
                .await
                .expect("read origin");
        assert_eq!(kind, "comment_mention");
        assert_eq!(id.as_deref(), Some("c-42"));
    }

    /// A create with no origin flags (and no daemon env) is stamped `manual` —
    /// never left NULL, so `origin_type IS NULL` keeps meaning exactly one thing:
    /// "created before provenance existed".
    #[tokio::test]
    async fn issue_create_without_flags_stamps_manual() {
        // The env pair is a DAEMON injection into a dispatched agent child; a test
        // process never has it, and mutating process env from a test is unsound
        // (and lint-denied here), so this asserts the plain no-flag-no-env path.
        // `cli_origin_precedence_is_flags_then_env_then_manual` covers the env
        // legs directly, with no process-global state.
        assert!(
            std::env::var("HANGAR_ORIGIN_TYPE").is_err(),
            "a test process must not inherit the daemon's origin injection"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        let HangarCommand::Issue(IssueCommand::Create(args)) =
            parse_hangar(&["ainb", "hangar", "issue", "create", "--title", "By hand"])
        else {
            panic!("expected issue create");
        };
        run_issue_create(&store, args).await.expect("create issue");

        let (kind, id): (String, Option<String>) =
            sqlx::query_as("SELECT origin_type, origin_id FROM issue LIMIT 1")
                .fetch_one(store.pool())
                .await
                .expect("read origin");
        assert_eq!(kind, "manual");
        assert_eq!(id, None);
    }

    /// A bogus `--origin-type` fails BEFORE any write: the issue table is still
    /// empty afterwards (the resolve is deliberately ahead of the insert, like
    /// the assignee / parent resolves).
    #[tokio::test]
    async fn issue_create_with_a_bad_origin_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");

        let HangarCommand::Issue(IssueCommand::Create(args)) = parse_hangar(&[
            "ainb",
            "hangar",
            "issue",
            "create",
            "--title",
            "Nope",
            "--origin-type",
            "bogus",
            "--origin-id",
            "x",
        ]) else {
            panic!("expected issue create");
        };
        let err = run_issue_create(&store, args).await.unwrap_err();
        assert!(err.to_string().contains("unsupported origin_type"));

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM issue")
            .fetch_one(store.pool())
            .await
            .expect("count issues");
        assert_eq!(count, 0, "a bad origin must fail ahead of the insert");
    }

    /// End-to-end through the parsed command: `hangar autopilot collaborator add`
    /// PERSISTS a row, read back with raw SQL after the command returns
    /// (multica parity #27).
    #[tokio::test]
    async fn autopilot_collaborator_add_persists_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        for sql in [
            "INSERT INTO workspace (id, slug, name, created_at) \
             VALUES ('ws-1','default','Default',0)",
            "INSERT INTO user (id, email, created_at) VALUES ('u-amy','amy@x.io',0)",
            "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
             VALUES ('rt-1','ws-1','d-1','claude','local')",
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES ('ag-1','ws-1','builder','rt-1','workspace','u-amy')",
            "INSERT INTO autopilot \
             (id, workspace_id, agent_id, name, cron_expr, max_concurrent_runs, execution_mode, \
              concurrency_policy, next_tick_at, enabled, api_trigger_enabled, created_at) \
             VALUES ('ap-1','ws-1','ag-1','nightly','0 3 * * *',1,'run_only','skip', \
                     999999999999,1,0,0)",
        ] {
            sqlx::query(sql).execute(store.pool()).await.expect(sql);
        }

        let HangarCommand::Autopilot(AutopilotCommand::Collaborator(cmd)) = parse_hangar(&[
            "ainb",
            "hangar",
            "autopilot",
            "collaborator",
            "add",
            "--id",
            "ap-1",
            "--actor",
            "member:bob",
            "--role",
            "editor",
        ]) else {
            panic!("expected autopilot collaborator add");
        };
        run_autopilot_collaborator(&store, cmd, OutputFormat::Text)
            .await
            .expect("add collaborator");

        let (actor_type, actor_id, role): (String, String, String) = sqlx::query_as(
            "SELECT actor_type, actor_id, role FROM autopilot_collaborator \
             WHERE autopilot_id = 'ap-1'",
        )
        .fetch_one(store.pool())
        .await
        .expect("read the persisted grant");
        assert_eq!(actor_type, "member");
        assert_eq!(actor_id, "bob");
        assert_eq!(role, "editor");
    }

    /// The CLI honours the restricted-mode gate too — otherwise it would be a
    /// trivially open back door around the daemon's request seam.
    #[tokio::test]
    async fn autopilot_collaborator_add_is_denied_on_a_restricted_rule() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open_in(dir.path()).await.expect("open store");
        for sql in [
            "INSERT INTO workspace (id, slug, name, created_at) \
             VALUES ('ws-1','default','Default',0)",
            "INSERT INTO user (id, email, created_at) VALUES ('u-amy','amy@x.io',0)",
            "INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','u-amy','member')",
            "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
             VALUES ('rt-1','ws-1','d-1','claude','local')",
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES ('ag-1','ws-1','builder','rt-1','workspace','u-amy')",
            "INSERT INTO autopilot \
             (id, workspace_id, agent_id, name, cron_expr, max_concurrent_runs, execution_mode, \
              concurrency_policy, next_tick_at, enabled, api_trigger_enabled, access_mode, \
              created_at) \
             VALUES ('ap-1','ws-1','ag-1','nightly','0 3 * * *',1,'run_only','skip', \
                     999999999999,1,0,'restricted',0)",
        ] {
            sqlx::query(sql).execute(store.pool()).await.expect(sql);
        }

        let HangarCommand::Autopilot(AutopilotCommand::Collaborator(cmd)) = parse_hangar(&[
            "ainb",
            "hangar",
            "autopilot",
            "collaborator",
            "add",
            "--id",
            "ap-1",
            "--actor",
            "member:bob",
            "--as-user",
            "u-amy",
        ]) else {
            panic!("expected autopilot collaborator add");
        };
        let err = run_autopilot_collaborator(&store, cmd, OutputFormat::Text)
            .await
            .expect_err("a plain member may not grant on a restricted rule");
        assert!(
            err.to_string().contains("access_mode = restricted"),
            "got {err}"
        );

        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM autopilot_collaborator")
            .fetch_one(store.pool())
            .await
            .expect("count grants");
        assert_eq!(count, 0, "a denied grant writes nothing");
    }
}
