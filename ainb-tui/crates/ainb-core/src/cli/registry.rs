//! Command registry — replaces the `Commands` enum + `match` in `main.rs`.
//!
//! ## Design
//!
//! * [`CliCommand`] — trait every built-in subcommand implements. Plugins will
//!   register external implementations once Phase 4 wires the plugin loader
//!   into ainb-core. The trait stays object-safe by returning a `BoxFuture`
//!   from `run` instead of an `async fn`.
//! * [`CommandRegistry`] — owns the list of command impls; builds the root
//!   `clap::Command` by folding each impl's `build()` over a base, and
//!   dispatches an `ArgMatches` to the right impl by subcommand name.
//! * [`CliContext`] — the cross-cutting state every command needs. Phase 2b
//!   carries `format` (the global `--format`); later phases can hang config
//!   handles, telemetry sinks, etc. off it without touching command signatures.
//!
//! ## Hybrid clap derive + builder
//!
//! Argument shapes (`RunArgs`, `ListArgs`, the nested `*Commands` enums) keep
//! their `clap::Args` / `clap::Subcommand` derives. The registry uses the
//! derive-generated `augment_args` / `augment_subcommands` helpers to bolt
//! each shape onto a builder-side `clap::Command`. We pay zero migration cost
//! on the 2,500+ lines of arg definitions while still getting plugin
//! extensibility — a plugin's `CliCommand::build` is just another fold step.
//!
//! ## Why no global `Registry<T>` trait yet
//!
//! Phase 2a is landing a `ScreenRegistry` on a separate branch. Both registries
//! follow the same shape (build → dispatch) but the call signatures diverge
//! (Screen renders into a Buffer, Cli command runs an async I/O task). Lifting
//! a `trait Registry<Item>` is best done after both concrete registries land
//! and we can see the actual common surface — premature abstraction now would
//! bake in mismatches.

use std::pin::Pin;

use anyhow::{Context, Result};
use clap::{ArgMatches, Command, FromArgMatches, Subcommand};
use futures_util::future::BoxFuture;

use crate::cli::OutputFormat;

/// Cross-cutting state every command needs.
///
/// Phase 2b only carries `format`; later phases will bolt on a config handle,
/// telemetry sinks, etc.
#[derive(Debug, Clone, Copy)]
pub struct CliContext {
    pub format: OutputFormat,
}

/// Object-safe trait every built-in (and eventually plugin-supplied) subcommand
/// implements. See module docs for the contract.
pub trait CliCommand: Send + Sync {
    /// Subcommand name (e.g. "run", "list"). Must be unique within a registry.
    fn name(&self) -> &'static str;

    /// Augment the root `clap::Command` with this subcommand's surface. Most
    /// impls call `Args::augment_args` or `Subcommand::augment_subcommands` on
    /// a clap-derive type to keep the existing argument definitions.
    fn build(&self, app: Command) -> Command;

    /// Dispatch this subcommand. The returned future is `'static` because the
    /// impl is expected to extract its args from `matches` synchronously
    /// (cheap, infallible after clap parses) and move owned values into the
    /// async block.
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>>;
}

/// Holds the in-process list of command impls.
pub struct CommandRegistry {
    entries: Vec<Box<dyn CliCommand>>,
}

impl CommandRegistry {
    /// Empty registry. Useful for tests.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Registry pre-populated with every built-in CLI command. Order here is
    /// the order help text lists them — keep it stable.
    #[must_use]
    pub fn built_ins() -> Self {
        let mut r = Self::new();
        r.register(RunCommand);
        r.register(ListCommand);
        r.register(LogsCommand);
        r.register(AttachCommand);
        r.register(StatusCommand);
        r.register(KillCommand);
        r.register(AuthCommand);
        r.register(RecoverCommand);
        r.register(ConfigCommand);
        r.register(GitCommand);
        r.register(FavoritesCommand);
        r.register(InitCommand);
        r.register(DoctorCommand);
        r.register(ReflectCommand);
        r.register(PresetsCommand);
        r.register(UsageCommand);
        r.register(StatuslineCommand);
        r.register(ClaudecodeCommand);
        r.register(TmuxCommand);
        r.register(CompletionCommand);
        r.register(PluginCommand); // Phase 4 stub — surface reserved now
        r.register(FleetCommand);
        r.register(NotifydCommand); // hidden — ainb-hooks daemon alias
        r
    }

    pub fn register<C: CliCommand + 'static>(&mut self, cmd: C) {
        self.entries.push(Box::new(cmd));
    }

    /// All registered subcommand names, in registration order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.entries.iter().map(|c| c.name()).collect()
    }

    /// Number of registered commands.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn find(&self, name: &str) -> Option<&dyn CliCommand> {
        self.entries.iter().find(|c| c.name() == name).map(|c| &**c as &dyn CliCommand)
    }

    /// Build the root `clap::Command` by folding every registered impl's
    /// `build` over `base`. The caller seeds `base` with `clap::Command::new("ainb")`
    /// + global args (`--format`).
    pub fn build_clap(&self, base: Command) -> Command {
        self.entries.iter().fold(base, |acc, c| c.build(acc))
    }

    /// Dispatch a parsed `ArgMatches`. Callers should pull
    /// `matches.subcommand()` and pass the inner `ArgMatches` here. Returns
    /// `Err` for unknown names so the caller can surface a clap-style error
    /// (clap normally rejects unknown subcommands at parse time, but we keep
    /// a fallback for the runtime path).
    pub async fn dispatch(&self, name: &str, matches: &ArgMatches, ctx: CliContext) -> Result<()> {
        let cmd = self.find(name).with_context(|| format!("unknown subcommand: {name}"))?;
        cmd.run(matches, ctx).await
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Built-in command impls. Each is a unit struct so registration is a single
// `Box::new(...)` and there's no per-call allocation overhead.
// ──────────────────────────────────────────────────────────────────────────

fn boxed_err(e: clap::Error) -> Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    Box::pin(async move { Err(anyhow::Error::from(e)) })
}

