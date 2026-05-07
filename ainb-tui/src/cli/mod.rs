// ABOUTME: CLI argument parsing and command routing for ainb
//
// Provides command-line interface for:
// - Spawning AI coding sessions (run)
// - Managing sessions (list, attach, status, kill)
// - Viewing session output (logs)
// - Launching TUI (tui, default)

pub mod attach;
pub mod config_cmd;
pub mod favorites;
pub mod git_cmd;
pub mod init;
pub mod list;
pub mod logs;
pub mod presets;
pub mod recover;
pub mod run;
pub mod status;
pub mod statusline;
pub mod statusline_install;
pub mod usage;
pub mod util;

use clap::{Parser, Subcommand, ValueEnum};
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

/// AI agents in a box - spawn and manage AI coding sessions
#[derive(Parser)]
#[command(name = "ainb")]
#[command(author, version, about, long_about = None)]
#[command(after_help = EXAMPLES)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Output format
    #[arg(long, global = true, default_value = "text")]
    pub format: OutputFormat,
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
#[derive(Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Csv,
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

    /// Recover orphaned or crashed sessions
    Recover {
        #[command(subcommand)]
        command: recover::RecoverCommands,
    },

    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: config_cmd::ConfigCommands,
    },

    /// Git worktree operations
    Git {
        #[command(subcommand)]
        command: git_cmd::GitCommands,
    },

    /// Manage favorite repositories
    Favorites {
        #[command(subcommand)]
        command: favorites::FavoritesCommands,
    },

    /// First-time setup and prerequisite checking
    Init(init::InitArgs),

    /// Manage session presets
    Presets {
        #[command(subcommand)]
        command: presets::PresetsCommands,
    },

    /// Usage analytics, reports, export, and optimization
    Usage {
        #[command(subcommand)]
        command: usage::UsageCommands,
    },

    /// Claude Code-specific commands (statusline, etc.)
    ///
    /// Provider-namespaced commands that only apply to the Claude Code CLI
    /// — e.g. wiring its statusline hook so OAuth-grade rate-limit windows
    /// flow into ainb-tui's Burndown panel.
    Claudecode {
        #[command(subcommand)]
        cmd: ClaudeCodeCmd,
    },

    /// (Legacy alias) Claude Code statusline hook. Prefer
    /// `ainb claudecode statusline`. Kept so existing
    /// `~/.claude/settings.json` entries keep working unchanged; new
    /// installs write the new path and migrate on next `ainb init`.
    #[command(hide = true)]
    Statusline,

    /// Generate shell completions (bash, zsh, fish, powershell, elvish)
    Completion {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

/// Claude Code provider-specific subcommands.
///
/// New surface area for anything tied to the Claude Code CLI specifically
/// (statusline hook today; doctor / config introspection in the future).
/// Kept in a dedicated namespace so that other providers (e.g. Codex) can
/// grow their own equivalents without lying at the top level.
#[derive(clap::Subcommand)]
pub enum ClaudeCodeCmd {
    /// Statusline hook: read JSON on stdin, cache rate-limit windows for
    /// the TUI, and emit a powerline status string on stdout. Wire into
    /// `~/.claude/settings.json` as `statusLine.command`.
    Statusline {
        /// Side-channel mode: write the cache only and emit nothing on
        /// stdout. Use this when chaining ainb behind your own statusline
        /// renderer (Claude Code only allows one `statusLine.command`,
        /// so a custom script can pipe the same JSON to ainb in the
        /// background to keep the TUI Live Window's Tier1 cache fresh).
        /// Errors (malformed JSON, unwritable cache) still surface on
        /// stderr with a non-zero exit so silent breakage is impossible.
        #[arg(long)]
        cache_only: bool,
    },
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Both `ainb statusline` (legacy) and `ainb claudecode statusline`
    /// (new canonical) must parse successfully so existing
    /// `~/.claude/settings.json` entries keep working unchanged while new
    /// installs migrate to the namespaced form.
    #[test]
    fn statusline_legacy_alias_still_parses() {
        let cli = Cli::try_parse_from(["ainb", "statusline"]).expect("legacy alias must parse");
        assert!(matches!(cli.command, Some(Commands::Statusline)));
    }

    #[test]
    fn statusline_new_namespaced_path_parses() {
        let cli = Cli::try_parse_from(["ainb", "claudecode", "statusline"])
            .expect("namespaced path must parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Claudecode {
                cmd: ClaudeCodeCmd::Statusline { cache_only: false }
            })
        ));
    }

    /// `--cache-only` is a side-channel mode for users running their own
    /// statusline renderer who still want ainb's Tier1 cache fresh.
    #[test]
    fn statusline_cache_only_flag_parses() {
        let cli = Cli::try_parse_from(["ainb", "claudecode", "statusline", "--cache-only"])
            .expect("--cache-only must parse");
        assert!(matches!(
            cli.command,
            Some(Commands::Claudecode {
                cmd: ClaudeCodeCmd::Statusline { cache_only: true }
            })
        ));
    }

    /// The namespace is provider-specific; bare `ainb claudecode` without
    /// a subcommand must fail rather than silently doing nothing.
    #[test]
    fn claudecode_without_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["ainb", "claudecode"]).is_err());
    }
}
