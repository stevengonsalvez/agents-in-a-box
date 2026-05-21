// ABOUTME: CLI argument parsing and command routing for ainb
//
// Provides command-line interface for:
// - Spawning AI coding sessions (run)
// - Managing sessions (list, attach, status, kill)
// - Viewing session output (logs)
// - Launching TUI (tui, default)

pub mod attach;
pub mod auth;
pub mod config_cmd;
pub mod favorites;
pub mod fleet;
pub mod git_cmd;
pub mod init;
pub mod list;
pub mod logs;
pub mod plugin;
pub mod presets;
pub mod recover;
pub mod registry;
pub mod run;
pub mod status;
pub mod statusline;
pub mod statusline_install;
pub mod tmux_install;
pub mod usage;
pub mod util;

use clap::{Command, ValueEnum};
use std::path::PathBuf;

const EXAMPLES: &str = "\
EXAMPLES:
  ainb                            Launch the TUI
  ainb run --repo . --worktree    Start a session in the current repo
  ainb run --tool codex --repo .  Use Codex instead of Claude
  ainb list --format json         List all sessions as JSON
  ainb status my-project          Inspect a session by workspace name
  ainb attach my-project          Drop into a running session
  ainb config get authentication.default_model
  ainb recover list               Find orphaned sessions
  ainb completion zsh > _ainb     Generate zsh completions";

/// Build the root `clap::Command` for the `ainb` binary.
///
/// Used by both `main.rs` (real dispatch) and `registry.rs` (tests + the
/// `completion` subcommand, which needs the full surface to generate shell
/// completions). Keeps the `--format` global arg + `EXAMPLES` after-help in
/// one place; subcommands are added on top via `CommandRegistry::build_clap`.
#[must_use]
pub fn root_clap_command() -> Command {
    Command::new("ainb")
        .author(env!("CARGO_PKG_AUTHORS"))
        .version(env!("CARGO_PKG_VERSION"))
        .about("AI agents in a box - spawn and manage AI coding sessions")
        .after_help(EXAMPLES)
        .arg(
            clap::Arg::new("format")
                .long("format")
                .global(true)
                .value_parser(clap::builder::EnumValueParser::<OutputFormat>::new())
                .default_value("text")
                .help("Output format"),
        )
}

/// AI CLI provider to use for a session
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum Tool {
    #[default]
    Claude,
    Codex,
    Gemini,
    Copilot,
}

impl Tool {
    /// Convert to the internal CliProvider type
    pub fn to_cli_provider(self) -> crate::config::CliProvider {
        match self {
            Tool::Claude => crate::config::CliProvider::Claude,
            Tool::Codex => crate::config::CliProvider::Codex,
            Tool::Gemini => crate::config::CliProvider::Gemini,
            Tool::Copilot => crate::config::CliProvider::Copilot,
        }
    }
}

/// Output format for commands
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Csv,
    Markdown,
}

/// Arguments for the run command
#[derive(clap::Args)]
#[command(after_help = "\
EXAMPLES:
  ainb run --repo .                                 Use current directory
  ainb run --repo . --worktree                      Isolate in a new worktree
  ainb run --repo . --create-branch feat/new        Create a branch + worktree
  ainb run --remote-repo owner/repo                 Clone GitHub repo first
  ainb run --tool codex --repo .                    Use Codex instead of Claude
  ainb run --repo . -p \"fix the failing tests\"    Send an initial prompt
  ainb run --repo . --attach                        Drop into tmux after creating")]
pub struct RunArgs {
    /// Remote repository (e.g., username/repo or full URL)
    #[arg(long)]
    pub remote_repo: Option<String>,

    /// Local repository path
    #[arg(long)]
    pub repo: Option<PathBuf>,

    /// Create a new branch with this name
    #[arg(long)]
    pub create_branch: Option<String>,

    /// Use git worktree for isolation
    #[arg(long)]
    pub worktree: bool,

    /// AI tool to use
    #[arg(long, value_enum, default_value_t = Tool::Claude)]
    pub tool: Tool,

    /// Model to use (sonnet, opus, haiku)
    #[arg(long, default_value = "sonnet")]
    pub model: String,

    /// Initial prompt to send
    #[arg(long, short)]
    pub prompt: Option<String>,

    /// Attach to session after creation
    #[arg(long, short)]
    pub attach: bool,

    /// Skip permission prompts (dangerous!)
    #[arg(long)]
    pub dangerously_skip_permissions: bool,

    /// Custom session name
    #[arg(long)]
    pub name: Option<String>,

    /// Run in interactive mode (spawn tmux and attach)
    #[arg(long, short)]
    pub interactive: bool,
}

/// Arguments for the list command
#[derive(clap::Args)]
pub struct ListArgs {
    /// Show only running sessions
    #[arg(long)]
    pub running: bool,

    /// Show only sessions for a specific workspace
    #[arg(long)]
    pub workspace: Option<String>,
}

/// Arguments for the logs command
#[derive(clap::Args)]
pub struct LogsArgs {
    /// Session ID or name
    pub session: String,

    /// Follow log output (like tail -f)
    #[arg(long, short)]
    pub follow: bool,

    /// Number of lines to show
    #[arg(long, short, default_value = "100")]
    pub lines: usize,
}

/// Arguments for the attach command
#[derive(clap::Args)]
pub struct AttachArgs {
    /// Session ID or name
    pub session: String,
}

/// Arguments for the status command
#[derive(clap::Args)]
pub struct StatusArgs {
    /// Session ID or name
    pub session: String,
}

/// Arguments for the kill command
#[derive(clap::Args)]
pub struct KillArgs {
    /// Session ID or name
    pub session: String,

    /// Force kill without confirmation
    #[arg(long, short)]
    pub force: bool,
}

// Statusline / claudecode subcommand parse tests removed. The Commands
// enum they exercised no longer exists (registry pattern owns the
// surface); equivalent coverage lives in `cli/registry.rs` integration
// tests against the assembled `clap::Command`.
