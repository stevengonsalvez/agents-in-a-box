// ABOUTME: `ainb doctor` — one health report for skills, dependencies, hooks,
// and daemons. Preserves skill-manager checks while adding runtime diagnostics.

use anyhow::{Context, Result};
use serde::Serialize;

use super::OutputFormat;
use crate::cli::deps::{self, RealEnv};

#[derive(Serialize)]
struct DoctorReport<'a> {
    skill_doctor: String,
    skill_doctor_error: Option<String>,
    dependencies: &'a [deps::DepReport],
    hooks: Option<ainb_plugin_notifyd::HookHealth>,
    hooks_error: Option<String>,
    daemons: Vec<crate::fleet::daemons::DaemonStatus>,
    daemons_error: Option<String>,
    daemon_repairs: Vec<String>,
}

/// Full machine health check. `--offline` skips skill-source network probes.
#[derive(clap::Args)]
pub struct DoctorArgs {
    /// Skip skill-source reachability checks. Runtime checks stay local.
    #[arg(long)]
    pub offline: bool,
    /// Repair installed notification hooks: stable binary launcher, extracted
    /// scripts, and agent wiring. Reports a broken dev target without changing
    /// it into a release hook.
    #[arg(long)]
    pub fix_hooks: bool,
    /// Restart Ainb-managed daemon processes proved to be running an older
    /// Ainb release. Unknown or externally-owned processes are only reported.
    #[arg(long)]
    pub fix_daemons: bool,
}

