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

/// Resolve the agent set from the per-agent CLI flags. Empty selection
/// or `--all` means every known agent. The empty-selection guard MUST
/// list every flag — a missing one silently turns a single-agent
/// install into an install-for-everyone.
pub fn agents_from_flags(claude: bool, codex: bool, copilot: bool, all: bool) -> Vec<Agent> {
    if all || (!claude && !codex && !copilot) {
        return Agent::ALL.to_vec();
    }
    let mut out = Vec::new();
    if claude {
        out.push(Agent::Claude);
    }
    if codex {
        out.push(Agent::Codex);
    }
    if copilot {
        out.push(Agent::Copilot);
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

/// `reap` — kill every notifyd process that isn't the healthy live owner
/// (orphans + a wedged stale owner). The live daemon is left running. This
/// is the typed verb behind the Daemons overlay's "to clean up" hint —
/// safer than a hand-typed `kill <pid>` against a possibly-recycled pid.
pub fn cmd_reap(json: bool) -> Result<()> {
    let report = crate::procs::reap();
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    if report.killed.is_empty() && report.failed.is_empty() {
        println!("no orphan notifyd processes to reap");
    } else {
        for pid in &report.killed {
            println!("reaped pid {pid}");
        }
        for (pid, why) in &report.failed {
            println!("could not reap pid {pid}: {why}");
        }
        println!(
            "reaped {} orphan(s){}",
            report.killed.len(),
            if report.failed.is_empty() {
                String::new()
            } else {
                format!(", {} failed", report.failed.len())
            }
        );
    }
    match report.spared {
        Some(p) => println!("left live daemon running (pid {p})"),
        None => println!("no live daemon remains — next hook event will lazy-spawn one"),
    }
    Ok(())
}

/// `restart` — the single resume/repair command. Stop the current owner,
/// reap stragglers, spawn a fresh daemon, and wait for the approve socket
/// to rebind. Because [`crate::broker::client_await`] re-dials until its
/// own deadline, every still-blocked permission waiter re-registers the
/// moment the socket is back — so this one command both repairs a dead
/// socket and resumes pending prompts, without losing a waiting hook.
pub fn cmd_restart(json: bool) -> Result<()> {
    let outcome = crate::procs::restart(std::time::Duration::from_secs(3))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
        return Ok(());
    }
    match outcome.stopped {
        Some(p) => println!("stopped previous daemon (pid {p})"),
        None => println!("no previous daemon was running"),
    }
    if !outcome.reaped.is_empty() {
        println!("reaped {} straggler(s)", outcome.reaped.len());
    }
    match outcome.spawned {
        Some(p) => println!("spawned fresh daemon (pid {p})"),
        None => println!("failed to spawn daemon"),
    }
    if outcome.socket_bound {
        println!("approve socket is live — pending permission prompts will resume");
    } else {
        println!(
            "approve socket did not rebind in time; still-waiting hooks keep re-dialling \
             until it does or they time out"
        );
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
    if let Some(p) = &record.copilot_hooks_json {
        println!("copilot hooks: {}", p.display());
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

/// `list` — print persisted notifications (most recent first) straight from
/// the SQLite store.
// ponytail: DB read only — a live daemon's not-yet-persisted in-memory events
// won't appear. Add a daemon control-socket query if that ever matters.
pub fn cmd_list(
    include_dismissed: bool,
    agent: Option<&str>,
    project: Option<&str>,
    limit: u32,
    json: bool,
) -> Result<()> {
    let paths = Paths::from_home()?;
    let store = crate::store::Store::open(&paths.db).context("open notifications store")?;
    let rows = store.list(include_dismissed, agent, project, limit)?;
    if json {
        let arr: Vec<_> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id,
                    "ts": r.ts,
                    "agent": r.agent,
                    "session_id": r.session_id,
                    "cwd": r.cwd,
                    "project": r.project,
                    "raw_event": r.raw_event,
                    "read": r.read,
                    "dismissed": r.dismissed,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else if rows.is_empty() {
        println!("no notifications");
    } else {
        println!(
            "{:<14} {:<8} {:<22} {}",
            "ts(ms)", "agent", "project", "event"
        );
        for r in &rows {
            println!(
                "{:<14} {:<8} {:<22} {}",
                r.ts, r.agent, r.project, r.raw_event
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_from_flags_defaults_to_all() {
        // No flags → all; `--all` (with or without per-agent flags) → all.
        assert_eq!(
            agents_from_flags(false, false, false, false),
            Agent::ALL.to_vec()
        );
        assert_eq!(
            agents_from_flags(true, true, true, true),
            Agent::ALL.to_vec()
        );
    }

    #[test]
    fn agents_from_flags_respects_single_selection() {
        assert_eq!(
            agents_from_flags(true, false, false, false),
            vec![Agent::Claude]
        );
        assert_eq!(
            agents_from_flags(false, true, false, false),
            vec![Agent::Codex]
        );
        // Regression guard: `--copilot` alone must NOT fall through the
        // empty-selection branch and install for everyone.
        assert_eq!(
            agents_from_flags(false, false, true, false),
            vec![Agent::Copilot]
        );
    }

    #[test]
    fn agents_from_flags_both_explicit() {
        assert_eq!(
            agents_from_flags(true, true, false, false),
            vec![Agent::Claude, Agent::Codex]
        );
        assert_eq!(
            agents_from_flags(true, true, true, false),
            vec![Agent::Claude, Agent::Codex, Agent::Copilot]
        );
    }
}
