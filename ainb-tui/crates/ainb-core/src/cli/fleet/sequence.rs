// ABOUTME: `ainb fleet sequence` — ordered ack-gated multi-step prompts.
//
// Between steps, watches each target's JSONL transcript for the next
// assistant-turn-end and waits until either all targets ack or the timeout
// fires. Falls back to a small fixed delay if the transcript isn't found.

use std::time::Duration;

use anyhow::{Result, bail};

use crate::cli::OutputFormat;
use crate::fleet::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
use crate::fleet::read::{latest_transcript_for_cwd, wait_for_turn_end};
use crate::fleet::send::send;
use crate::fleet::types::Session;

pub async fn execute(matches: &clap::ArgMatches, _format: OutputFormat) -> Result<()> {
    let steps: Vec<String> = matches
        .get_many::<String>("steps")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();
    if steps.is_empty() {
        bail!("sequence requires at least one step");
    }

    let all = matches.get_flag("all");
    if !all {
        bail!("sequence v0.1 only supports --all targeting");
    }

    let timeout_s = matches.get_one::<u64>("timeout").copied().unwrap_or(300);

    let (ainb, peers) = tokio::join!(discover_from_ainb(), async { discover_from_peers() });
    let targets = merge_sessions(vec![ainb.unwrap_or_default(), peers.unwrap_or_default()]);

    let n = steps.len();
    for (idx, step) in steps.iter().enumerate() {
        eprintln!("[fleet/sequence] step {}/{}: {step}", idx + 1, n);
        for t in &targets {
            let _ = send(t, step).await?;
        }
        if idx < steps.len() - 1 {
            wait_for_all_acks(&targets, Duration::from_secs(timeout_s)).await;
        }
    }
    Ok(())
}

/// For each target, spawn a blocking task watching its latest JSONL for
/// `stop_reason == "end_turn"`. Returns when all observed acks or the deadline
/// hits. Targets whose transcript can't be found are skipped (best-effort).
async fn wait_for_all_acks(targets: &[Session], timeout: Duration) {
    let mut handles = Vec::with_capacity(targets.len());
    for t in targets {
        let cwd = t.cwd.clone();
        let h = tokio::task::spawn_blocking(move || {
            let Some(path) = latest_transcript_for_cwd(&cwd) else {
                return false;
            };
            wait_for_turn_end(&path, timeout).unwrap_or(false)
        });
        handles.push(h);
    }
    for h in handles {
        let _ = h.await;
    }
}