/// Entry point for `ainb doctor`.
#[allow(clippy::unused_async)]
pub async fn execute(args: DoctorArgs, format: OutputFormat) -> Result<()> {
    let dependencies = deps::detect(&RealEnv);
    let (hooks, hooks_error) = match ainb_plugin_notifyd::Paths::from_home() {
        Ok(paths) => {
            if args.fix_hooks {
                if let Err(error) = ainb_plugin_notifyd::auto_repair_hook_binary(&paths) {
                    return Err(error).context("repairing legacy hook binary pointer");
                }
                let health = ainb_plugin_notifyd::hook_health(&paths);
                // A WORKING dev target is intentionally exact: a developer
                // pointing hooks at their own build must keep it. A dev target
                // whose binary is GONE is not a choice, it is a dead pointer,
                // and refusing to repair it was how a deleted worktree left
                // every hook broken with no route back from the CLI.
                let live_dev_target = health.hook_binary_mode
                    == Some(ainb_plugin_notifyd::HookBinaryMode::Dev)
                    && health.hook_binary_ready;
                if !health.issues.is_empty() && !live_dev_target {
                    ainb_plugin_notifyd::repair_hooks(&paths)
                        .context("repairing installed hooks")?;
                }
            }
            (Some(ainb_plugin_notifyd::hook_health(&paths)), None)
        }
        Err(error) => (None, Some(error.to_string())),
    };
    let (mut daemons, daemons_error) = match crate::fleet::daemons::collect() {
        Ok(rows) => (rows, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    let daemon_repairs = if args.fix_daemons {
        let repairs = repair_stale_daemons(&daemons);
        // Refresh status after an attempted repair so text and JSON say what is
        // live now, rather than the pre-restart process.
        if let Ok(rows) = crate::fleet::daemons::collect() {
            daemons = rows;
        }
        repairs
    } else {
        Vec::new()
    };
    match format {
        OutputFormat::Json => {
            let (skill_doctor, skill_doctor_error) = run_skill_doctor(args.offline);
            println!(
                "{}",
                serde_json::to_string_pretty(&DoctorReport {
                    skill_doctor,
                    skill_doctor_error: skill_doctor_error.clone(),
                    dependencies: &dependencies,
                    hooks,
                    hooks_error,
                    daemons,
                    daemons_error,
                    daemon_repairs,
                })?
            );
            if let Some(error) = skill_doctor_error {
                return Err(anyhow::anyhow!(error));
            }
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            deps::print_text(&dependencies);
            print_runtime_text(
                hooks.as_ref(),
                hooks_error.as_deref(),
                &daemons,
                daemons_error.as_deref(),
            );
            for repair in &daemon_repairs {
                println!("daemon repair: {repair}");
            }
            // The skill check can traverse several tool homes. Render the
            // runtime result first so a slow skill scan never hides a dead
            // hook or daemon from the user.
            let (skill_doctor, skill_doctor_error) = run_skill_doctor(args.offline);
            println!("\nSKILL HEALTH");
            println!("------------");
            print!("{skill_doctor}");
            if let Some(error) = skill_doctor_error {
                return Err(anyhow::anyhow!(error));
            }
        }
    }
    Ok(())
}

/// Restart only owner processes with positive old-version evidence. Bridge,
/// fleet watcher, and ATC launch policy belongs to user configuration, so
/// Doctor must never guess their command line or kill them.
fn repair_stale_daemons(daemons: &[crate::fleet::daemons::DaemonStatus]) -> Vec<String> {
    let mut outcomes = Vec::new();
    if daemons.iter().any(|daemon| {
        daemon.kind == crate::fleet::daemons::DaemonKind::Notifyd
            && daemon.version.as_deref().is_some_and(|version| {
                crate::fleet::daemons::probe::release_version_is_older(
                    version,
                    env!("CARGO_PKG_VERSION"),
                )
            })
    }) {
        let outcome = ainb_plugin_notifyd::procs::restart(std::time::Duration::from_secs(3))
            .map(|_| "notifyd restarted against current Ainb".to_string())
            .unwrap_or_else(|error| format!("notifyd restart failed: {error:#}"));
        outcomes.push(outcome);
    }
    let mcp = crate::mcp_pool::client::daemon_runtime_status();
    if mcp.old {
        let outcome = crate::mcp_pool::client::restart_daemon()
            .map(|_| "MCP pool restarted against current Ainb".to_string())
            .unwrap_or_else(|error| format!("MCP pool restart failed: {error:#}"));
        outcomes.push(outcome);
    }
    let hangar = crate::cli::hangar::daemon_runtime_status();
    if hangar.old {
        let outcome = crate::cli::hangar::start_or_upgrade_daemon_from_current(
            crate::cli::hangar::LauncherLifetime::Ephemeral,
        )
        .map(|_| "Hangar restarted against current Ainb".to_string())
        .unwrap_or_else(|error| format!("Hangar restart failed: {error:#}"));
        outcomes.push(outcome);
    }
    if outcomes.is_empty() {
        outcomes.push("no stale managed Ainb daemon found".to_string());
    }
    outcomes
}

fn run_skill_doctor(offline: bool) -> (String, Option<String>) {
    let home = ainb_skill_core::paths::default_ainb_home();
    let mut output = Vec::new();
    let result = ainb_cli::doctor::dispatch(&home, ainb_cli::DoctorArgs { offline }, &mut output);
    (
        String::from_utf8_lossy(&output).into_owned(),
        result.err().map(|error| error.to_string()),
    )
}

fn print_runtime_text(
    hooks: Option<&ainb_plugin_notifyd::HookHealth>,
    hooks_error: Option<&str>,
    daemons: &[crate::fleet::daemons::DaemonStatus],
    daemons_error: Option<&str>,
) {
    println!("\nRUNTIME HEALTH");
    println!("--------------");
    match hooks {
        Some(hooks) => {
            let installed = hooks.installed_version.as_deref().unwrap_or("not installed");
            println!(
                "hooks (ainb-hooks): installed {installed} | bundled {} | {}",
                hooks.bundled_version,
                if hooks.version_current {
                    "current"
                } else {
                    "update needed"
                }
            );
            println!(
                "  script: {}",
                if hooks.script_ready {
                    "ready"
                } else {
                    "BROKEN"
                }
            );
            println!(
                "  hook binary: {}{}",
                if hooks.hook_binary_ready {
                    "ready"
                } else {
                    "BROKEN"
                },
                hooks
                    .hook_binary_mode
                    .map_or_else(String::new, |mode| format!(" ({})", mode.label()))
            );
            if let Some(target) = &hooks.hook_binary {
                println!("    target: {}", target.display());
            }
            println!(
                "  notifyd: {} | approval broker: {}",
                if hooks.notify_socket_live {
                    "running"
                } else {
                    "idle (starts on hook)"
                },
                if hooks.approve_socket_live {
                    "running"
                } else {
                    "idle"
                }
            );
            for agent in &hooks.agents {
                println!(
                    "  {}: {} — {}",
                    agent.agent,
                    if agent.wiring_ready {
                        "wired"
                    } else {
                        "NOT WIRED"
                    },
                    agent.detail
                );
            }
            if let Some(event) = &hooks.last_event {
                println!("  last event: {event}");
            }
            for issue in &hooks.issues {
                println!("  ! {}: {}", issue.component, issue.message);
                println!("    fix: {}", issue.repair);
            }
        }
        None => println!(
            "hooks: cannot inspect — {}",
            hooks_error.unwrap_or("unknown error")
        ),
    }
    match daemons_error {
        Some(error) => println!("\ndaemons: cannot inspect — {error}"),
        None => {
            println!("\ndaemons:");
            print!(
                "{}",
                crate::cli::fleet::daemons::render_text(
                    daemons,
                    crate::fleet::daemons::heartbeat::now_ms()
                )
            );
        }
    }
}
