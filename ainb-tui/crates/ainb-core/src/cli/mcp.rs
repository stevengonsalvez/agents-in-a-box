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
            mcp_pool::shim::execute(socket).await
        }
        Some(("status", _)) => {
            if !mcp_pool::client::daemon_alive() {
                println!("{{\"running\":false}}");
                return Ok(());
            }
            println!("{}", mcp_pool::client::daemon_status()?);
            Ok(())
        }
        Some(("stop", _)) => {
            if !mcp_pool::client::daemon_alive() {
                println!("mcp daemon not running");
                return Ok(());
            }
            mcp_pool::client::daemon_stop()?;
            println!("mcp daemon stopped");
            Ok(())
        }
        _ => unreachable!("clap subcommand_required"),
    }
}
