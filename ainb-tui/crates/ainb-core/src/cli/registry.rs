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
        r.register(PresetsCommand);
        r.register(UsageCommand);
        r.register(StatuslineCommand);
        r.register(ClaudecodeCommand);
        r.register(CompletionCommand);
        r.register(PluginCommand); // Phase 4 stub — surface reserved now
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
        let argv: Vec<String> = std::env::args()
            .skip_while(|a| a != "usage")
            .skip(1)
            .collect();
        let format = ctx.format;
        let parsed = crate::cli::usage::UsageCommands::from_arg_matches(matches);
        Box::pin(async move {
            let cmd = parsed.map_err(anyhow::Error::from)?;
            if is_host_admin_subcommand(&cmd) {
                return crate::cli::usage::execute(cmd, format).await;
            }
            dispatch_usage_via_plugin(&argv);
            // dispatch_usage_via_plugin always exits the process on
            // both happy and failure paths (process::exit) — return
            // unreachable Ok to satisfy the type. Matches the
            // exit-2 contract spelled out in
            // plans/plugin-phase-6-data-plane.md.
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

/// `ainb usage` shim.
///
/// Phase 6 wired this through the wasmi-based burndown plugin with a
/// cross-plugin broker hack (`inject_session_reader_snapshot`) to side-step
/// the wasmi single-threaded deadlock. Phase 7b deletes the wasmi host
/// entirely; the subprocess-Rust replacement plugins (burndown +
/// session-reader) are tracked under Phase 7c. Until those land, the
/// burndown shim returns the operator-actionable error every existing
/// caller already handled (exit 2, "install via 'ainb plugin install').
fn dispatch_usage_via_plugin(_argv: &[String]) -> ! {
    eprintln!(
        "error: usage analytics requires the burndown plugin \
         (install via 'ainb plugin install burndown') — \
         subprocess port pending under Phase 7c"
    );
    std::process::exit(2);
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
            Some(("statusline", m)) => (
                Some("statusline".to_string()),
                m.get_flag("cache-only"),
            ),
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
        app.subcommand(
            Command::new(self.name())
                .about("Manage ainb plugins")
                .subcommand_required(true)
                .subcommand(install)
                .subcommand(update)
                .subcommand(remove_cmd)
                .subcommand(list)
                .subcommand(search)
                .subcommand(marketplace),
        )
    }
    fn run(&self, matches: &ArgMatches, ctx: CliContext) -> BoxFuture<'static, Result<()>> {
        let matches = matches.clone();
        Box::pin(async move { crate::cli::plugin::execute(&matches, ctx.format).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Command {
        crate::cli::root_clap_command()
    }

    #[test]
    fn built_ins_registers_eighteen_commands() {
        let r = CommandRegistry::built_ins();
        let names = r.names();
        // 16 user-facing built-ins + claudecode namespace + plugin stub = 18.
        // The TUI is NOT in the registry — main.rs handles `tui` /
        // no-subcommand inline.
        assert_eq!(names.len(), 18, "expected 18 entries, got {names:?}");
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
            "presets",
            "usage",
            "statusline",
            "claudecode",
            "completion",
            "plugin",
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
        assert_eq!(args.get_one::<String>("plugin").map(String::as_str), Some("burndown"));
        assert!(args.get_flag("yes"));
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
