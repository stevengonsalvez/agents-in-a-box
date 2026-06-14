// ABOUTME: `ainb fleet` subcommand dispatcher.
//
// Mirrors the pattern used by `cli/plugin/mod.rs` — nested clap subcommands
// matched by `matches.subcommand()`. Each subcommand has its own module with
// an `execute(args, format) -> Result<()>` function.

use anyhow::{Result, bail};

use crate::cli::OutputFormat;

pub mod broadcast;
pub mod budget_alert;
pub mod cost;
pub mod daemon;
pub mod enrich_cache;
pub mod needs;
pub mod sequence;
pub mod standup;

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("standup", sub)) => standup::execute(sub, format).await,
        Some(("broadcast", sub)) => broadcast::execute(sub, format).await,
        Some(("sequence", sub)) => sequence::execute(sub, format).await,
        Some(("needs", sub)) => needs::execute(sub, format).await,
        Some(("cost", sub)) => cost::execute(sub, format).await,
        Some(("daemon", sub)) => daemon::execute(sub, format).await,
        Some(("enrich-cache", sub)) => enrich_cache::execute(sub, format).await,
        _ => bail!("unknown `ainb fleet` subcommand — try `ainb fleet --help`"),
    }
}
