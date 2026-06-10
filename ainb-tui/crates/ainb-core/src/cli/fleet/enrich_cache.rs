// ABOUTME: `ainb fleet enrich-cache` — the single atomic write path for the
// content-addressed enrich cache.
//
// The enrich producer (the workflow batched agent, or the calling session for
// small fleets) calls `put --key <enrich_key> --suggestion <text>` to persist a
// drafted suggestion. `<enrich_key>` is the value the `needs`/`standup` reader
// already emitted on each card, so reader and producer never disagree. Keeping
// the cache format in one place (Rust) avoids JS/Bash reimplementations
// drifting apart or racing into corruption.

use anyhow::{Result, bail};

use crate::cli::OutputFormat;
use crate::fleet::enrich_cache;

pub async fn execute(matches: &clap::ArgMatches, _format: OutputFormat) -> Result<()> {
    match matches.subcommand() {
        Some(("put", sub)) => {
            let key = sub
                .get_one::<String>("key")
                .ok_or_else(|| anyhow::anyhow!("missing --key"))?;
            let suggestion = sub
                .get_one::<String>("suggestion")
                .ok_or_else(|| anyhow::anyhow!("missing --suggestion"))?;
            enrich_cache::put(key, suggestion)?;
            println!("ok");
            Ok(())
        }
        Some(("get", sub)) => {
            let key = sub
                .get_one::<String>("key")
                .ok_or_else(|| anyhow::anyhow!("missing --key"))?;
            match enrich_cache::lookup(key) {
                Some(s) => {
                    println!("{s}");
                    Ok(())
                }
                None => bail!("miss"),
            }
        }
        _ => bail!("usage: ainb fleet enrich-cache <put|get> --key <k> [--suggestion <s>]"),
    }
}
