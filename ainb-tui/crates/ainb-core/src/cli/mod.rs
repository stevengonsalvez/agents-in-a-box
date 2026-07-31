// ABOUTME: CLI argument parsing and command routing for ainb
//
// Provides command-line interface for:
// - Spawning AI coding sessions (run)
// - Managing sessions (list, attach, status, kill)
// - Viewing session output (logs)
// - Launching TUI (tui, default)

pub mod attach;
pub mod auth;
pub mod codex_statusline;
pub mod config_cmd;
pub mod deps;
pub mod diff_review;
pub mod doctor;
pub mod favorites;
pub mod fleet;
pub mod git_cmd;
pub mod hangar;
pub mod headroom;
pub mod init;
pub mod list;
pub mod logs;
pub mod mcp;
pub mod otel;
pub mod plugin;
pub mod presets;
pub mod recover;
pub mod reflect;
pub mod registry;
pub mod rtk;
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
  ainb                                Launch the TUI
  ainb run --repo . --worktree        Start an isolated session in the current repo
  ainb run --worktree --tool codex    Use Codex instead of Claude
  ainb list --format json             List all sessions as JSON
  ainb status my-project              Inspect a session by workspace name
  ainb attach my-project              Drop into a running session
  ainb config get authentication.default_model
  ainb recover list                   Find orphaned sessions
  ainb completion zsh > _ainb         Generate zsh completions

SKILL MANAGER:
  ainb skill browse <query>        Search the skill catalog (skills.sh)
  ainb skill install <uri>         Install a skill/agent/command unit
  ainb skill sync                  Reconcile on-disk units with the manifest
  ainb skill remove <uri>          Uninstall a unit (per-file, never wipes config)
  ainb source / ainb search        (skill-manager — run with --help)";

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
        // `-V` shows the bare semver; `--version` shows build identity
        // (commit + date + origin) stamped by build.rs so a binary is traceable
        // to a commit — the plain number is stale between releases.
        .version(env!("CARGO_PKG_VERSION"))
        .long_version(env!("AINB_VERSION_LONG"))
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

// The user-facing command `about` is set in `cli/registry.rs` (`.about()`
// applied after `augment_args` so it wins). Keep the doc-comment below a
// SINGLE line — a multi-line doc-comment also becomes clap `long_about` and
// would leak this note into `ainb run --help`.
/// Spawn a new AI coding session.
#[derive(clap::Args)]
#[command(after_help = "\
EXAMPLES:
  ainb run --repo . --worktree                       Isolated worktree (recommended)
  ainb run --repo . --create-branch feat/new         Isolated worktree on a named branch
  ainb run --repo . --worktree -p \"fix the tests\"    Spawn with an initial prompt
  ainb run --repo . --worktree --parent tower        Route completions to an orchestrator
  ainb run --remote-repo owner/repo --worktree       Clone a GitHub repo first, then isolate
  ainb run --repo . --worktree --tool codex          Use Codex instead of Claude
  ainb run --repo . --worktree --attach              Drop into tmux after creating
  ainb run --repo .                                  Shared checkout, NO isolation

Without --worktree (or --create-branch) the session runs directly in the
checkout you point at: it shares that branch, index and working tree with your
editor and with every other session started there. Prefer --worktree.")]
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

    /// Provider model ID to pass through unchanged
    #[arg(long)]
    pub model: Option<String>,

    /// Initial prompt to send
    #[arg(long, short)]
    pub prompt: Option<String>,

    /// Attach to session after creation
    #[arg(long, short)]
    pub attach: bool,

    /// Skip permission prompts (dangerous!)
    #[arg(long)]
    pub dangerously_skip_permissions: bool,

    /// Custom tmux session name (NOT an attach/status/kill handle)
    //
    // Keep the doc-comment above a SINGLE line: clap turns a multi-line
    // doc-comment into `long_about`, which would then dominate `--help`.
    //
    // This only renames the tmux session. `ainb attach|status|kill` resolve
    // their argument as a session id, an id prefix, or a *workspace* name
    // (derived from the repository directory in `run.rs`), never from this
    // flag. See the "SPAWNING SESSIONS FROM AN AGENT" section of ainb(1).
    #[arg(long)]
    pub name: Option<String>,

    /// Run in interactive mode (spawn tmux and attach)
    #[arg(long, short)]
    pub interactive: bool,

    /// Parent session id — links this session to an orchestrator (e.g. ATC) so
    /// its completions route to the parent's durable inbox (event-driven
    /// plumbing). Exported into the session as `AINB_PARENT_SESSION`.
    #[arg(long)]
    pub parent: Option<String>,
}

/// List sessions (running + idle). Description set in `cli/registry.rs`.
#[derive(clap::Args)]
pub struct ListArgs {
    /// Show only running sessions
    #[arg(long)]
    pub running: bool,

    /// Show only sessions for a specific workspace
    #[arg(long)]
    pub workspace: Option<String>,
}

/// View session output/logs. Description set in `cli/registry.rs`.
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

/// Attach to a running session. Description set in `cli/registry.rs`.
#[derive(clap::Args)]
pub struct AttachArgs {
    /// Session ID or name
    pub session: String,
}

/// Show a session's status/health. Description set in `cli/registry.rs`.
#[derive(clap::Args)]
pub struct StatusArgs {
    /// Session ID or name
    pub session: String,
}

/// Terminate a session. Description set in `cli/registry.rs`.
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
