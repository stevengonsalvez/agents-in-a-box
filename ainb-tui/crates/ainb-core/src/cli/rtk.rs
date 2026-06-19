// ABOUTME: `ainb rtk` namespace — RTK (Rust Token Killer) lifecycle management.
//   status     detect install + wiring state + total tokens saved
//   install    brew install rtk + rtk init -g [--codex]
//   uninstall  rtk init -g --uninstall (removes hook; leaves binary)

use anyhow::Result;
use clap::ArgMatches;

use crate::cli::OutputFormat;
use crate::rtk;

pub async fn execute(matches: &ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("status", _)) => cmd_status(format),
        Some(("install", m)) => cmd_install(m.get_flag("codex")),
        Some(("uninstall", _)) => cmd_uninstall(),
        _ => unreachable!("clap subcommand_required"),
    }
}

fn cmd_status(format: OutputFormat) -> Result<()> {
    let s = rtk::status();
    match format {
        OutputFormat::Json => {
            let total_saved = s.total_saved.map_or_else(|| "null".to_string(), |n| n.to_string());
            println!(
                r#"{{"installed":{installed},"wired":{wired},"total_saved":{total_saved}}}"#,
                installed = s.installed,
                wired = s.wired,
            );
        }
        _ => {
            let installed_str = if s.installed { "yes" } else { "no" };
            let wired_str = if s.wired {
                "yes"
            } else {
                "no (run `ainb rtk install`)"
            };
            println!("rtk installed:  {installed_str}");
            println!("hook wired:     {wired_str}");
            match s.total_saved {
                Some(n) => println!("tokens saved:   {n}"),
                None => println!("tokens saved:   (unavailable)"),
            }
        }
    }
    Ok(())
}

fn cmd_install(also_codex: bool) -> Result<()> {
    rtk::install()?;
    println!("rtk installed and Claude Code hook wired.");
    if also_codex {
        rtk::install_codex()?;
        println!("Codex AGENTS.md prompt injection wired (best-effort).");
    }
    println!("Verify with: ainb rtk status");
    Ok(())
}

fn cmd_uninstall() -> Result<()> {
    rtk::uninstall()?;
    println!("RTK hook removed from ~/.claude/settings.json.");
    println!("The rtk binary is still installed; remove with: brew uninstall rtk");
    Ok(())
}