pub struct RunCommand;
impl CliCommand for RunCommand {
    fn name(&self) -> &'static str {
        "run"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(<crate::cli::RunArgs as clap::Args>::augment_args(
            Command::new(self.name()).about("Spawn a new AI coding session"),
        ))
    }
    fn run(&self, matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::RunArgs::from_arg_matches(matches) {
            Ok(args) => Box::pin(async move { crate::cli::run::execute(args).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct ListCommand;
impl CliCommand for ListCommand {
    fn name(&self) -> &'static str {
        "list"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(<crate::cli::ListArgs as clap::Args>::augment_args(
            Command::new(self.name()).about("List all sessions"),
        ))
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::ListArgs::from_arg_matches(matches) {
            Ok(args) => Box::pin(async move { crate::cli::list::execute(args, ctx.format).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct LogsCommand;
impl CliCommand for LogsCommand {
    fn name(&self) -> &'static str {
        "logs"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(<crate::cli::LogsArgs as clap::Args>::augment_args(
            Command::new(self.name()).about("View session output/logs"),
        ))
    }
    fn run(&self, matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::LogsArgs::from_arg_matches(matches) {
            Ok(args) => Box::pin(async move { crate::cli::logs::execute(args).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct AttachCommand;
impl CliCommand for AttachCommand {
    fn name(&self) -> &'static str {
        "attach"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(<crate::cli::AttachArgs as clap::Args>::augment_args(
            Command::new(self.name()).about("Attach to a session (drops into tmux)"),
        ))
    }
    fn run(&self, matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::AttachArgs::from_arg_matches(matches) {
            Ok(args) => Box::pin(async move { crate::cli::attach::execute(args).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct StatusCommand;
impl CliCommand for StatusCommand {
    fn name(&self) -> &'static str {
        "status"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(<crate::cli::StatusArgs as clap::Args>::augment_args(
            Command::new(self.name()).about("Check session status"),
        ))
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::StatusArgs::from_arg_matches(matches) {
            Ok(args) => {
                Box::pin(async move { crate::cli::status::execute(args, ctx.format).await })
            }
            Err(e) => boxed_err(e),
        }
    }
}

pub struct KillCommand;
impl CliCommand for KillCommand {
    fn name(&self) -> &'static str {
        "kill"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(<crate::cli::KillArgs as clap::Args>::augment_args(
            Command::new(self.name()).about("Kill a session"),
        ))
    }
    fn run(&self, matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::KillArgs::from_arg_matches(matches) {
            Ok(args) => Box::pin(async move { crate::cli::status::kill(args).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct AuthCommand;
impl CliCommand for AuthCommand {
    fn name(&self) -> &'static str {
        "auth"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(Command::new(self.name()).about("Set up authentication"))
    }
    fn run(&self, _matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        Box::pin(async move { crate::cli::auth::run_auth_setup().await })
    }
}

pub struct RecoverCommand;
impl CliCommand for RecoverCommand {
    fn name(&self) -> &'static str {
        "recover"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            <crate::cli::recover::RecoverCommands as Subcommand>::augment_subcommands(
                Command::new(self.name())
                    .about("Recover orphaned or crashed sessions")
                    .subcommand_required(true),
            ),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::recover::RecoverCommands::from_arg_matches(matches) {
            Ok(c) => Box::pin(async move { crate::cli::recover::execute(c, ctx.format).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct ConfigCommand;
impl CliCommand for ConfigCommand {
    fn name(&self) -> &'static str {
        "config"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            <crate::cli::config_cmd::ConfigCommands as Subcommand>::augment_subcommands(
                Command::new(self.name())
                    .about("Manage configuration")
                    .subcommand_required(true),
            ),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::config_cmd::ConfigCommands::from_arg_matches(matches) {
            Ok(c) => Box::pin(async move { crate::cli::config_cmd::execute(c, ctx.format).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct GitCommand;
impl CliCommand for GitCommand {
    fn name(&self) -> &'static str {
        "git"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            <crate::cli::git_cmd::GitCommands as Subcommand>::augment_subcommands(
                Command::new(self.name())
                    .about("Git worktree operations")
                    .subcommand_required(true),
            ),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::git_cmd::GitCommands::from_arg_matches(matches) {
            Ok(c) => Box::pin(async move { crate::cli::git_cmd::execute(c, ctx.format).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct FavoritesCommand;
impl CliCommand for FavoritesCommand {
    fn name(&self) -> &'static str {
        "favorites"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            <crate::cli::favorites::FavoritesCommands as Subcommand>::augment_subcommands(
                Command::new(self.name())
                    .about("Manage favorite repositories")
                    .subcommand_required(true),
            ),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::favorites::FavoritesCommands::from_arg_matches(matches) {
            Ok(c) => Box::pin(async move { crate::cli::favorites::execute(c, ctx.format).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct InitCommand;
impl CliCommand for InitCommand {
    fn name(&self) -> &'static str {
        "init"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(<crate::cli::init::InitArgs as clap::Args>::augment_args(
            Command::new(self.name()).about("First-time setup and prerequisite checking"),
        ))
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::init::InitArgs::from_arg_matches(matches) {
            Ok(args) => Box::pin(async move { crate::cli::init::execute(args, ctx.format).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct DoctorCommand;
impl CliCommand for DoctorCommand {
    fn name(&self) -> &'static str {
        "doctor"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            <crate::cli::doctor::DoctorArgs as clap::Args>::augment_args(
                Command::new(self.name())
                    .about("Classified dependency check for the reflect / statusline toolchain"),
            ),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::doctor::DoctorArgs::from_arg_matches(matches) {
            Ok(args) => {
                Box::pin(async move { crate::cli::doctor::execute(args, ctx.format).await })
            }
            Err(e) => boxed_err(e),
        }
    }
}

pub struct ReflectCommand;
impl CliCommand for ReflectCommand {
    fn name(&self) -> &'static str {
        "reflect"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            <crate::cli::reflect::ReflectCommands as Subcommand>::augment_subcommands(
                Command::new(self.name())
                    .about("Reflect plugin lifecycle: bootstrap installer + dependency check")
                    .subcommand_required(true),
            ),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::reflect::ReflectCommands::from_arg_matches(matches) {
            Ok(c) => Box::pin(async move { crate::cli::reflect::execute(c, ctx.format).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct PresetsCommand;
impl CliCommand for PresetsCommand {
    fn name(&self) -> &'static str {
        "presets"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            <crate::cli::presets::PresetsCommands as Subcommand>::augment_subcommands(
                Command::new(self.name())
                    .about("Manage session presets")
                    .subcommand_required(true),
            ),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match crate::cli::presets::PresetsCommands::from_arg_matches(matches) {
            Ok(c) => Box::pin(async move { crate::cli::presets::execute(c, ctx.format).await }),
            Err(e) => boxed_err(e),
        }
    }
}

pub struct UsageCommand;
impl CliCommand for UsageCommand {
    fn name(&self) -> &'static str {
        "usage"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            <crate::cli::usage::UsageCommands as Subcommand>::augment_subcommands(
                Command::new(self.name())
                    .about("Usage analytics, reports, export, and optimization")
                    .subcommand_required(true),
            ),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        // Phase 6c-cli: host has no UsageData of its own. Plan, Currency,
        // and Cache subcommands are config admin (still in-tree). The
        // remaining 9 subcommands dispatch to the burndown plugin, which
        // synchronously fetches the snapshot from session-reader via
        // `ainb_request_data`.
        //
        // We rebuild argv from `std::env::args()` rather than `matches`
        // (clap-derive doesn't have a `to_args`); clap has already
        // validated the args before dispatch lands here.
        //
        // Pre-pend `--format <value>` so the plugin sees the host's
        // global `--format` flag — clap-derive strips global args from
        // the residual argv before the subcommand sees them, which is
        // why `ainb --format json usage report` would otherwise reach
        // the plugin as `report` (no format flag) and render text.
        // Subcommand-position `--format` (e.g. `ainb usage report
        // --format json`) is preserved verbatim by `std::env::args`
        // and the plugin's `extract_format` picks the last occurrence.
        let format = ctx.format;
        let format_token = output_format_to_token(format);
        let mut argv: Vec<String> = vec!["--format".to_string(), format_token.to_string()];
        argv.extend(std::env::args().skip_while(|a| a != "usage").skip(1));
        let parsed = crate::cli::usage::UsageCommands::from_arg_matches(matches);
        Box::pin(async move {
            let cmd = parsed.map_err(anyhow::Error::from)?;
            if is_host_admin_subcommand(&cmd) {
                return crate::cli::usage::execute(cmd, format).await;
            }
            dispatch_usage_via_plugin(argv).await;
            // dispatch_usage_via_plugin always exits the process on
            // both happy and failure paths (process::exit). Returning
            // unreachable Ok satisfies the trait's `Result<()>`.
            #[allow(unreachable_code)]
            Ok(())
        })
    }
}

/// Plan / Currency / Cache stay in-tree (config admin, not analytics).
/// Everything else routes through the burndown plugin.
fn is_host_admin_subcommand(cmd: &crate::cli::usage::UsageCommands) -> bool {
    use crate::cli::usage::UsageCommands::*;
    matches!(cmd, Plan { .. } | Currency(_) | Cache { .. })
}

/// Mirror of clap-derive's lowercased ValueEnum form so we can pass
/// the host's `OutputFormat` back over argv without re-parsing it
/// inside the plugin (the plugin reads `--format <token>` via its
/// own lightweight `extract_format`, which matches on these tokens
/// verbatim).
fn output_format_to_token(format: crate::cli::OutputFormat) -> &'static str {
    use crate::cli::OutputFormat::*;
    match format {
        Text => "text",
        Json => "json",
        Csv => "csv",
        Markdown => "markdown",
    }
}

/// `ainb usage` shim (Phase 7c).
///
/// Boots the subprocess plugin runtime, waits for session-reader to publish
/// the initial `sessions.usage_data` snapshot, dispatches the parsed argv
/// to the burndown plugin's `cli_dispatch` over JSON-RPC, prints captured
/// stdout/stderr, then exits with the plugin's reported exit code.
///
/// Always exits the process — never returns. Failure modes:
///
/// * No `dist/plugins/` discovered → exit 2 with the "install burndown"
///   hint that pre-7c callers already handle.
/// * Burndown plugin not registered → same exit-2 hint.
/// * Session-reader never publishes within
///   [`PLUGIN_DATA_WAIT`] → exit 1 with a "session-reader didn't publish"
///   message so the user can rerun with `RUST_LOG=debug` to see the scan
///   progress.
/// * Plugin returned a non-zero exit code → exit that code, after writing
///   its stdout/stderr verbatim.
///
/// Cleanup: the owning [`Runtime`] is dropped via `spawn_blocking` so the
/// tokio "Cannot drop a runtime in a context where blocking is not allowed"
/// panic from [`ainb plugin list`](crate::cli::plugin) doesn't bite us here.
async fn dispatch_usage_via_plugin(argv: Vec<String>) -> ! {
    let exit_code = match run_usage_via_plugin(argv).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    };
    std::process::exit(exit_code);
}

/// Default hard deadline for `ainb usage` to surface output. Generously
/// sized because the first scan of a large `~/.claude/projects` (100k+
/// calls, 50+ msgpack chunks at 2 MB/chunk) takes ~40 s on a cold cache
/// and must finish *before* burndown's `cli_dispatch` returns useful
/// data. Subsequent runs hit session-reader's sqlite cache and finish
/// in well under a second.
///
/// Override via the `AINB_USAGE_TIMEOUT_SECS` env var when running
/// against very large session archives.
const PLUGIN_DATA_WAIT_DEFAULT_SECS: u64 = 120;

fn plugin_data_wait() -> std::time::Duration {
    std::env::var("AINB_USAGE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or_else(|| std::time::Duration::from_secs(PLUGIN_DATA_WAIT_DEFAULT_SECS))
}

/// Pause between retry attempts when burndown still reports
/// "install session-reader" — long enough that we don't hot-loop
/// against the plugin's mutex while it is processing handle_event
/// chunks, short enough that we don't add visible tail latency on the
/// cached path (where the very first dispatch usually succeeds).
const PLUGIN_DATA_POLL: std::time::Duration = std::time::Duration::from_millis(250);

async fn run_usage_via_plugin(argv: Vec<String>) -> anyhow::Result<i32> {
    // Step 1 — bring up the runtime + discover staged plugins. Mirrors
    // `crate::cli::plugin::watch`, including the spawn_blocking drop
    // dance to avoid the tokio "Cannot drop a runtime in a context
    // where blocking is not allowed" panic on shutdown.
    let (runtime, handle, _outcome) = crate::plugins::init_plugin_runtime()
        .map_err(|e| anyhow::anyhow!("plugin runtime init failed: {e}"))?;

    let result = dispatch_inner(&handle, argv).await;

    // Drop the owning runtime off the async context.
    tokio::task::spawn_blocking(move || drop(runtime)).await.ok();

    result
}

/// Inner half of [`run_usage_via_plugin`] — does the real work given a
/// `RuntimeHandle`, leaving the runtime ownership / cleanup dance to the
/// caller. Split for testability and for the spawn_blocking shutdown
/// pattern.
///
/// Data-flow contract assumed here:
///
/// 1. Both `burndown` and `session-reader` are eager-spawned (see
///    `dist/plugins/burndown/manifest.toml`). The runtime's `discover`
///    has already sent each plugin task an `EnsureSpawned`, which
///    triggers `on_init` on the child process.
/// 2. burndown's `on_init` subscribes to `sessions.usage_data` *before*
///    publishing `sessions.refresh_request`, guaranteeing it catches
///    chunk-0 of session-reader's response. Re-publishing
///    `refresh_request` from this shim is therefore counter-productive
///    — each republish queues a *fresh* full rescan on session-reader,
///    and burndown's plugin task processes its inbox serially: the
///    `cli_dispatch` we are about to send would sit behind every queued
///    `handle_event` chunk, never reaching `cli_dispatch` before the
///    deadline.
/// 3. session-reader chunks the snapshot into ≥1 `UsageDataEvent`
///    publishes; only the chunk with `is_final = true` causes burndown
///    to populate `self.data`. burndown's plugin task processes its
///    inbox serially, so once `cli_dispatch` is queued *after* every
///    in-flight `handle_event`, it is guaranteed to see populated
///    `self.data` — except on the very first call where the inbox may
///    not yet have any chunks (snapshot not yet built). That call
///    returns the "install session-reader" hint; the retry loop below
///    re-queues `cli_dispatch` behind whatever chunks have accumulated
///    since.
async fn dispatch_inner(
    handle: &ainb_plugin_runtime::RuntimeHandle,
    argv: Vec<String>,
) -> anyhow::Result<i32> {
    use ainb_plugin_runtime::{CliOutcome, PluginId};

    let trace = std::env::var("AINB_USAGE_TRACE").is_ok();
    let burndown = PluginId::from("burndown");
    let registered = handle.registered_plugins();
    if trace {
        eprintln!(
            "[usage-cli] registered plugins: {:?}",
            registered.iter().map(|p| p.id.to_string()).collect::<Vec<_>>()
        );
    }
    if !registered.iter().any(|p| p.id == burndown) {
        anyhow::bail!(
            "error: usage analytics requires the burndown plugin \
             (install via 'ainb plugin install burndown')"
        );
    }

    // Retry contract: burndown reports `exit_code = 1` + empty stdout +
    // a stderr containing this exact byte sequence when `self.data` is
    // None. Any *other* exit/stdout/stderr shape (including a real
    // error from clap, a runtime fault, or a successful report with
    // exit 0) breaks out of the retry loop immediately so we don't
    // mask a genuine failure as "still waiting".
    let install_hint_marker = b"install session-reader";
    let wait_budget = plugin_data_wait();
    let deadline = std::time::Instant::now() + wait_budget;
    let started = std::time::Instant::now();
    let mut last_trace = started;
    let mut attempt: u32 = 0;
    let mut outcome;
    loop {
        attempt = attempt.wrapping_add(1);
        if trace && last_trace.elapsed() >= std::time::Duration::from_secs(2) {
            // Periodic heartbeat: include plugin lifecycle state so a
            // user running `AINB_USAGE_TRACE=1` can distinguish "scan
            // in progress" from "plugin crashed". Mirrors
            // `ainb plugin watch` semantics.
            for p in &registered {
                let state = handle.lifecycle_state(&p.id);
                eprintln!(
                    "[usage-cli] @{:.1}s attempt {attempt} plugin {} lifecycle={:?}",
                    started.elapsed().as_secs_f32(),
                    p.id,
                    state
                );
            }
            last_trace = std::time::Instant::now();
        }
        outcome = handle.dispatch_cli(&burndown, "usage", argv.clone()).await.map_err(|e| {
            anyhow::anyhow!("burndown plugin task disconnected before replying: {e}")
        })?;
        let should_retry = matches!(
            &outcome,
            CliOutcome::Ok(r)
                if r.exit_code == 1
                    && r.stdout.is_empty()
                    && r.stderr
                        .windows(install_hint_marker.len())
                        .any(|w| w == install_hint_marker)
        );
        if !should_retry {
            if trace {
                eprintln!(
                    "[usage-cli] dispatch returned in {:.1}s (attempt {attempt})",
                    started.elapsed().as_secs_f32()
                );
            }
            break;
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "error: session-reader plugin didn't publish usage data within {}s \
                 — rerun with RUST_LOG=ainb_plugin_runtime=debug,ainb_plugin_session_reader=debug \
                 to see scan progress, or raise the budget via AINB_USAGE_TIMEOUT_SECS=<n>",
                wait_budget.as_secs()
            );
        }
        if trace {
            eprintln!(
                "[usage-cli] attempt {attempt} returned 'install session-reader' \
                 (snapshot not yet finalised); retrying in {}ms",
                PLUGIN_DATA_POLL.as_millis()
            );
        }
        tokio::time::sleep(PLUGIN_DATA_POLL).await;
    }

    let exit = match outcome {
        CliOutcome::Ok(result) => {
            use std::io::Write;
            // Write raw bytes — the plugin owns the format (text /
            // json / csv per `--format`) and we must not re-encode.
            let _ = std::io::stdout().write_all(&result.stdout);
            let _ = std::io::stderr().write_all(&result.stderr);
            result.exit_code
        }
        CliOutcome::PluginError { code, message } => {
            eprintln!("error: burndown plugin error {code}: {message}");
            1
        }
        CliOutcome::RuntimeError(msg) => {
            eprintln!("error: plugin runtime: {msg}");
            1
        }
    };
    Ok(exit)
}

/// Legacy top-level `ainb statusline`. Kept (hidden) so existing
/// `~/.claude/settings.json` entries written before the
/// `claudecode` namespace existed keep working unchanged. New installs
/// (and `ainb init` migrations) write the canonical `ainb claudecode
/// statusline` form.
pub struct StatuslineCommand;
impl CliCommand for StatuslineCommand {
    fn name(&self) -> &'static str {
        "statusline"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            Command::new(self.name())
                .hide(true)
                .about(
                    "(Legacy alias) Claude Code statusline hook. Prefer \
                     `ainb claudecode statusline`. Kept so existing \
                     `~/.claude/settings.json` entries keep working unchanged.",
                )
                .arg(
                    clap::Arg::new("cache-only")
                        .long("cache-only")
                        .action(clap::ArgAction::SetTrue)
                        .help(
                            "Side-channel mode: write the cache only and emit nothing on \
                             stdout.",
                        ),
                ),
        )
    }
    fn run(&self, matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        let cache_only = matches.get_flag("cache-only");
        Box::pin(async move { crate::cli::statusline::execute(cache_only) })
    }
}

/// Canonical `ainb claudecode <subcmd>` provider-namespaced surface.
///
/// Today: only `statusline` (with optional `--cache-only`). Reserved for
/// other Claude Code-specific commands (doctor, config introspection)
/// without polluting the top level. Other providers (e.g. Codex) can
/// grow their own namespace.
pub struct ClaudecodeCommand;
impl CliCommand for ClaudecodeCommand {
    fn name(&self) -> &'static str {
        "claudecode"
    }
    fn build(&self, app: Command) -> Command {
        app.subcommand(
            Command::new(self.name())
                .about(
                    "Claude Code-specific commands (statusline, etc.). \
                     Provider-namespaced — other providers grow their own.",
                )
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(
                    Command::new("statusline")
                        .about(
                            "Claude Code statusline hook: read JSON on stdin, cache \
                             rate-limit windows for the TUI, and emit a powerline status \
                             string on stdout.",
                        )
                        .arg(
                            clap::Arg::new("cache-only")
                                .long("cache-only")
                                .action(clap::ArgAction::SetTrue)
                                .help(
                                    "Side-channel mode: write the cache only and emit \
                                     nothing on stdout.",
                                ),
                        ),
                ),
        )
    }
    fn run(&self, matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        let (sub_name, cache_only) = match matches.subcommand() {
            Some(("statusline", m)) => (Some("statusline".to_string()), m.get_flag("cache-only")),
            _ => (None, false),
        };
        Box::pin(async move {
            match sub_name.as_deref() {
                Some("statusline") => crate::cli::statusline::execute(cache_only),
                _ => Err(anyhow::anyhow!(
                    "claudecode requires a subcommand (e.g. `statusline`)"
                )),
            }
        })
    }
}

/// `ainb tmux {install,status}` — manage the bundled rich tmux.conf at
/// `~/.tmux.conf`. See `crate::cli::tmux_install` for the implementation.
pub struct TmuxCommand;
impl CliCommand for TmuxCommand {
    fn name(&self) -> &'static str {
        "tmux"
    }
    fn build(&self, app: Command) -> Command {
        let install = <crate::cli::tmux_install::InstallArgs as clap::Args>::augment_args(
            Command::new("install").about(
                "Install or upgrade the bundled rich tmux.conf to ~/.tmux.conf \
                 (backs up any existing file, shows a diff preview, then reloads \
                 live sessions).",
            ),
        );
        let status = <crate::cli::tmux_install::StatusArgs as clap::Args>::augment_args(
            Command::new("status").about(
                "Report whether ~/.tmux.conf is missing, up to date, or stale \
                 relative to the bundled rich conf.",
            ),
        );
        app.subcommand(
            Command::new(self.name())
                .about(
                    "Manage the rich tmux.conf shipped with ainb-tui \
                     (Catppuccin Mocha + TPM + resurrect/continuum/yank + \
                     discoverable detach hints).",
                )
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(install)
                .subcommand(status),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        match matches.subcommand() {
            Some(("install", m)) => {
                match crate::cli::tmux_install::InstallArgs::from_arg_matches(m) {
                    Ok(args) => Box::pin(async move {
                        crate::cli::tmux_install::install(args, ctx.format).await
                    }),
                    Err(e) => boxed_err(e),
                }
            }
            Some(("status", m)) => {
                match crate::cli::tmux_install::StatusArgs::from_arg_matches(m) {
                    Ok(args) => {
                        Box::pin(
                            async move { crate::cli::tmux_install::status(args, ctx.format).await },
                        )
                    }
                    Err(e) => boxed_err(e),
                }
            }
            _ => Box::pin(async move {
                Err(anyhow::anyhow!(
                    "tmux requires a subcommand: install | status"
                ))
            }),
        }
    }
}

/// `ainb notifyd {run,stop,install,uninstall,status}` — the ainb-hooks
/// notification daemon, exposed as a hidden subcommand of the main
/// binary.
///
/// The standalone `ainb-notifyd` binary remains the documented
/// entrypoint, but `notify.sh`'s lazy-spawn fires `ainb notifyd` (the
/// host binary is the one guaranteed to be on `PATH` after a normal
/// install). This subcommand makes that path real: it delegates to the
/// same `ainb_plugin_notifyd` functions the standalone binary uses, so
/// both entrypoints share one implementation. Hidden from `--help`
/// because end users should reach for `ainb-notifyd` or the TUI Inbox;
/// this exists for the hook script's auto-start.
pub struct NotifydCommand;
impl CliCommand for NotifydCommand {
    fn name(&self) -> &'static str {
        "notifyd"
    }
    fn build(&self, app: Command) -> Command {
        let agent_flags = |c: Command| {
            c.arg(
                clap::Arg::new("claude")
                    .long("claude")
                    .action(clap::ArgAction::SetTrue)
                    .help("Target Claude Code"),
            )
            .arg(
                clap::Arg::new("codex")
                    .long("codex")
                    .action(clap::ArgAction::SetTrue)
                    .help("Target Codex CLI"),
            )
            .arg(
                clap::Arg::new("all")
                    .long("all")
                    .action(clap::ArgAction::SetTrue)
                    .help("Target every known agent"),
            )
        };
        app.subcommand(
            Command::new(self.name())
                .about(
                    "ainb-hooks notification daemon (alias of the ainb-notifyd \
                     binary; used by notify.sh lazy-spawn).",
                )
                .hide(true)
                .subcommand(Command::new("run").about("Run the daemon in the foreground (default)"))
                .subcommand(Command::new("stop").about("Stop a running daemon via its PID file"))
                .subcommand(agent_flags(
                    Command::new("install").about("Install the ainb-hooks hook"),
                ))
                .subcommand(agent_flags(
                    Command::new("uninstall").about("Uninstall the ainb-hooks hook"),
                ))
                .subcommand(Command::new("status").about("Report install + daemon status")),
        )
    }
    fn run(&self, matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        use ainb_plugin_notifyd::cli;

        // The verb (and, for install/uninstall, the resolved agent set)
        // is computed synchronously here so only owned values cross into
        // the 'static future. Every body delegates to the shared
        // `ainb_plugin_notifyd::cli` functions — the same ones the
        // standalone `ainb-notifyd` binary calls — so the two
        // entrypoints can never diverge in behaviour or output.
        enum Verb {
            Run,
            Stop,
            Install(Vec<ainb_plugin_notifyd::Agent>),
            Uninstall(Vec<ainb_plugin_notifyd::Agent>),
            Status,
        }
        let agents = |m: &ArgMatches| {
            cli::agents_from_flags(m.get_flag("claude"), m.get_flag("codex"), m.get_flag("all"))
        };
        // No sub-verb → `run` (matches the standalone binary's default
        // and the lazy-spawn call `ainb notifyd`).
        let verb = match matches.subcommand() {
            Some(("run", _)) | None => Verb::Run,
            Some(("stop", _)) => Verb::Stop,
            Some(("install", m)) => Verb::Install(agents(m)),
            Some(("uninstall", m)) => Verb::Uninstall(agents(m)),
            Some(("status", _)) => Verb::Status,
            Some((other, _)) => {
                let other = other.to_string();
                return Box::pin(async move {
                    Err(anyhow::anyhow!("unknown notifyd subcommand: {other}"))
                });
            }
        };
        Box::pin(async move {
            match verb {
                Verb::Run => cli::cmd_run().await,
                Verb::Stop => cli::cmd_stop(),
                Verb::Install(a) => cli::cmd_install(&a),
                Verb::Uninstall(a) => cli::cmd_uninstall(&a),
                Verb::Status => cli::cmd_status(),
            }
        })
    }
}

pub struct CompletionCommand;
impl CliCommand for CompletionCommand {
    fn name(&self) -> &'static str {
        "completion"
    }
    fn build(&self, app: Command) -> Command {
        let shell_arg = clap::Arg::new("shell")
            .required(true)
            .value_parser(clap::builder::EnumValueParser::<clap_complete::Shell>::new())
            .help("Shell to generate completions for");
        app.subcommand(
            Command::new(self.name())
                .about("Generate shell completions (bash, zsh, fish, powershell, elvish)")
                .arg(shell_arg),
        )
    }
    fn run(&self, matches: &ArgMatches, _ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        let shell = *matches
            .get_one::<clap_complete::Shell>("shell")
            .expect("clap enforces required arg");
        Box::pin(async move {
            // Rebuild the clap app — completion generation needs the full surface
            // including all registered subcommands.
            let registry = CommandRegistry::built_ins();
            let mut app = crate::cli::root_clap_command();
            app = registry.build_clap(app);
            let name = app.get_name().to_string();
            clap_complete::generate(shell, &mut app, name, &mut std::io::stdout());
            Ok(())
        })
    }
}

/// `ainb plugin {marketplace,install,update,remove,list,search}` — Phase 4
/// marketplace + installer. Argument shapes nailed down in Phase 2b so plugin
/// authors could target them today; Phase 4 wires the real handlers in
/// `crate::cli::plugin`.
pub struct PluginCommand;
impl CliCommand for PluginCommand {
    fn name(&self) -> &'static str {
        "plugin"
    }
    fn build(&self, app: Command) -> Command {
        let install = Command::new("install")
            .about("Install a plugin from a marketplace")
            .arg(
                clap::Arg::new("plugin")
                    .required(true)
                    .help("plugin id, e.g. burndown or ainb-plugins/burndown@0.1.0"),
            )
            .arg(
                clap::Arg::new("yes")
                    .long("yes")
                    .short('y')
                    .action(clap::ArgAction::SetTrue)
                    .help("skip the capability approval prompt"),
            );
        let update = Command::new("update")
            .about("Update an installed plugin to the latest matching version")
            .arg(clap::Arg::new("plugin").required(true))
            .arg(
                clap::Arg::new("yes")
                    .long("yes")
                    .short('y')
                    .action(clap::ArgAction::SetTrue)
                    .help("skip prompts when new capabilities are requested"),
            );
        let remove_cmd = Command::new("remove")
            .about("Remove an installed plugin")
            .arg(clap::Arg::new("plugin").required(true))
            .arg(
                clap::Arg::new("yes")
                    .long("yes")
                    .short('y')
                    .action(clap::ArgAction::SetTrue)
                    .help("skip the data-directory deletion prompt"),
            );
        let list = Command::new("list").about("List installed plugins");
        let search = Command::new("search")
            .about("Search registered marketplaces by plugin name")
            .arg(clap::Arg::new("query").required(true));
        let marketplace = Command::new("marketplace")
            .about("Manage marketplace registries")
            .subcommand_required(true)
            .subcommand(
                Command::new("add")
                    .about("Register a marketplace by URL or local path")
                    .arg(clap::Arg::new("url").required(true)),
            )
            .subcommand(
                Command::new("remove")
                    .about("Unregister a marketplace by name")
                    .arg(clap::Arg::new("name").required(true)),
            )
            .subcommand(Command::new("list").about("List registered marketplaces"));
        // Phase 7d-cli — DevX subcommands for the subprocess plugin runtime.
        let lint_cmd = Command::new("lint")
            .about("Validate a plugin manifest + binary (ABI 2.0 sanity checks)")
            .arg(
                clap::Arg::new("plugin")
                    .required(true)
                    .help("plugin id, staging dir, or manifest.toml path"),
            );
        let watch_cmd = Command::new("watch")
            .about("Live-tail lifecycle + snapshot events for a registered plugin")
            .arg(
                clap::Arg::new("plugin")
                    .required(true)
                    .help("plugin id (matches `ainb plugin list`)"),
            )
            .arg(
                clap::Arg::new("duration")
                    .long("duration")
                    .value_parser(clap::value_parser!(u64))
                    .help("seconds to watch before exiting (default 30)"),
            );
        let tail_cmd = Command::new("tail")
            .about("Stream the host's tracing layer filtered to a single plugin id")
            .arg(
                clap::Arg::new("plugin")
                    .required(true)
                    .help("plugin id (matches `ainb plugin list`)"),
            )
            .arg(
                clap::Arg::new("level")
                    .long("level")
                    .help("min log level: trace|debug|info|warn|error (default debug)"),
            )
            .arg(
                clap::Arg::new("since")
                    .long("since")
                    .help("RFC-3339 timestamp; suppresses events older than this"),
            )
            .arg(
                clap::Arg::new("duration")
                    .long("duration")
                    .value_parser(clap::value_parser!(u64))
                    .help("seconds to tail before exiting (default 30)"),
            );
        app.subcommand(
            Command::new(self.name())
                .about("Manage ainb plugins")
                .subcommand_required(true)
                .subcommand(install)
                .subcommand(update)
                .subcommand(remove_cmd)
                .subcommand(list)
                .subcommand(search)
                .subcommand(marketplace)
                .subcommand(lint_cmd)
                .subcommand(watch_cmd)
                .subcommand(tail_cmd),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        let matches = matches.clone();
        Box::pin(async move { crate::cli::plugin::execute(&matches, ctx.format).await })
    }
}

pub struct FleetCommand;
impl CliCommand for FleetCommand {
    fn name(&self) -> &'static str {
        "fleet"
    }
    fn build(&self, app: Command) -> Command {
        let standup = Command::new("standup")
            .about("Live fleet status: every claude session across ainb + peers + bg jobs")
            .arg(
                clap::Arg::new("text")
                    .long("text")
                    .action(clap::ArgAction::SetTrue)
                    .help("Force text output even with --format json"),
            )
            .arg(
                clap::Arg::new("no-enrich")
                    .long("no-enrich")
                    .action(clap::ArgAction::SetTrue)
                    .help("Skip AI enrichment — 0-token output (env AINB_FLEET_ENRICH=0)"),
            );
        let broadcast = Command::new("broadcast")
            .about("Send one prompt to selected sessions (peers-first, tmux fallback)")
            .arg(clap::Arg::new("prompt").required(true))
            .arg(
                clap::Arg::new("all")
                    .long("all")
                    .action(clap::ArgAction::SetTrue)
                    .help("Fan out to every running session"),
            )
            .arg(
                clap::Arg::new("filter")
                    .long("filter")
                    .help("Regex against tmux/workspace name"),
            )
            .arg(clap::Arg::new("cwd").long("cwd").help("Substring against cwd"));
        let sequence = Command::new("sequence")
            .about("Ordered prompts with ack between steps")
            .arg(
                clap::Arg::new("steps")
                    .required(true)
                    .num_args(1..)
                    .action(clap::ArgAction::Append),
            )
            .arg(clap::Arg::new("all").long("all").action(clap::ArgAction::SetTrue))
            .arg(
                clap::Arg::new("timeout")
                    .long("timeout")
                    .value_parser(clap::value_parser!(u64))
                    .default_value("300")
                    .help("Per-step timeout (seconds)"),
            );
        let needs = Command::new("needs")
            .about("Center control panel — sessions blocked on input / errors / idle / waiting")
            .arg(
                clap::Arg::new("idle-min")
                    .long("idle-min")
                    .value_parser(clap::value_parser!(i64))
                    .help("Minutes of assistant silence before flagging IDLE (default 5, env AINB_FLEET_IDLE_MIN)"),
            )
            .arg(
                clap::Arg::new("no-enrich")
                    .long("no-enrich")
                    .action(clap::ArgAction::SetTrue)
                    .help("Skip AI enrichment — 0-token HUD (env AINB_FLEET_ENRICH=0)"),
            );
        let enrich_cache = Command::new("enrich-cache")
            .about("Content-addressed enrich cache (the producer's write path)")
            .subcommand_required(true)
            .arg_required_else_help(true)
            .subcommand(
                Command::new("put")
                    .about("Store a drafted suggestion under a card's enrich_key")
                    .arg(clap::Arg::new("key").long("key").required(true))
                    .arg(clap::Arg::new("suggestion").long("suggestion").required(true)),
            )
            .subcommand(
                Command::new("get")
                    .about("Read a cached suggestion by enrich_key (exit non-zero on miss)")
                    .arg(clap::Arg::new("key").long("key").required(true)),
            );
        let daemon = Command::new("daemon")
            .about("Watcher: registers as ainb-fleet-cp peer, auto-continues API errors")
            .arg(
                clap::Arg::new("verbose")
                    .long("verbose")
                    .short('v')
                    .action(clap::ArgAction::SetTrue),
            );
        app.subcommand(
            Command::new(self.name())
                .about("Fleet orchestration: standup / broadcast / sequence / needs / daemon")
                .subcommand_required(true)
                .arg_required_else_help(true)
                .subcommand(standup)
                .subcommand(broadcast)
                .subcommand(sequence)
                .subcommand(needs)
                .subcommand(daemon)
                .subcommand(enrich_cache),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        let matches = matches.clone();
        Box::pin(async move { crate::cli::fleet::execute(&matches, ctx.format).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Command {
        crate::cli::root_clap_command()
    }

    #[test]
    fn built_ins_registers_twenty_three_commands() {
        let r = CommandRegistry::built_ins();
        let names = r.names();
        // 16 user-facing built-ins + doctor + reflect + claudecode namespace
        // + tmux namespace + plugin stub + fleet + hidden notifyd = 23. The
        // TUI is NOT in the registry — main.rs handles `tui` / no-subcommand
        // inline.
        assert_eq!(names.len(), 23, "expected 23 entries, got {names:?}");
        for required in [
            "run",
            "list",
            "logs",
            "attach",
            "status",
            "kill",
            "auth",
            "recover",
            "config",
            "git",
            "favorites",
            "init",
            "doctor",
            "reflect",
            "presets",
            "usage",
            "statusline",
            "claudecode",
            "tmux",
            "completion",
            "plugin",
            "fleet",
            "notifyd",
        ] {
            assert!(
                names.contains(&required),
                "missing required command: {required}"
            );
        }
    }

    #[test]
    fn command_registry_resolves_built_ins() {
        let r = CommandRegistry::built_ins();
        for n in r.names() {
            assert!(r.find(n).is_some(), "find({n}) returned None");
        }
    }

    #[test]
    fn unknown_command_yields_clap_error() {
        let r = CommandRegistry::built_ins();
        let app = r.build_clap(root());
        let err = app.try_get_matches_from(["ainb", "this-command-does-not-exist"]);
        assert!(err.is_err(), "expected clap to reject unknown subcommand");
        let err = err.unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::InvalidSubcommand,
            "wrong clap error kind: {err:?}"
        );
    }

    #[test]
    fn registry_preserves_clap_args_for_run() {
        let r = CommandRegistry::built_ins();
        let app = r.build_clap(root());
        let matches = app
            .try_get_matches_from(["ainb", "run", "--repo", ".", "--worktree"])
            .expect("run subcommand parses with derive-side args");
        let (name, sub) = matches.subcommand().expect("subcommand present");
        assert_eq!(name, "run");
        let args = crate::cli::RunArgs::from_arg_matches(sub).expect("args extract");
        assert!(args.worktree);
        assert_eq!(args.repo.as_deref(), Some(std::path::Path::new(".")));
    }

    #[test]
    fn plugin_subcommand_parses_install_with_flags() {
        // Surface check: the registry-built clap accepts the Phase 4
        // install shape including `--yes`. Behaviour is exercised via
        // tests/plugin_install_flow.rs against an isolated $AINB_HOME.
        let r = CommandRegistry::built_ins();
        let app = r.build_clap(root());
        let matches = app
            .try_get_matches_from(["ainb", "plugin", "install", "burndown", "--yes"])
            .expect("plugin install parses");
        let (top, sub) = matches.subcommand().expect("subcommand");
        assert_eq!(top, "plugin");
        let (sub_name, args) = sub.subcommand().expect("plugin install");
        assert_eq!(sub_name, "install");
        assert_eq!(
            args.get_one::<String>("plugin").map(String::as_str),
            Some("burndown")
        );
        assert!(args.get_flag("yes"));
    }

    #[test]
    fn plugin_subcommand_parses_lint() {
        // Phase 7d-cli surface check: `ainb plugin lint <arg>`.
        let r = CommandRegistry::built_ins();
        let app = r.build_clap(root());
        let matches = app
            .try_get_matches_from(["ainb", "plugin", "lint", "/tmp/some-plugin"])
            .expect("plugin lint parses");
        let (top, sub) = matches.subcommand().expect("subcommand");
        assert_eq!(top, "plugin");
        let (sub_name, args) = sub.subcommand().expect("plugin lint");
        assert_eq!(sub_name, "lint");
        assert_eq!(
            args.get_one::<String>("plugin").map(String::as_str),
            Some("/tmp/some-plugin")
        );
    }

    #[test]
    fn plugin_subcommand_parses_watch_with_duration() {
        // Phase 7d-cli surface check: `ainb plugin watch <id> --duration N`.
        let r = CommandRegistry::built_ins();
        let app = r.build_clap(root());
        let matches = app
            .try_get_matches_from(["ainb", "plugin", "watch", "burndown", "--duration", "5"])
            .expect("plugin watch parses");
        let (top, sub) = matches.subcommand().expect("subcommand");
        assert_eq!(top, "plugin");
        let (sub_name, args) = sub.subcommand().expect("plugin watch");
        assert_eq!(sub_name, "watch");
        assert_eq!(
            args.get_one::<String>("plugin").map(String::as_str),
            Some("burndown")
        );
        assert_eq!(args.get_one::<u64>("duration").copied(), Some(5));
    }

    #[test]
    fn plugin_subcommand_parses_tail_with_level_and_since() {
        // Phase 7d-cli surface check: every flag the dispatcher reads.
        let r = CommandRegistry::built_ins();
        let app = r.build_clap(root());
        let matches = app
            .try_get_matches_from([
                "ainb",
                "plugin",
                "tail",
                "burndown",
                "--level",
                "warn",
                "--since",
                "2026-05-10T20:00:00Z",
                "--duration",
                "10",
            ])
            .expect("plugin tail parses");
        let (top, sub) = matches.subcommand().expect("subcommand");
        assert_eq!(top, "plugin");
        let (sub_name, args) = sub.subcommand().expect("plugin tail");
        assert_eq!(sub_name, "tail");
        assert_eq!(
            args.get_one::<String>("plugin").map(String::as_str),
            Some("burndown")
        );
        assert_eq!(
            args.get_one::<String>("level").map(String::as_str),
            Some("warn")
        );
        assert_eq!(
            args.get_one::<String>("since").map(String::as_str),
            Some("2026-05-10T20:00:00Z")
        );
        assert_eq!(args.get_one::<u64>("duration").copied(), Some(10));
    }

    #[test]
    fn plugin_subcommand_parses_marketplace_add() {
        let r = CommandRegistry::built_ins();
        let app = r.build_clap(root());
        let matches = app
            .try_get_matches_from(["ainb", "plugin", "marketplace", "add", "file:///tmp/m.json"])
            .expect("plugin marketplace add parses");
        let (top, sub) = matches.subcommand().expect("subcommand");
        assert_eq!(top, "plugin");
        let (sub_name, mkt_sub) = sub.subcommand().expect("marketplace");
        assert_eq!(sub_name, "marketplace");
        let (add_name, add_args) = mkt_sub.subcommand().expect("add");
        assert_eq!(add_name, "add");
        assert_eq!(
            add_args.get_one::<String>("url").map(String::as_str),
            Some("file:///tmp/m.json")
        );
    }
}
