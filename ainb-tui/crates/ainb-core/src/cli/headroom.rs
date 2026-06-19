// ABOUTME: `ainb headroom` namespace — proxy lifecycle inspection.
//   status  query the Headroom proxy (running / port / pid / tokens_saved)
//   stop    send SIGTERM to the ainb-managed proxy

use anyhow::Result;
use clap::ArgMatches;

use crate::cli::OutputFormat;
use crate::headroom;

pub async fn execute(matches: &ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("status", _)) => cmd_status(format).await,
        Some(("stop", _)) => cmd_stop(),
        _ => unreachable!("clap subcommand_required"),
    }
}

async fn cmd_status(format: OutputFormat) -> Result<()> {
    let s = headroom::status().await;
    match format {
        OutputFormat::Json => {
            // Emit a minimal JSON object — enough for scripting.
            let tokens_saved = s.tokens_saved.map_or_else(|| "null".to_string(), |n| n.to_string());
            let pid_str = s.pid.map_or_else(|| "null".to_string(), |n| n.to_string());
            println!(
                r#"{{"running":{running},"port":{port},"pid":{pid},"tokens_saved":{tokens_saved}}}"#,
                running = s.running,
                port = s.port,
                pid = pid_str,
            );
        }
        _ => {
            let running_str = if s.running { "running" } else { "stopped" };
            println!("headroom proxy: {running_str}");
            println!("  port:         {}", s.port);
            match s.pid {
                Some(pid) => println!("  pid:          {pid}"),
                None => println!("  pid:          (none)"),
            }
            match s.tokens_saved {
                Some(n) => println!("  tokens_saved: {n}"),
                None => println!("  tokens_saved: (unavailable)"),
            }
        }
    }
    Ok(())
}

fn cmd_stop() -> Result<()> {
    if headroom::stop() {
        println!("headroom proxy stopped");
    } else {
        println!(
            "no ainb-managed headroom proxy is running (a proxy you started yourself is left untouched)"
        );
    }
    Ok(())
}
