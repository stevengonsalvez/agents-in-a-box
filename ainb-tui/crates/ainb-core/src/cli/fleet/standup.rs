// ABOUTME: `ainb fleet standup` — list every claude session across ainb + peers + jobs.
//
// Default output: JSON for LLM consumption. `--text` switches to a column table.

use anyhow::Result;

use crate::cli::OutputFormat;
use crate::fleet::discover::{
    discover_from_ainb, discover_from_jobs, discover_from_peers, merge_sessions,
};
use crate::fleet::read::{last_narrative_snapshot, latest_transcript_for_cwd};
use crate::fleet::types::Session;

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let want_text = matches.get_flag("text");

    let (ainb_res, jobs_res) = tokio::join!(discover_from_ainb(), async { discover_from_jobs() });
    let ainb = ainb_res.unwrap_or_default();
    let peers = discover_from_peers().unwrap_or_default();
    let jobs = jobs_res.unwrap_or_default();

    let mut merged = merge_sessions(vec![ainb, peers, jobs]);
    enrich_with_jsonl_summary(&mut merged).await;

    if want_text || matches!(format, OutputFormat::Text) {
        print_text(&merged);
    } else {
        let json = serde_json::to_string_pretty(&merged)?;
        println!("{json}");
    }
    Ok(())
}

/// Replace each session's `summary` with a synthesised one-liner from its
/// JSONL transcript. Reads happen in parallel (spawn_blocking) to stay snappy
/// even on fleets of 30+ sessions. Sessions without a transcript keep
/// summary = None.
async fn enrich_with_jsonl_summary(sessions: &mut [Session]) {
    let cwds: Vec<String> = sessions.iter().map(|s| s.cwd.clone()).collect();
    let mut tasks = Vec::with_capacity(cwds.len());
    for cwd in cwds {
        tasks.push(tokio::task::spawn_blocking(move || {
            let path = latest_transcript_for_cwd(&cwd)?;
            last_narrative_snapshot(&path)
        }));
    }
    for (i, t) in tasks.into_iter().enumerate() {
        if let Ok(Some(summary)) = t.await {
            sessions[i].summary = Some(summary);
        }
    }
}

fn print_text(sessions: &[crate::fleet::types::Session]) {
    println!("ainb fleet standup — {} sessions", sessions.len());
    println!("{:<36}  {:<14}  {}", "name", "sources", "cwd");
    for s in sessions {
        let name = s.tmux_session.as_deref().or(s.workspace_name.as_deref()).unwrap_or(&s.id);
        let sources = s
            .sources
            .iter()
            .map(|src| format!("{src:?}").to_lowercase())
            .collect::<Vec<_>>()
            .join(",");
        let name_display = if name.len() > 34 {
            format!("{}…", &name[..33])
        } else {
            name.to_string()
        };
        println!("{name_display:<36}  {sources:<14}  {}", s.cwd);
    }
}
