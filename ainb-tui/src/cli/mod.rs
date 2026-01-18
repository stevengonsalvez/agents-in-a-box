// ABOUTME: CLI argument parsing and command routing for ainb
//
// Provides command-line interface for:
// - Spawning AI coding sessions (run)
// - Managing sessions (list, attach, status, kill)
// - Viewing session output (logs)
// - Launching TUI (tui, default)

pub mod run;
pub mod list;
pub mod logs;
pub mod attach;
pub mod status;

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// AI agents in a box - spawn and manage AI coding sessions
#[derive(Parser)]
#[command(name = "ainb")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Output format
    #[arg(long, global = true, default_value = "text")]
    pub format: OutputFormat,
}

/// Output format for commands
#[derive(Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

/// Available CLI commands
#[derive(Subcommand)]
pub enum Commands {
    /// Launch the TUI (default if no command given)
    Tui,

    /// Spawn a new AI coding session
    Run(RunArgs),

    /// List all sessions
    List(ListArgs),

    /// View session output/logs
    Logs(LogsArgs),

    /// Attach to a session (drops into tmux)
    Attach(AttachArgs),

    /// Check session status
    Status(StatusArgs),

    /// Kill a session
    Kill(KillArgs),

    /// Set up authentication
    Auth,
}

/// Arguments for the run command
#[derive(clap::Args)]
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

    /// AI tool to use (claude, codex, gemini)
    #[arg(long, default_value = "claude")]
    pub tool: String,

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
