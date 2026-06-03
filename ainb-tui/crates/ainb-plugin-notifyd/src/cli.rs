//! Shared CLI command bodies for the notification daemon.
//!
//! Both entrypoints — the standalone `ainb-notifyd` binary and the
//! hidden `ainb notifyd` subcommand in `ainb-core` — call into these
//! functions so the two surfaces never diverge in behaviour or output.
//! The verb-parsing layer (clap derive in the binary, clap builder in
//! the registry) lives in each entrypoint; everything *after* "which
//! verb + which agents" is here.

use anyhow::{Context, Result};
use tracing::warn;

use crate::install::{Agent, ClaudeRegister};
use crate::{Paths, RunConfig, install_for, run_daemon, status, uninstall};

/// Resolve the agent set from the three CLI flags. Empty selection or
/// `--all` means every known agent.
pub fn agents_from_flags(claude: bool, codex: bool, all: bool) -> Vec<Agent> {
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

/// `run` — bind the socket and serve until SIGTERM. Async because the
/// accept loop is.
pub async fn cmd_run() -> Result<()> {
    let config = RunConfig::from_home()?;
    run_daemon(config).await
}

/// `stop` — SIGTERM a running daemon via its PID file; clean up a
/// stale PID file if the process is gone.
pub fn cmd_stop() -> Result<()> {
    let paths = Paths::from_home()?;
    match crate::pid::read(&paths.pid)? {
        Some(p) if crate::pid::is_running(p) => {
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

/// `install` — wire the ainb-hooks hook into the chosen agents and
/// print the resolved on-disk paths.
pub fn cmd_install(agents: &[Agent]) -> Result<()> {
    let paths = Paths::from_home()?;
    let report = install_for(&paths, agents)?;
    let record = &report.record;
    println!("installed for: {:?}", record.agents);
    println!("hook script:   {}", record.hook_script.display());
    if let Some(p) = &record.codex_hooks_json {
        println!("codex hooks:   {}", p.display());
    }
    match &report.claude {
        Some(ClaudeRegister::Registered) => println!(
            "claude plugin: registered (ainb-hooks@agents-in-a-box) — restart Claude to load it"
        ),
        Some(ClaudeRegister::ClaudeCliMissing) => {
            println!("claude plugin: SKIPPED — `claude` CLI not found on PATH")
        }
        Some(ClaudeRegister::Failed(e)) => println!("claude plugin: FAILED — {e}"),
        None => {}
    }
    Ok(())
}

/// `uninstall` — reverse the install for the chosen agents (preserves
/// user-authored Codex hooks).
pub fn cmd_uninstall(agents: &[Agent]) -> Result<()> {
    let paths = Paths::from_home()?;
    uninstall(&paths, agents)?;
    println!("uninstalled: {agents:?}");
    Ok(())
}

/// `status` — per-agent install/hook/socket state + the most recent
/// event + daemon PID liveness.
pub fn cmd_status() -> Result<()> {
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
    // Daemon PID liveness, for at-a-glance debugging.
    if let Some(pid) = crate::pid::read(&paths.pid)? {
        if crate::pid::is_running(pid) {
            println!("\ndaemon: pid {pid} (running)");
        } else {
            println!("\ndaemon: stale pid {pid} (not running)");
        }
    } else {
        println!("\ndaemon: not running");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_from_flags_defaults_to_all() {
        assert_eq!(agents_from_flags(false, false, false), Agent::ALL.to_vec());
        assert_eq!(agents_from_flags(true, true, true), Agent::ALL.to_vec());
    }

    #[test]
    fn agents_from_flags_respects_single_selection() {
        assert_eq!(agents_from_flags(true, false, false), vec![Agent::Claude]);
        assert_eq!(agents_from_flags(false, true, false), vec![Agent::Codex]);
    }

    #[test]
    fn agents_from_flags_both_explicit() {
        assert_eq!(
            agents_from_flags(true, true, false),
            vec![Agent::Claude, Agent::Codex]
        );
    }
}
