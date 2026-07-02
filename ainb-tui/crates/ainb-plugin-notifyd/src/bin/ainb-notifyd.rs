//! `ainb-notifyd` — the binary entry point for the ainb notification
//! daemon and the `install` / `uninstall` / `status` plugin verbs.
//!
//! `ainb-notifyd run` runs the daemon, `ainb-notifyd install
//! --claude --codex` wires up the host agents, `ainb-notifyd status`
//! prints health, `ainb-notifyd stop` signals the running daemon to
//! exit. The hidden `ainb notifyd …` subcommand in `ainb-core` is an
//! alias of this binary — both call the shared command bodies in
//! `ainb_plugin_notifyd::cli`, so behaviour and output never diverge.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};

use ainb_plugin_notifyd::cli;

/// ainb-notifyd: notification daemon + hook installer for
/// Claude Code, Codex CLI, and GitHub Copilot CLI.
#[derive(Debug, Parser)]
#[command(name = "ainb-notifyd", version)]
struct Cli {
    /// Output format for machine-readable verbs (status/reap/restart).
    #[arg(long, global = true, value_enum, default_value_t = Format::Text)]
    format: Format,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// Output rendering for the daemon-control verbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Text,
    Json,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the daemon in the foreground (default if no subcommand).
    Run,
    /// Stop a running daemon by PID file.
    Stop,
    /// Kill orphan / wedged notifyd processes, sparing the live owner.
    Reap,
    /// Stop, reap, and respawn the daemon — the single resume/repair
    /// command for a dead or wedged approve socket.
    Restart,
    /// Install the ainb-hooks plugin for one or more agents.
    Install(AgentArgs),
    /// Uninstall the ainb-hooks plugin for one or more agents.
    Uninstall(AgentArgs),
    /// Report install + daemon status.
    Status,
}

/// Per-agent targeting flags shared by `install` and `uninstall`.
#[derive(Debug, Args)]
struct AgentArgs {
    /// Target Claude Code.
    #[arg(long)]
    claude: bool,
    /// Target Codex CLI.
    #[arg(long)]
    codex: bool,
    /// Target GitHub Copilot CLI.
    #[arg(long)]
    copilot: bool,
    /// Target every known agent (mutually exclusive with per-agent flags).
    #[arg(long)]
    all: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let cli = Cli::parse();
    let json = cli.format == Format::Json;
    let result: Result<()> = match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => cli::cmd_run().await,
        Cmd::Stop => cli::cmd_stop(),
        Cmd::Reap => cli::cmd_reap(json),
        Cmd::Restart => cli::cmd_restart(json),
        Cmd::Install(a) => {
            cli::cmd_install(&cli::agents_from_flags(a.claude, a.codex, a.copilot, a.all))
        }
        Cmd::Uninstall(a) => {
            cli::cmd_uninstall(&cli::agents_from_flags(a.claude, a.codex, a.copilot, a.all))
        }
        Cmd::Status => cli::cmd_status(json),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ainb-notifyd: {e:#}");
            ExitCode::FAILURE
        }
    }
}
