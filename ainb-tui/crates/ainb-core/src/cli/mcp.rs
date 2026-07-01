// ABOUTME: `ainb mcp` namespace — shared MCP pool plumbing.
//   daemon  run the pool daemon in the foreground (spawned detached by
//           session creation via mcp_pool::client::ensure_daemon)
//   proxy   stdio shim bridging one Claude MCP slot onto a pool socket
//   status  query the daemon's control socket
//   stop    shut the daemon down (kills pooled children)

use anyhow::Result;
use clap::ArgMatches;
use std::path::PathBuf;

use crate::mcp_pool;

pub async fn execute(matches: &ArgMatches) -> Result<()> {
    match matches.subcommand() {
        Some(("daemon", sub)) => {
            let grace = sub.get_one::<u64>("idle-grace").copied();
            mcp_pool::daemon::execute(grace).await
        }
        Some(("proxy", sub)) => {
            let socket = sub
                .get_one::<String>("socket")
                .map(PathBuf::from)
                .expect("clap enforces <socket>");
            let session = sub.get_one::<String>("session").cloned();
            mcp_pool::shim::execute(socket, session).await
        }
        Some(("status", _)) => {
            if !mcp_pool::client::daemon_alive() {
                println!("{{\"running\":false}}");
                return Ok(());
            }
            println!("{}", mcp_pool::client::daemon_status()?);
            Ok(())
        }
        Some(("stop", sub)) => {
            if !mcp_pool::client::daemon_alive() {
                println!("mcp daemon not running");
                return Ok(());
            }
            match sub.get_one::<String>("server") {
                Some(name) => {
                    mcp_pool::client::stop_server(name)?;
                    println!("stopped server '{name}' (next attach respawns it)");
                }
                None => {
                    mcp_pool::client::daemon_stop()?;
                    println!("mcp daemon stopped");
                }
            }
            Ok(())
        }
        Some(("import", sub)) => {
            let report = mcp_pool::import::execute(sub.get_flag("user"))?;
            if report.imported.is_empty() {
                println!("nothing new to import");
            } else {
                println!(
                    "imported into {}: {}",
                    report.target.display(),
                    report.imported.join(", ")
                );
            }
            if !report.skipped_existing.is_empty() {
                println!(
                    "already configured (untouched): {}",
                    report.skipped_existing.join(", ")
                );
            }
            if !report.skipped_unresolvable.is_empty() {
                println!(
                    "skipped (command not on host): {}",
                    report.skipped_unresolvable.join(", ")
                );
            }
            Ok(())
        }
        Some(("install", sub)) => {
            let codex = sub.get_flag("codex");
            let copilot = sub.get_flag("copilot");
            if !codex && !copilot {
                anyhow::bail!("pass --codex and/or --copilot");
            }
            let config = crate::config::AppConfig::load().unwrap_or_default();
            let mut servers = mcp_pool::pooled_servers(&config);
            // Include project .mcp.json stdio servers, same as session create.
            if let Ok(cwd) = std::env::current_dir() {
                let known: std::collections::HashSet<String> =
                    servers.iter().map(|s| s.name.clone()).collect();
                for s in mcp_pool::mcp_json::parse_stdio_servers(&cwd.join(".mcp.json")) {
                    if !known.contains(&s.name) && s.resolvable_on_host() {
                        servers.push(s);
                    }
                }
            }
            if servers.is_empty() {
                anyhow::bail!(
                    "no poolable servers configured — define [mcp_servers.*] or run `ainb mcp import` first"
                );
            }
            if codex {
                let r = mcp_pool::install::install_codex(&servers)?;
                println!(
                    "codex: wired {} into {}",
                    r.wired.join(", "),
                    r.target.display()
                );
            }
            if copilot {
                let r = mcp_pool::install::install_copilot(&servers)?;
                println!(
                    "copilot: wired {} into {}",
                    r.wired.join(", "),
                    r.target.display()
                );
            }
            println!(
                "note: pool daemon must be running for these sessions (`ainb mcp daemon`, or any ainb Claude session starts it)"
            );
            Ok(())
        }
        _ => unreachable!("clap subcommand_required"),
    }
}
