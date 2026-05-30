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
use ainb_hangar_store::repo::issue::{Issue, IssueRepo, NewIssue};
use ainb_hangar_store::repo::task::{Task, TaskRepo};
use ainb_hangar_store::repo::token::{mint_daemon_token, mint_pat, PatRecord, PatRepo};
use ainb_hangar_store::service::cancel::CancelTaskService;
use ainb_hangar_store::service::retry::{RetryDecision, RetryService};
use ainb_hangar_store::Store;

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
}

/// `hangar config <noun>`.
#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Manage the provider-subprocess env allowlist.
    #[command(name = "env.allow", subcommand)]
    EnvAllow(EnvAllowCommand),
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
    }
}

/// Dispatch the `hangar config` verbs (synchronous — pure file IO, no database).
fn dispatch_config(cmd: ConfigCommand, format: OutputFormat) -> Result<()> {
    match cmd {
        ConfigCommand::EnvAllow(EnvAllowCommand::List) => run_env_allow_list(format),
        ConfigCommand::EnvAllow(EnvAllowCommand::Add(args)) => run_env_allow_add(args),
        ConfigCommand::EnvAllow(EnvAllowCommand::Remove(args)) => run_env_allow_remove(args),
    }
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
    let mut cfg = ainb_hangar_daemon::dispatch::load_allow_at(&path).context("load env.allow.toml")?;
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
    let mut cfg = ainb_hangar_daemon::dispatch::load_allow_at(&path).context("load env.allow.toml")?;
    if cfg.allow.remove(&args.key) {
        ainb_hangar_daemon::dispatch::save_allow_at(&path, &cfg).context("save env.allow.toml")?;
        println!("removed {} from the env allowlist", args.key);
    } else {
        println!("{} was not on the env allowlist (nothing removed)", args.key);
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
        AuthCommand::Token(TokenCommand::Create(args)) => {
            run_token_create(&store, args).await
        }
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
    }
}

/// `hangar issue create`: bootstrap a workspace if absent, then insert.
async fn run_issue_create(store: &Store, args: IssueCreateArgs) -> Result<()> {
    let pool = store.pool();
    let workspace_id = ensure_default_workspace(store).await?;
    let idgen = SystemIdGen;
    let clock = SystemClock;
    let id = idgen.new_ulid();
    let creator = ActorRef::new(ActorKind::Member, DEFAULT_CREATOR_ID)
        .expect("default creator id is non-empty");
    let new = NewIssue {
        id: id.clone(),
        workspace_id,
        title: args.title,
        description: args.description,
        state: args.state,
        assignee: None,
        creator,
        created_at: ainb_hangar_core::clock::HangarClock::now_ms(&clock),
    };
    IssueRepo::insert(pool, &new).await.context("insert issue")?;
    println!("created issue {id}");
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
            println!("task {} not retried (non-retryable or attempts exhausted)", args.id);
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
    let id: Option<String> =
        sqlx::query_scalar("SELECT id FROM user ORDER BY created_at LIMIT 1")
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
    let scope = t
        .scope
        .as_deref()
        .map_or_else(|| "null".to_string(), json_string);
    let last_used = t
        .last_used
        .map_or_else(|| "null".to_string(), |v| v.to_string());
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

/// One-line text summary of an issue.
fn issue_line(i: &Issue) -> String {
    format!("{}  [{}]  {}", i.id, i.state, i.title)
}

/// Minimal stable JSON object for one issue (hand-rolled to avoid pulling a
/// serde derive onto the store's `Issue` type from this crate).
fn issue_to_json(i: &Issue) -> String {
    let desc = i
        .description
        .as_deref()
        .map_or_else(|| "null".to_string(), json_string);
    format!(
        "{{\"id\":{},\"workspace_id\":{},\"title\":{},\"description\":{},\"state\":{},\"created_at\":{}}}",
        json_string(&i.id),
        json_string(&i.workspace_id),
        json_string(&i.title),
        desc,
        json_string(&i.state),
        i.created_at,
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
    "id,state,title,description,created_at"
}
fn issue_csv_row(i: &Issue) -> String {
    format!(
        "{},{},{},{},{}",
        csv_field(&i.id),
        csv_field(&i.state),
        csv_field(&i.title),
        csv_field(i.description.as_deref().unwrap_or("")),
        i.created_at,
    )
}
const fn issue_md_header() -> &'static str {
    "| id | state | title | description |\n| --- | --- | --- | --- |\n"
}
fn issue_md_row(i: &Issue) -> String {
    format!(
        "| {} | {} | {} | {} |",
        md_cell(&i.id),
        md_cell(&i.state),
        md_cell(&i.title),
        md_cell(i.description.as_deref().unwrap_or("")),
    )
}

/// One-line text summary of a task.
fn task_line(t: &Task) -> String {
    format!(
        "{}  [{}]  runtime={} agent={}",
        t.id, t.status, t.runtime_id, t.agent_id
    )
}
const fn task_csv_header() -> &'static str {
    "id,status,runtime_id,agent_id,attempt,max_attempts"
}
fn task_csv_row(t: &Task) -> String {
    format!(
        "{},{},{},{},{},{}",
        csv_field(&t.id),
        csv_field(&t.status),
        csv_field(&t.runtime_id),
        csv_field(&t.agent_id),
        t.attempt,
        t.max_attempts,
    )
}
const fn task_md_header() -> &'static str {
    "| id | status | runtime | agent | attempt |\n| --- | --- | --- | --- | --- |\n"
}
fn task_md_row(t: &Task) -> String {
    format!(
        "| {} | {} | {} | {} | {}/{} |",
        md_cell(&t.id),
        md_cell(&t.status),
        md_cell(&t.runtime_id),
        md_cell(&t.agent_id),
        t.attempt,
        t.max_attempts,
    )
}

/// Minimal stable JSON object for one task.
fn task_to_json(t: &Task) -> String {
    format!(
        "{{\"id\":{},\"status\":{},\"runtime_id\":{},\"agent_id\":{},\"attempt\":{},\"max_attempts\":{}}}",
        json_string(&t.id),
        json_string(&t.status),
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
            "ainb", "hangar", "issue", "create", "--title", "Fix bug", "--description", "details",
        ]);
        let HangarCommand::Issue(IssueCommand::Create(args)) = cmd else {
            panic!("expected issue create, got {cmd:?}");
        };
        assert_eq!(args.title, "Fix bug");
        assert_eq!(args.description.as_deref(), Some("details"));
        assert_eq!(args.state, DEFAULT_ISSUE_STATE);
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
            "ainb", "hangar", "beads", "reconcile", "--dry-run", "--label", "foo", "--label",
            "bar", "--json",
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
        assert!(err.is_err(), "bare `ainb hangar` must error (subcommand_required)");
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
        assert!(out.contains("ainb_SECRETBODY"), "plaintext must be printed once");
        assert_eq!(out.matches("ainb_SECRETBODY").count(), 1, "exactly once");
        assert!(
            out.to_lowercase().contains("once") && out.to_lowercase().contains("not recoverable"),
            "must warn there is no second chance: {out}"
        );
    }

    #[test]
    fn token_create_output_echoes_advisory_name() {
        let out = token_create_output("pat-1", Some("ci-bot"), "ainb_SECRETBODY");
        assert!(out.contains("ci-bot"), "advisory --name must be echoed: {out}");
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
}
