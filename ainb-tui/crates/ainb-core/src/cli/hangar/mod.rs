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
    }
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
}
