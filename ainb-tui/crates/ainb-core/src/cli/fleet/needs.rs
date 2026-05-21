// ABOUTME: `ainb fleet needs` — list sessions blocked on input / errors / waiting.

use anyhow::Result;

use crate::cli::OutputFormat;
use crate::fleet::discover::{discover_from_ainb, discover_from_peers, merge_sessions};

pub async fn execute(_matches: &clap::ArgMatches, _format: OutputFormat) -> Result<()> {
    let (ainb, peers) = tokio::join!(discover_from_ainb(), async { discover_from_peers() });
    let sessions = merge_sessions(vec![
        ainb.unwrap_or_default(),
        peers.unwrap_or_default(),
    ]);

    let blocked: Vec<_> = sessions
        .into_iter()
        .filter(|s| s.summary.as_deref().is_some_and(|sum| sum.starts_with("WAITING:")))
        .collect();

    let json = serde_json::to_string_pretty(&blocked)?;
    println!("{json}");
    Ok(())
}
