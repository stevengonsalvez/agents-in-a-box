//! `ainb-notifyd` — the binary entry point for the ainb notification
//! daemon and the `install` / `uninstall` / `status` plugin verbs.
//!
//! Until the main `ainb` binary's CLI registry is extended, this is
//! the user-facing surface: `ainb-notifyd run` runs the daemon,
//! `ainb-notifyd install --claude --codex` wires up the host agents,
//! `ainb-notifyd status` prints health, `ainb-notifyd stop` signals
//! the running daemon to exit.

use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tracing::warn;

use ainb_plugin_notifyd::install::{Agent, install as install_for, status, uninstall};
use ainb_plugin_notifyd::pid;
use ainb_plugin_notifyd::{Paths, RetentionPolicy, RunConfig, run_daemon};

/// ainb-notifyd: notification daemon + hook installer for
/// Claude Code and Codex CLI.
#[derive(Debug, Parser)]
#[command(name = "ainb-notifyd", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run the daemon in the foreground (default if no subcommand).
    Run,
    /// Stop a running daemon by PID file.
    Stop,
    /// Install the ainb-hooks plugin for one or more agents.
    Install(InstallArgs),
    /// Uninstall the ainb-hooks plugin for one or more agents.
    Uninstall(UninstallArgs),
    /// Report install + daemon status.
    Status,
}

#[derive(Debug, Args)]
struct InstallArgs {
    /// Install for Claude Code.
    #[arg(long)]
    claude: bool,
    /// Install for Codex CLI.
    #[arg(long)]
    codex: bool,
    /// Install for every known agent. Mutually exclusive with the
    /// per-agent flags.
    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
struct UninstallArgs {
    /// Uninstall for Claude Code.
    #[arg(long)]
    claude: bool,
    /// Uninstall for Codex CLI.
    #[arg(long)]
    codex: bool,
    /// Uninstall for every known agent.
    #[arg(long)]
    all: bool,
}

fn agents_from(claude: bool, codex: bool, all: bool) -> Vec<Agent> {
    if all || (!claude && !codex) {
        return Agent::ALL.to_vec();
    }
    let mut out = Vec::new();
    if claude {
        out.push(Agent::Claude);
    }
    if codex {
        out.push(Agent::Codex);
    }
    out
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
    let result = match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => cmd_run().await,
        Cmd::Stop => cmd_stop(),
        Cmd::Install(args) => cmd_install(args),
        Cmd::Uninstall(args) => cmd_uninstall(args),
        Cmd::Status => cmd_status(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ainb-notifyd: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn cmd_run() -> Result<()> {
    let config = RunConfig::from_home()?;
    run_daemon(config).await
}

fn cmd_stop() -> Result<()> {
    let paths = Paths::from_home()?;
    match pid::read(&paths.pid)? {
        Some(p) if pid::is_running(p) => {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            kill(Pid::from_raw(p as i32), Signal::SIGTERM)
                .with_context(|| format!("sending SIGTERM to pid {p}"))?;
            println!("sent SIGTERM to ainb-notifyd (pid {p})");
        }
        Some(p) => {
            warn!(pid = p, "stale pid file; removing");
            std::fs::remove_file(&paths.pid).ok();
            println!("no live daemon (stale pid {p} cleaned up)");
        }
        None => {
            println!("no daemon running");
        }
    }
    Ok(())
}

fn cmd_install(args: InstallArgs) -> Result<()> {
    let paths = Paths::from_home()?;
    let agents = agents_from(args.claude, args.codex, args.all);
    let record = install_for(&paths, &agents)?;
    println!("installed for: {:?}", record.agents);
    println!("hook script:   {}", record.hook_script.display());
    if let Some(p) = &record.claude_plugin_dir {
        println!("claude plugin: {}", p.display());
    }
    if let Some(p) = &record.codex_hooks_json {
        println!("codex hooks:   {}", p.display());
    }
    Ok(())
}

fn cmd_uninstall(args: UninstallArgs) -> Result<()> {
    let paths = Paths::from_home()?;
    let agents = agents_from(args.claude, args.codex, args.all);
    uninstall(&paths, &agents)?;
    println!("uninstalled: {:?}", agents);
    Ok(())
}

fn cmd_status() -> Result<()> {
    let paths = Paths::from_home()?;
    let rows = status(&paths)?;
    println!(
        "{:<8} {:<10} {:<10} {:<10} {}",
        "agent", "installed", "hook_ok", "socket_ok", "last_event"
    );
    for r in rows {
        println!(
            "{:<8} {:<10} {:<10} {:<10} {}",
            r.agent,
            if r.installed { "yes" } else { "no" },
            if r.hook_script_ok { "yes" } else { "no" },
            if r.socket_ok { "yes" } else { "no" },
            if r.last_event.is_empty() {
                "-".to_string()
            } else {
                r.last_event
            }
        );
    }
    // Also report daemon pid liveness for at-a-glance debugging.
    if let Some(pid) = pid::read(&paths.pid)? {
        if pid::is_running(pid) {
            println!("\ndaemon: pid {pid} (running)");
        } else {
            println!("\ndaemon: stale pid {pid} (not running)");
        }
    } else {
        println!("\ndaemon: not running");
    }
    Ok(())
}
